//! Comandos Tauri chamados pela janela de definições (`invoke` em `src/api.ts`).
//!
//! Regras:
//! - A chave da API nunca sai daqui para a UI: só "configurada / não configurada".
//! - Nada aqui está no caminho crítico do ditado; o orquestrador é avisado por
//!   mensagem (`DictationEvent::ProviderChanged`) quando algo que o afeta muda.
//! - Trabalho bloqueante (disco, cofre, lista de processos) vai para
//!   `spawn_blocking` para não ocupar as threads do runtime assíncrono.

use std::{path::PathBuf, sync::Arc};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_dialog::DialogExt;

use crate::{
    audio::{self, InputDevice, Recorder, decode},
    config::{Config, SttEngine},
    dict_io,
    dictation::{DictationEvent, pipeline},
    files::{self, JobView},
    history::{self, HistoryEntry, HistoryItem},
    hotkey,
    metrics::Stats,
    overlay, paste, secrets, settings_window,
    state::{AppState, MISSING_KEY, Status},
    stt::{
        SttError,
        models::{self, ModelKind, ModelList},
        openrouter::{self, KeyInfo},
    },
    sysmem::{self, MemoryReport},
    updater::{self, UpdateInfo},
};

/// Erro devolvido à UI. O `invoke` do lado do TypeScript rejeita a Promise com
/// este texto, já pronto a mostrar.
#[derive(Debug)]
pub struct CommandError(String);

impl Serialize for CommandError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

/// Qualquer erro que se saiba mostrar (`Display`) converte-se num `CommandError`,
/// o que permite usar `?` diretamente nos comandos.
impl<E: std::fmt::Display> From<E> for CommandError {
    fn from(e: E) -> Self {
        Self(e.to_string())
    }
}

type CmdResult<T> = Result<T, CommandError>;
type AppStateRef<'a> = State<'a, Arc<AppState>>;

/// Corre `f` numa thread de bloqueio e devolve o resultado.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> CmdResult<T> {
    Ok(tauri::async_runtime::spawn_blocking(f).await?)
}

#[derive(Serialize)]
pub struct Paths {
    config: String,
    data: String,
    logs: String,
}

#[derive(Serialize)]
pub struct SettingsView {
    config: Config,
    defaults: Config,
    api_key_set: bool,
    /// O atalho de ditado está registado (`false`: outra app ocupou-o).
    shortcut_ok: bool,
    /// O atalho de comando está registado (ou o modo está desligado).
    command_ok: bool,
    /// O atalho do histórico rápido está registado (ou não há atalho).
    quick_ok: bool,
    autostart: bool,
    paths: Paths,
}

async fn settings_view(app: &AppHandle, state: &Arc<AppState>) -> CmdResult<SettingsView> {
    let api_key_set = blocking(|| secrets::get().map(|k| k.is_some())).await??;
    Ok(SettingsView {
        config: (*state.config()).clone(),
        defaults: Config::default(),
        api_key_set,
        shortcut_ok: state.shortcut_ok(),
        command_ok: state.command_ok(),
        quick_ok: state.quick_ok(),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        paths: Paths {
            config: Config::path(&state.config_dir).display().to_string(),
            data: state.data_dir.display().to_string(),
            logs: state.log_dir.display().to_string(),
        },
    })
}

#[tauri::command]
pub async fn get_settings(app: AppHandle, state: AppStateRef<'_>) -> CmdResult<SettingsView> {
    settings_view(&app, &state).await
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    state: AppStateRef<'_>,
    config: Config,
) -> CmdResult<SettingsView> {
    config.validate()?;
    let current = state.config();
    // O atalho de comando primeiro: se falhar, ainda não mexemos em nada.
    let command = apply_command_shortcut(&app, &state, &current, &config)?;
    let quick = match apply_quick_shortcut(&app, &state, &current, &config) {
        Ok(quick) => quick,
        Err(e) => {
            command.undo(&app, &state);
            return Err(e.into());
        }
    };
    // O atalho estava a funcionar antes desta gravação? (Se não, o antigo não
    // está registado e não há nada para largar nem para repor.)
    let was_ok = state.shortcut_ok();
    let shortcut_changed = config.shortcut != current.shortcut;
    let mut now_ok = was_ok;
    if shortcut_changed {
        let changed = if was_ok {
            hotkey::change_dictation(&app, &current.shortcut, &config.shortcut)
        } else {
            hotkey::register_dictation(&app, &config.shortcut)
        };
        if let Err(e) = changed {
            command.undo(&app, &state);
            quick.undo(&app, &state);
            return Err(e.into());
        }
        now_ok = true;
    } else if !was_ok && hotkey::register_dictation(&app, &config.shortcut).is_ok() {
        // O atalho estava ocupado e a outra app largou-o entretanto: aproveito
        // qualquer gravação para voltar a tentar, sem incomodar se falhar.
        now_ok = true;
    }
    // Quem transcreve (ou como) mudou: o orquestrador recria os clientes no próximo
    // ditado. O idioma só conta no motor local (o Whisper fixa-o ao carregar).
    let local_changed = config.stt_engine != current.stt_engine
        || config.local_model != current.local_model
        || config.local_idle_minutes != current.local_idle_minutes
        || (config.stt_engine == SttEngine::Local && config.language != current.language);
    let transport_changed = config.transport != current.transport || local_changed;
    // Um modelo que já não vai ser usado liberta a memória já, não aos 5 minutos.
    let drop_local_model = current.stt_engine == SttEngine::Local
        && (config.stt_engine != SttEngine::Local || config.local_model != current.local_model);
    let overlay_changed = config.show_overlay != current.show_overlay;
    let show_overlay = config.show_overlay;
    // Desligar "Guardar o áudio" apaga o que já estava guardado.
    let audio_turned_off = current.keep_audio && !config.keep_audio;
    let new_shortcut = config.shortcut.clone();
    let state_ = Arc::clone(&state);
    // Um erro do próprio `blocking` (a tarefa falhou) também tem de desfazer os
    // atalhos, por isso não se usa `?` aqui.
    let saved: CmdResult<()> = match blocking(move || state_.replace_config(config)).await {
        Ok(saved) => saved.map(drop).map_err(CommandError::from),
        Err(e) => Err(e),
    };
    if let Err(e) = saved {
        // Não conseguimos gravar: repõe o atalho antigo para que o que está ativo
        // continue igual ao que está no disco.
        if shortcut_changed {
            if was_ok {
                let _ = hotkey::change_dictation(&app, &new_shortcut, &current.shortcut);
            } else {
                hotkey::unregister_dictation(&app, &new_shortcut);
                now_ok = false;
            }
        }
        command.undo(&app, &state);
        quick.undo(&app, &state);
        if now_ok != was_ok {
            state.set_shortcut_ok(now_ok);
        }
        return Err(e);
    }
    // Avisa a barra e o tray (atalho novo, sons) mesmo que o estado do atalho
    // não tenha mudado.
    if now_ok != was_ok {
        state.set_shortcut_ok(now_ok);
    } else {
        state.publish();
    }
    if drop_local_model {
        state.local.unload();
    }
    if transport_changed {
        let _ = state.events.send(DictationEvent::ProviderChanged);
    }
    if audio_turned_off {
        let state_ = Arc::clone(&state);
        match blocking(move || state_.history.clear_audio()).await? {
            Ok(()) => tracing::info!("áudio dos ditados apagado"),
            Err(e) => tracing::warn!(error = %e, "não foi possível apagar o áudio dos ditados"),
        }
    }
    if overlay_changed {
        if show_overlay {
            overlay::create(&app);
        } else {
            overlay::destroy(&app);
        }
    }
    tracing::info!("definições guardadas");
    settings_view(&app, &state).await
}

/// O que `apply_command_shortcut` mudou, para o poder desfazer se o resto da
/// gravação falhar.
struct CommandChange {
    /// Atalho registado antes e depois (`None` = nenhum).
    before: Option<String>,
    after: Option<String>,
    was_ok: bool,
}

impl CommandChange {
    fn undo(&self, app: &AppHandle, state: &AppState) {
        if self.before != self.after
            && let Err(e) =
                hotkey::change_command(app, self.after.as_deref(), self.before.as_deref())
        {
            tracing::warn!(error = %e, "não foi possível repor o atalho de comando");
        }
        state.set_command_ok(self.was_ok);
    }
}

/// Liga, desliga ou troca o atalho do modo comando, como o de ditado: um atalho
/// novo ocupado por outra app é um erro (a gravação falha e a janela diz
/// porquê); o mesmo atalho, antes ocupado, só volta a ser tentado em silêncio.
fn apply_command_shortcut(
    app: &AppHandle,
    state: &AppState,
    current: &Config,
    config: &Config,
) -> Result<CommandChange, hotkey::HotkeyError> {
    let was_ok = state.command_ok();
    // O que está mesmo registado agora: com o atalho ocupado, nada.
    let before = (current.command_enabled && was_ok).then(|| current.command_shortcut.clone());
    let wanted = config
        .command_enabled
        .then(|| config.command_shortcut.clone());
    let same_as_saved = config.command_enabled
        && current.command_enabled
        && config.command_shortcut == current.command_shortcut;
    let (after, now_ok) = if before == wanted {
        (before.clone(), was_ok || !config.command_enabled)
    } else if same_as_saved {
        // Estava ocupado e continua igual: tenta outra vez, sem erro se falhar.
        match hotkey::change_command(app, None, wanted.as_deref()) {
            Ok(()) => (wanted, true),
            Err(_) => (None, false),
        }
    } else {
        hotkey::change_command(app, before.as_deref(), wanted.as_deref())?;
        (wanted, true)
    };
    state.set_command_ok(now_ok);
    Ok(CommandChange {
        before,
        after,
        was_ok,
    })
}

/// O que `apply_quick_shortcut` mudou, para o poder desfazer (como `CommandChange`).
struct QuickChange {
    /// Atalho registado antes e depois (`None` = nenhum).
    before: Option<String>,
    after: Option<String>,
    was_ok: bool,
}

impl QuickChange {
    fn undo(&self, app: &AppHandle, state: &AppState) {
        if self.before != self.after
            && let Err(e) = hotkey::change_quick(app, self.after.as_deref(), self.before.as_deref())
        {
            tracing::warn!(error = %e, "não foi possível repor o atalho do histórico rápido");
        }
        state.set_quick_ok(self.was_ok);
    }
}

/// O atalho do histórico rápido: vazio no config é "desligado". Como no comando,
/// um atalho novo ocupado por outra app é um erro; o mesmo atalho, antes ocupado,
/// só volta a ser tentado em silêncio.
fn apply_quick_shortcut(
    app: &AppHandle,
    state: &AppState,
    current: &Config,
    config: &Config,
) -> Result<QuickChange, hotkey::HotkeyError> {
    let non_empty = |s: &str| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
    let was_ok = state.quick_ok();
    let saved = non_empty(&current.quick_history_shortcut);
    // O que está mesmo registado agora: com o atalho ocupado, nada.
    let before = saved.clone().filter(|_| was_ok);
    let wanted = non_empty(&config.quick_history_shortcut);
    let (after, now_ok) = if before == wanted {
        (before.clone(), was_ok || wanted.is_none())
    } else if wanted.is_some() && wanted == saved {
        // Estava ocupado e continua igual: tenta outra vez, sem erro se falhar.
        match hotkey::change_quick(app, None, wanted.as_deref()) {
            Ok(()) => (wanted, true),
            Err(_) => (None, false),
        }
    } else {
        hotkey::change_quick(app, before.as_deref(), wanted.as_deref())?;
        (wanted, true)
    };
    state.set_quick_ok(now_ok);
    Ok(QuickChange {
        before,
        after,
        was_ok,
    })
}

/// Valida a chave no OpenRouter e só depois a guarda no cofre. Devolve o estado
/// da chave (limite, gasto), nunca a chave.
#[tauri::command]
pub async fn set_api_key(state: AppStateRef<'_>, key: String) -> CmdResult<Option<KeyInfo>> {
    let key = key.trim().to_owned();
    if key.is_empty() {
        return Err("cola a chave antes de guardar".into());
    }
    let info = match openrouter::check_key(&state.http, &key).await {
        Ok(info) => Some(info),
        // Chave rejeitada: não guardamos.
        Err(e @ (SttError::Unauthorized(_) | SttError::Forbidden(_))) => return Err(e.into()),
        // Sem rede (ou outro erro do servidor): guardamos na mesma e avisamos no log.
        Err(e) => {
            tracing::warn!(error = %e, "não foi possível validar a chave; guardada sem validação");
            None
        }
    };
    blocking(move || secrets::set(&key)).await??;
    let _ = state.events.send(DictationEvent::ProviderChanged);
    state.set_key_set(true); // a barra deixa de mostrar "Falta a chave"
    tracing::info!("chave do OpenRouter guardada");
    Ok(info)
}

#[tauri::command]
pub async fn delete_api_key(state: AppStateRef<'_>) -> CmdResult<()> {
    blocking(secrets::delete).await??;
    let _ = state.events.send(DictationEvent::ProviderChanged);
    state.set_key_set(false);
    tracing::info!("chave do OpenRouter apagada");
    Ok(())
}

#[tauri::command]
pub async fn test_api_key(state: AppStateRef<'_>) -> CmdResult<KeyInfo> {
    let key = blocking(secrets::get)
        .await??
        .ok_or("não há nenhuma chave guardada")?;
    Ok(openrouter::check_key(&state.http, &key).await?)
}

/// Lista de modelos: da cache se tiver menos de 24 h (e `refresh` for falso), senão
/// da API; se a API falhar, a cache antiga marcada como `stale`.
#[tauri::command]
pub async fn list_models(
    state: AppStateRef<'_>,
    kind: ModelKind,
    refresh: bool,
) -> CmdResult<ModelList> {
    let dir = state.cache_dir.clone();
    let cached = {
        let dir = dir.clone();
        blocking(move || models::read(&dir, kind)).await?
    };
    let now = time::OffsetDateTime::now_utc();
    if !refresh && let Some(list) = cached.as_ref().filter(|l| l.is_fresh(now)) {
        return Ok(list.clone());
    }
    match openrouter::list_models(&state.http, kind.output_modality()).await {
        Ok(models) => {
            let list = ModelList {
                fetched_at: now,
                models,
                stale: false,
            };
            let to_write = list.clone();
            blocking(move || {
                if let Err(e) = models::write(&dir, kind, &to_write) {
                    tracing::warn!(error = %e, "não foi possível guardar a cache de modelos");
                }
            })
            .await?;
            Ok(list)
        }
        Err(e) => match cached {
            Some(list) => {
                tracing::warn!(error = %e, "a usar a lista de modelos em cache");
                Ok(ModelList {
                    stale: true,
                    ..list
                })
            }
            None => Err(e.into()),
        },
    }
}

#[tauri::command]
pub async fn get_stats(state: AppStateRef<'_>) -> CmdResult<Stats> {
    let state = Arc::clone(&state);
    blocking(move || state.metrics.stats(50)).await
}

#[tauri::command]
pub async fn get_history(state: AppStateRef<'_>) -> CmdResult<Vec<HistoryItem>> {
    let state = Arc::clone(&state);
    blocking(move || state.history.list_items()).await
}

/// Quanto tempo o áudio de uma entrada apagada espera pelo "Desfazer". O aviso
/// fica 5 s, mas não desaparece enquanto o rato estiver por cima: há folga.
const UNDO_AUDIO_FOR: std::time::Duration = std::time::Duration::from_secs(60);

/// O id vem da janela: só serve se tiver o formato dos nossos (ver
/// `history::is_valid_id`). Nunca chega a ser um caminho sem passar por aqui.
fn checked_id(id: &str) -> CmdResult<()> {
    if history::is_valid_id(id) {
        Ok(())
    } else {
        Err("identificador de ditado inválido".into())
    }
}

/// Apaga uma entrada do histórico e devolve-a, para o "Desfazer" a poder repor.
/// O áudio dela (se houver) só é apagado daqui a `UNDO_AUDIO_FOR`.
#[tauri::command]
pub async fn delete_history_entry(
    state: AppStateRef<'_>,
    id: String,
) -> CmdResult<Option<HistoryEntry>> {
    checked_id(&id)?;
    let state = Arc::clone(&state);
    let state_ = Arc::clone(&state);
    let id_ = id.clone();
    let Some((removed, token)) = blocking(move || state_.history.remove(&id_)).await?? else {
        return Ok(None);
    };
    tracing::info!("ditado apagado do histórico");
    // O `token` identifica este apagamento: se o ditado for apagado outra vez (e
    // desfeito), este temporizador não apaga o áudio desse novo "Desfazer".
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(UNDO_AUDIO_FOR).await;
        let _ = blocking(move || {
            if let Err(e) = state.history.drop_pending_audio(&id, token) {
                tracing::warn!(error = %e, "não foi possível apagar o áudio do ditado");
            }
        })
        .await;
    });
    Ok(Some(removed))
}

/// "Desfazer" depois de apagar uma entrada.
#[tauri::command]
pub async fn restore_history_entry(state: AppStateRef<'_>, entry: HistoryEntry) -> CmdResult<()> {
    checked_id(&entry.id)?;
    let state = Arc::clone(&state);
    let limit = state.config().history_limit;
    blocking(move || state.history.restore(entry, limit)).await??;
    Ok(())
}

/// "Reprocessar": transcreve outra vez o áudio guardado de um ditado e
/// substitui o texto da entrada. Devolve a entrada atualizada.
#[tauri::command]
pub async fn reprocess_history_entry(
    state: AppStateRef<'_>,
    id: String,
) -> CmdResult<HistoryEntry> {
    checked_id(&id)?;
    let state = Arc::clone(&state);
    let (state_, id_) = (Arc::clone(&state), id.clone());
    let (entry, wav) = blocking(move || {
        let entry = state_
            .history
            .get(&id_)
            .ok_or("este ditado já não está no histórico")?;
        if entry.instruction.is_some() {
            return Err("os comandos não se podem reprocessar");
        }
        let wav = state_
            .history
            .read_audio(&id_)
            .map_err(|_| "já não tenho o áudio deste ditado")?;
        Ok((entry, wav))
    })
    .await??;
    let state_ = Arc::clone(&state);
    let providers = blocking(move || state_.load_providers())
        .await??
        .ok_or(MISSING_KEY)?;
    let new = pipeline::reprocess(Arc::clone(&state), providers, wav.into(), &entry).await?;
    let state_ = Arc::clone(&state);
    let updated = blocking(move || {
        state_.history.update(&id, |e| {
            e.text = new.text;
            e.raw_text = new.raw_text;
            e.model = new.model;
            e.total_ms = Some(new.total_ms);
            // O texto novo substitui a correção feita à mão.
            e.corrected = false;
        })
    })
    .await??
    .ok_or("o ditado foi apagado entretanto")?;
    // Métricas e Início atualizam-se como depois de um ditado.
    let _ = state.app.emit(pipeline::EVENT_DICTATION_RECORDED, ());
    Ok(updated)
}

/// "Corrigir": troca o texto de um ditado pelo que corrigiste à mão. Devolve a
/// entrada atualizada. O histórico rápido e o "Copiar" passam a dar o texto corrigido.
#[tauri::command]
pub async fn correct_history_entry(
    state: AppStateRef<'_>,
    id: String,
    text: String,
) -> CmdResult<HistoryEntry> {
    checked_id(&id)?;
    let state = Arc::clone(&state);
    let entry = blocking(move || state.history.correct(&id, &text)).await??;
    Ok(entry)
}

/// As últimas apps onde ditaste (só desta sessão), para criar um estilo sem
/// saber o nome do executável. O próprio Ditok fica de fora.
#[tauri::command]
pub fn recent_apps(state: AppStateRef<'_>) -> Vec<String> {
    let ours = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    state
        .recent_apps()
        .into_iter()
        .filter(|app| Some(app) != ours.as_ref())
        .collect()
}

#[tauri::command]
pub async fn clear_history(state: AppStateRef<'_>) -> CmdResult<()> {
    let state_ = Arc::clone(&state);
    // Leva também o áudio guardado (ver `HistoryStore::clear`).
    blocking(move || state_.history.clear()).await??;
    // O "Colar o último ditado" também deixa de o ter.
    let _ = state.events.send(DictationEvent::HistoryCleared);
    tracing::info!("histórico apagado");
    Ok(())
}

/// Copia um texto (ex.: um ditado do histórico) para o clipboard. É feito aqui e
/// não no WebView para usar o mesmo clipboard da colagem (no Windows, fora do
/// histórico do Win+V e da sincronização na nuvem).
#[tauri::command]
pub async fn copy_text(text: String) -> CmdResult<()> {
    blocking(move || paste::copy(&text)).await??;
    Ok(())
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> CmdResult<bool> {
    let launcher = app.autolaunch();
    if enabled {
        launcher.enable()?;
    } else {
        launcher.disable()?;
    }
    tracing::info!(enabled, "arranque automático");
    Ok(launcher.is_enabled()?)
}

#[tauri::command]
pub async fn get_memory() -> CmdResult<Option<MemoryReport>> {
    blocking(sysmem::report).await
}

/// `None` = já está na versão mais recente.
#[tauri::command]
pub async fn check_update(app: AppHandle) -> CmdResult<Option<UpdateInfo>> {
    Ok(updater::check(&app).await?)
}

/// Só devolve em caso de erro: se correr bem, a app reinicia.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> CmdResult<()> {
    Ok(updater::install(&app).await?)
}

// ---------------------------------------------------------------- barra flutuante

/// Clique na pílula: começa um ditado em modo alternar, ou termina-o.
#[tauri::command]
pub fn overlay_click(state: AppStateRef<'_>) {
    let _ = state.events.send(DictationEvent::Click);
}

/// Botão ✕ da pílula: igual a carregar em Esc.
#[tauri::command]
pub fn overlay_cancel(state: AppStateRef<'_>) {
    let _ = state.events.send(DictationEvent::Cancel);
}

/// A pílula mudou de tamanho: guarda onde está, para o rato saber quando lhe toca.
#[tauri::command]
pub fn overlay_hit_rect(rect: overlay::HitRect) {
    overlay::set_hit_rect(rect);
}

/// O que a pílula em repouso tem de avisar (chave em falta, atalho ocupado) e o
/// atalho para a dica. Pedido quando a barra carrega; depois chega por evento.
#[tauri::command]
pub fn overlay_status(state: AppStateRef<'_>) -> Status {
    state.status()
}

/// Clique na pílula com um aviso: abre a janela no sítio certo ("key" ou "shortcut").
#[tauri::command]
pub fn overlay_open(app: AppHandle, target: String) -> CmdResult<()> {
    if !matches!(target.as_str(), "key" | "shortcut") {
        return Err(format!("sítio desconhecido: {target}").into());
    }
    settings_window::open_at(&app, &target);
    Ok(())
}

/// Clique direito na pílula: o menu do tray. `async` para o comando não correr
/// na thread principal, que é onde o menu fica aberto à espera de escolha.
#[tauri::command]
pub async fn overlay_menu(app: AppHandle) {
    // A pílula nunca recebe o foco: a janela em primeiro plano ainda é a tua.
    crate::tray::remember_menu_target();
    overlay::popup_menu(&app);
}

// ---------------------------------------------------------------- janela principal

/// Para onde a janela deve ir ao abrir (ver `settings_window::open_at`).
#[tauri::command]
pub fn main_take_open() -> Option<String> {
    settings_window::take_pending()
}

/// Abre a página das chaves do OpenRouter no browser. O endereço é fixo: a
/// janela não consegue pedir para abrir outra coisa qualquer.
#[tauri::command]
pub fn open_openrouter_keys() -> CmdResult<()> {
    const URL: &str = "https://openrouter.ai/keys";
    #[cfg(windows)]
    let mut command = std::process::Command::new("explorer");
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    // O `explorer` devolve um código de saída diferente de 0 mesmo quando abre:
    // só conta se nem sequer arrancou.
    command.arg(URL).spawn()?;
    Ok(())
}

// ---------------------------------------------------------------- microfone

#[tauri::command]
pub async fn list_input_devices() -> CmdResult<Vec<InputDevice>> {
    Ok(blocking(audio::input_devices).await??)
}

/// Liga (ou renova) o medidor de nível do microfone escolhido, para o guia. O
/// nível chega à janela pelo evento `mic:level`. A janela renova-o a cada 2 s;
/// sem renovação, desliga-se sozinho.
#[tauri::command]
pub fn mic_monitor(state: AppStateRef<'_>, recorder: State<'_, Recorder>, on: bool) {
    if on {
        recorder.monitor_start(state.config().input_device.clone());
    } else {
        recorder.monitor_stop();
    }
}

// ---------------------------------------------------------------- ficheiros

/// O que a janela mostra antes de começar.
#[derive(Serialize)]
pub struct FileProbe {
    name: String,
    path: String,
    duration_s: Option<f64>,
    size_bytes: u64,
}

/// Lê o cabeçalho de um ficheiro: confirma que o formato é suportado e diz a duração.
#[tauri::command]
pub async fn file_probe(path: String) -> CmdResult<FileProbe> {
    let path = PathBuf::from(path);
    blocking(move || {
        let info = decode::probe(&path)?;
        Ok(FileProbe {
            name: path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            size_bytes: std::fs::metadata(&path)?.len(),
            duration_s: info.duration.map(|d| d.as_secs_f64()),
            path: path.display().to_string(),
        })
    })
    .await?
}

/// O diálogo do sistema para escolher um ficheiro: a alternativa a arrastar.
#[tauri::command]
pub async fn file_pick(app: AppHandle) -> CmdResult<Option<String>> {
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Escolher um ficheiro para transcrever")
        .add_filter("Áudio e vídeo", decode::EXTENSIONS);
    if let Some(window) = app.get_webview_window(settings_window::LABEL) {
        dialog = dialog.set_parent(&window);
    }
    // O diálogo responde num callback; o `oneshot` transforma-o em algo que se
    // pode esperar com `.await`.
    let (tx, rx) = tokio::sync::oneshot::channel();
    dialog.pick_file(move |picked| {
        let _ = tx.send(picked);
    });
    let picked = rx.await.ok().flatten();
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string()))
}

#[tauri::command]
pub async fn file_start(state: AppStateRef<'_>, path: String) -> CmdResult<JobView> {
    let path = PathBuf::from(path);
    let state_ = Arc::clone(&state);
    let providers = blocking(move || state_.load_providers())
        .await??
        .ok_or(MISSING_KEY)?;
    let probe_path = path.clone();
    let info = blocking(move || decode::probe(&probe_path)).await??;
    Ok(files::start(state.inner(), providers.stt, path, info)?)
}

#[tauri::command]
pub fn file_cancel(state: AppStateRef<'_>) {
    state.files.cancel();
}

#[tauri::command]
pub fn file_status(state: AppStateRef<'_>) -> Option<JobView> {
    state.files.view()
}

#[tauri::command]
pub fn file_clear(state: AppStateRef<'_>) {
    state.files.clear();
}

/// Guarda o texto ao lado do ficheiro original; devolve o caminho.
#[tauri::command]
pub async fn file_save_beside(state: AppStateRef<'_>) -> CmdResult<String> {
    let state = Arc::clone(&state);
    let path = blocking(move || state.files.save_beside()).await??;
    tracing::info!(path = %path.display(), "transcrição guardada ao lado do original");
    Ok(path.display().to_string())
}

// ---------------------------------------------------------------- dicionário

/// O diálogo do sistema para escolher onde guardar. `None` se cancelaste.
async fn ask_save_path(
    app: &AppHandle,
    title: &str,
    file_name: &str,
    filter: (&str, &[&str]),
) -> Option<PathBuf> {
    let mut dialog = app
        .dialog()
        .file()
        .set_title(title)
        .set_file_name(file_name)
        .add_filter(filter.0, filter.1);
    if let Some(window) = app.get_webview_window(settings_window::LABEL) {
        dialog = dialog.set_parent(&window);
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    dialog.save_file(move |picked| {
        let _ = tx.send(picked);
    });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

/// O diálogo do sistema para escolher um ficheiro. `None` se cancelaste.
async fn ask_open_path(app: &AppHandle, title: &str, filter: (&str, &[&str])) -> Option<PathBuf> {
    let mut dialog = app
        .dialog()
        .file()
        .set_title(title)
        .add_filter(filter.0, filter.1);
    if let Some(window) = app.get_webview_window(settings_window::LABEL) {
        dialog = dialog.set_parent(&window);
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    dialog.pick_file(move |picked| {
        let _ = tx.send(picked);
    });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

/// Guarda o dicionário (e, se pedido, as expansões) num ficheiro à escolha.
/// Devolve o caminho, ou `None` se cancelaste o diálogo.
#[tauri::command]
pub async fn dictionary_export(
    app: AppHandle,
    state: AppStateRef<'_>,
    format: dict_io::Format,
    with_snippets: bool,
) -> CmdResult<Option<String>> {
    let config = state.config();
    let name = format!("ditok-dicionario.{}", format.extension());
    let Some(path) = ask_save_path(
        &app,
        "Exportar o dicionário",
        &name,
        ("Dicionário", &[format.extension()]),
    )
    .await
    else {
        return Ok(None);
    };
    let target = path.clone();
    blocking(move || -> CmdResult<()> {
        let bytes = dict_io::export(&config.dictionary, &config.snippets, format, with_snippets);
        std::fs::write(&target, bytes)?;
        Ok(())
    })
    .await??;
    tracing::info!(path = %path.display(), ?format, with_snippets, "dicionário exportado");
    Ok(Some(path.display().to_string()))
}

/// O que um ficheiro de dicionário faria às tuas listas, nos dois modos de resolver
/// conflitos. Nada é gravado: quem grava é a UI, depois de confirmares.
#[derive(Serialize)]
pub struct ImportPreview {
    file_name: String,
    format: dict_io::Format,
    /// Entradas do ficheiro que não servem (vazias ou compridas demais).
    invalid: usize,
    keep: dict_io::Merged,
    replace: dict_io::Merged,
}

/// Escolhe um ficheiro, lê-o e mostra o que mudava. `None` se cancelaste.
#[tauri::command]
pub async fn dictionary_import_preview(
    app: AppHandle,
    state: AppStateRef<'_>,
) -> CmdResult<Option<ImportPreview>> {
    let Some(path) = ask_open_path(
        &app,
        "Importar para o dicionário",
        ("Dicionário (JSON, CSV, TXT)", &["json", "csv", "txt"]),
    )
    .await
    else {
        return Ok(None);
    };
    let config = state.config();
    let preview = blocking(move || -> CmdResult<ImportPreview> {
        let format = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(dict_io::Format::from_extension)
            .ok_or("Formato não suportado: usa um ficheiro .json, .csv ou .txt")?;
        if std::fs::metadata(&path)?.len() > dict_io::MAX_FILE_BYTES {
            return Err(dict_io::IoError::TooBig.into());
        }
        let parsed = dict_io::parse(&std::fs::read(&path)?, format)?;
        let merge = |mode| dict_io::merge(&config.dictionary, &config.snippets, &parsed, mode);
        let keep = merge(dict_io::ImportMode::Keep)?;
        let replace = merge(dict_io::ImportMode::Replace)?;
        Ok(ImportPreview {
            file_name: path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            format,
            invalid: keep.invalid,
            keep,
            replace,
        })
    })
    .await??;
    Ok(Some(preview))
}

// ---------------------------------------------------------------- estatísticas de uso

/// O resumo para a página Métricas: palavras, sequência, mapa de calor, apps.
#[tauri::command]
pub async fn usage_summary(state: AppStateRef<'_>) -> CmdResult<crate::usage::UsageSummary> {
    let state = Arc::clone(&state);
    blocking(move || {
        let days = state.usage.load();
        crate::usage::summarize(
            &days,
            chrono::Local::now().date_naive(),
            state.config().typing_wpm,
        )
    })
    .await
}

/// "Apagar estatísticas": zera os dias. Os custos e as métricas de tempo ficam.
#[tauri::command]
pub async fn usage_clear(state: AppStateRef<'_>) -> CmdResult<()> {
    let state = Arc::clone(&state);
    blocking(move || state.usage.clear()).await??;
    tracing::info!("estatísticas de uso apagadas");
    Ok(())
}

// ---------------------------------------------------------------- resumos

/// Um resumo pronto, com o que custou.
#[derive(Serialize)]
pub struct SummaryOut {
    text: String,
    model: String,
    cost_usd: Option<f64>,
    seconds: f64,
}

/// Resume um texto (a transcrição de um ficheiro, um ditado comprido) com o modelo
/// dos resumos. O texto vai ao OpenRouter: quem chama (a janela) só o faz quando
/// carregas em "Resumir". O custo entra nas métricas e no orçamento do mês.
#[tauri::command]
pub async fn summarize_text(
    state: AppStateRef<'_>,
    text: String,
    kind: crate::summarize::Kind,
) -> CmdResult<SummaryOut> {
    let config = state.config();
    let state_ = Arc::clone(&state);
    let openrouter = blocking(move || state_.load_openrouter())
        .await??
        .ok_or(MISSING_KEY)?;
    let model = config.effective_summary_model().to_owned();
    let summary = crate::summarize::summarize(&openrouter, &model, kind, &text).await?;
    let metrics = crate::metrics::DictationMetrics::for_summary(
        &model,
        text.chars().count(),
        summary.elapsed.as_millis().try_into().unwrap_or(u64::MAX),
        summary.cost,
    );
    let budget = config.monthly_budget_usd;
    let state_ = Arc::clone(&state);
    match blocking(move || state_.metrics.record(&metrics, budget)).await? {
        Ok(Some(alert)) => state.notify(&alert.message()),
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "não foi possível guardar as métricas do resumo"),
    }
    tracing::info!(
        ?kind,
        model = %model,
        chars = text.chars().count(),
        seconds = summary.elapsed.as_secs_f64(),
        cost_usd = summary.cost,
        "resumo"
    );
    Ok(SummaryOut {
        text: summary.text,
        model,
        cost_usd: summary.cost,
        seconds: summary.elapsed.as_secs_f64(),
    })
}

/// Guarda o resumo (Markdown) ao lado do ficheiro transcrito; devolve o caminho.
#[tauri::command]
pub async fn file_save_summary(state: AppStateRef<'_>, text: String) -> CmdResult<String> {
    let state = Arc::clone(&state);
    let path = blocking(move || state.files.save_summary_beside(&text)).await??;
    tracing::info!(path = %path.display(), "resumo guardado ao lado do original");
    Ok(path.display().to_string())
}

// ---------------------------------------------------------------- modelos locais

/// O evento de progresso das descargas (para a janela de definições).
const EVENT_LOCAL_MODELS: &str = "local-models:progress";

#[derive(Serialize)]
pub struct LocalModelView {
    id: crate::local_models::LocalModelId,
    label: &'static str,
    description: &'static str,
    warning: Option<&'static str>,
    size_bytes: u64,
    /// Memória aproximada com o modelo carregado.
    ram_mb: u32,
    installed: bool,
    /// Bytes já descarregados (uma descarga parada retoma daí).
    downloaded_bytes: u64,
}

#[derive(Serialize)]
pub struct LocalModelsView {
    /// Esta versão da app inclui o motor local?
    available: bool,
    /// O modelo está carregado em memória agora.
    loaded: bool,
    /// Uma descarga a decorrer (o id do modelo).
    downloading: Option<crate::local_models::LocalModelId>,
    models: Vec<LocalModelView>,
}

fn local_models_view(state: &AppState) -> LocalModelsView {
    let dir = state.models_dir();
    let downloading = state
        .download
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(|d| d.id);
    LocalModelsView {
        available: crate::stt::local::AVAILABLE,
        loaded: state.local.is_loaded(),
        downloading,
        models: crate::local_models::CATALOG
            .iter()
            .map(|m| LocalModelView {
                id: m.id,
                label: m.label,
                description: m.description,
                warning: m.warning,
                size_bytes: m.total_bytes(),
                ram_mb: m.ram_mb,
                installed: m.is_installed(&dir),
                downloaded_bytes: m.downloaded_bytes(&dir),
            })
            .collect(),
    }
}

#[tauri::command]
pub async fn local_models_list(state: AppStateRef<'_>) -> CmdResult<LocalModelsView> {
    let state = Arc::clone(&state);
    blocking(move || local_models_view(&state)).await
}

/// Começa a descarregar um modelo (a descarga corre em segundo plano; o progresso vem
/// pelo evento `local-models:progress`). Uma de cada vez.
#[tauri::command]
pub async fn local_model_download(
    app: AppHandle,
    state: AppStateRef<'_>,
    id: crate::local_models::LocalModelId,
) -> CmdResult<()> {
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let mut active = state
            .download
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active.is_some() {
            return Err("já há um modelo a descarregar: espera que acabe ou cancela-o".into());
        }
        *active = Some(crate::local_models::ActiveDownload {
            id: Some(id),
            speaker: None,
            cancel: Arc::clone(&cancel),
        });
    }
    let client = crate::local_models::download_client()?;
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn(async move {
        let spec = crate::local_models::spec(id);
        tracing::info!(model = spec.label, "a descarregar o modelo local");
        let emit = {
            let app = app.clone();
            move |p: crate::local_models::Progress| {
                let _ = app.emit_to(settings_window::LABEL, EVENT_LOCAL_MODELS, &p);
            }
        };
        let result = crate::local_models::download(
            &client,
            spec,
            &state.models_dir(),
            &cancel,
            emit.clone(),
        )
        .await;
        *state
            .download
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let (state_name, message) = match &result {
            Ok(()) => ("done", None),
            Err(crate::local_models::DownloadError::Cancelled) => ("cancelled", None),
            Err(e) => ("error", Some(e.to_string())),
        };
        if let Err(e) = &result {
            tracing::warn!(model = spec.label, error = %e, "descarga do modelo local não concluída");
        }
        let total = spec.total_bytes();
        emit(crate::local_models::Progress {
            id: crate::local_models::dir_name(id),
            file: String::new(),
            done: if result.is_ok() {
                total
            } else {
                spec.downloaded_bytes(&state.models_dir())
            },
            total,
            state: state_name,
            message,
        });
    });
    Ok(())
}

#[tauri::command]
pub fn local_model_cancel(state: AppStateRef<'_>) {
    if let Some(active) = state
        .download
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        active
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Apaga um modelo descarregado (e o que ficou a meio).
#[tauri::command]
pub async fn local_model_delete(
    state: AppStateRef<'_>,
    id: crate::local_models::LocalModelId,
) -> CmdResult<()> {
    if state.config().stt_engine == SttEngine::Local && state.config().local_model == id {
        return Err("este é o modelo em uso: escolhe outro (ou a nuvem) antes de o apagar".into());
    }
    let state = Arc::clone(&state);
    blocking(move || {
        state.local.unload();
        crate::local_models::delete(crate::local_models::spec(id), &state.models_dir())
    })
    .await??;
    tracing::info!(?id, "modelo local apagado");
    Ok(())
}

// ---------------------------------------------------------------- notas (reuniões)

use crate::notes::{
    session::{self, StateView},
    speakers,
    store::{self, Line, Meta, Mode, NotesStore},
};

/// Uma sessão aberta: a meta, as linhas prontas a mostrar e o resumo, se já houver.
#[derive(Serialize)]
pub struct NoteOut {
    meta: Meta,
    lines: Vec<Line>,
    summary: Option<String>,
}

#[tauri::command]
pub fn notes_state(state: AppStateRef<'_>) -> StateView {
    state.notes.view()
}

/// Guarda que o aviso sobre gravar conversas foi lido e aceite.
#[tauri::command]
pub async fn notes_accept_consent(state: AppStateRef<'_>) -> CmdResult<()> {
    let mut config = (*state.config()).clone();
    if config.notes_consent {
        return Ok(());
    }
    config.notes_consent = true;
    let state_ = Arc::clone(&state);
    blocking(move || state_.replace_config(config)).await??;
    let _ = state.app.emit(crate::state::EVENT_SETTINGS_CHANGED, ());
    Ok(())
}

#[tauri::command]
pub async fn notes_start(state: AppStateRef<'_>, mode: Mode) -> CmdResult<StateView> {
    Ok(session::start(&state, mode).await?)
}

#[tauri::command]
pub async fn notes_stop(state: AppStateRef<'_>) -> CmdResult<StateView> {
    Ok(session::stop(&state).await?)
}

#[tauri::command]
pub async fn notes_list(state: AppStateRef<'_>) -> CmdResult<Vec<Meta>> {
    let dir = state.data_dir.clone();
    blocking(move || NotesStore::new(&dir).list()).await
}

#[tauri::command]
pub async fn notes_get(state: AppStateRef<'_>, id: String) -> CmdResult<NoteOut> {
    let dir = state.data_dir.clone();
    blocking(move || -> Result<NoteOut, store::StoreError> {
        let store = NotesStore::new(&dir);
        let meta = store.meta(&id)?;
        let lines = store::for_display(&store.lines(&id)?, meta.mode);
        let summary = store.summary(&id)?;
        Ok(NoteOut {
            meta,
            lines,
            summary,
        })
    })
    .await?
    .map_err(Into::into)
}

/// Não se mexe na sessão que está a gravar (ou a fechar).
fn not_active(state: &AppState, id: &str) -> CmdResult<()> {
    if state.notes.view().id.as_deref() == Some(id) {
        return Err("esta gravação ainda está em curso".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn notes_delete(state: AppStateRef<'_>, id: String) -> CmdResult<()> {
    not_active(&state, &id)?;
    let dir = state.data_dir.clone();
    blocking(move || NotesStore::new(&dir).delete(&id)).await??;
    Ok(())
}

/// Muda o título e/ou os nomes das faixas ("Eu" → "Manuel"). Campos vazios ficam como estão.
#[tauri::command]
pub async fn notes_rename(
    state: AppStateRef<'_>,
    id: String,
    title: Option<String>,
    mic_name: Option<String>,
    system_name: Option<String>,
) -> CmdResult<Meta> {
    not_active(&state, &id)?;
    let dir = state.data_dir.clone();
    blocking(move || -> Result<Meta, store::StoreError> {
        let store = NotesStore::new(&dir);
        let mut meta = store.meta(&id)?;
        let clean = |s: Option<String>| {
            s.map(|s| s.trim().chars().take(80).collect::<String>())
                .filter(|s| !s.is_empty())
        };
        if let Some(t) = clean(title) {
            meta.title = t;
        }
        if let Some(n) = clean(mic_name) {
            meta.mic_name = n;
        }
        if let Some(n) = clean(system_name) {
            meta.system_name = n;
        }
        store.save_meta(&meta)?;
        Ok(meta)
    })
    .await?
    .map_err(Into::into)
}

/// O texto corrido de uma sessão ("Eu: …\nOutros: …"), para o resumir.
#[tauri::command]
pub async fn notes_text(state: AppStateRef<'_>, id: String) -> CmdResult<String> {
    let dir = state.data_dir.clone();
    blocking(move || -> Result<String, store::StoreError> {
        let store = NotesStore::new(&dir);
        let meta = store.meta(&id)?;
        let lines = store::for_display(&store.lines(&id)?, meta.mode);
        Ok(store::plain_for_summary(&meta, &lines))
    })
    .await?
    .map_err(Into::into)
}

#[tauri::command]
pub async fn notes_save_summary(
    state: AppStateRef<'_>,
    id: String,
    summary: String,
) -> CmdResult<()> {
    let dir = state.data_dir.clone();
    blocking(move || NotesStore::new(&dir).save_summary(&id, &summary)).await??;
    Ok(())
}

/// Exporta a sessão (com o resumo, se houver) para um ficheiro à escolha. `None` se cancelaste.
#[tauri::command]
pub async fn notes_export(
    app: AppHandle,
    state: AppStateRef<'_>,
    id: String,
    markdown: bool,
) -> CmdResult<Option<String>> {
    let dir = state.data_dir.clone();
    let id_ = id.clone();
    let (text, title) = blocking(move || -> Result<(String, String), store::StoreError> {
        let store = NotesStore::new(&dir);
        let meta = store.meta(&id_)?;
        let lines = store::for_display(&store.lines(&id_)?, meta.mode);
        let mut text = if markdown {
            store::export_markdown(&meta, &lines)
        } else {
            store::export_text(&meta, &lines)
        };
        if let Some(summary) = store.summary(&id_)? {
            text = format!("{summary}\n\n---\n\n{text}");
        }
        Ok((text, meta.title))
    })
    .await??;
    let ext = if markdown { "md" } else { "txt" };
    let name = format!(
        "{}.{ext}",
        title.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "-")
    );
    let Some(path) = ask_save_path(&app, "Exportar as notas", &name, ("Notas", &[ext])).await
    else {
        return Ok(None);
    };
    let target = path.clone();
    blocking(move || std::fs::write(&target, text)).await??;
    Ok(Some(path.display().to_string()))
}

/// Os dispositivos de saída que se podem captar (por agora só informativo).
#[tauri::command]
pub async fn notes_output_devices() -> CmdResult<Vec<InputDevice>> {
    Ok(blocking(crate::notes::capture::output_devices).await??)
}

/// Teste de áudio: `seconds` de microfone e de áudio do computador, com os níveis ao
/// vivo (evento `notes:test`). Usa as mesmas escolhas que a gravação.
#[tauri::command]
pub async fn notes_test_start(state: AppStateRef<'_>, seconds: u64) -> CmdResult<()> {
    let config = state.config();
    let capture = crate::notes::capture::CaptureConfig {
        mic: true,
        system: true,
        mic_device: config.input_device.clone(),
        output_device: config.notes_output_device.clone(),
    };
    crate::notes::check::start(state.app.clone(), capture, seconds.clamp(5, 30))?;
    Ok(())
}

// ---------------------------------------------------------------- oradores (notes/speakers.rs)

const EVENT_SPEAKERS: &str = "speakers:progress";

#[derive(Serialize)]
pub struct SpeakerModelView {
    id: speakers::Model,
    label: &'static str,
    description: &'static str,
    size_bytes: u64,
    installed: bool,
}

#[derive(Serialize)]
pub struct SpeakersView {
    /// Esta compilação inclui o motor local (que também separa oradores)?
    available: bool,
    /// O modelo escolhido nas definições: é o que as Notas usam.
    selected: speakers::Model,
    models: Vec<SpeakerModelView>,
    /// Qual dos modelos está a descarregar agora, se algum.
    downloading: Option<speakers::Model>,
}

/// O progresso de uma descarga de oradores: o de `local_models::Progress` e o modelo.
#[derive(Serialize)]
struct SpeakersProgress {
    model: speakers::Model,
    #[serde(flatten)]
    progress: crate::local_models::Progress,
}

fn speakers_view(state: &AppState) -> SpeakersView {
    let dir = state.models_dir();
    let downloading = state
        .download
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(|d| d.speaker);
    SpeakersView {
        available: speakers::AVAILABLE,
        selected: state.config().notes_speaker_model,
        models: speakers::Model::ALL
            .iter()
            .map(|&m| SpeakerModelView {
                id: m,
                label: m.label(),
                description: m.description(),
                size_bytes: m.total_bytes(),
                installed: speakers::is_installed(&dir, m),
            })
            .collect(),
        downloading,
    }
}

#[tauri::command]
pub async fn speakers_list(state: AppStateRef<'_>) -> CmdResult<SpeakersView> {
    let state = Arc::clone(&state);
    blocking(move || speakers_view(&state)).await
}

/// Descarrega um modelo de oradores. O progresso vai no evento `speakers:progress`.
#[tauri::command]
pub async fn speakers_download(
    app: AppHandle,
    state: AppStateRef<'_>,
    id: speakers::Model,
) -> CmdResult<()> {
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let mut active = state
            .download
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active.is_some() {
            return Err("já há um modelo a descarregar: espera que acabe ou cancela-o".into());
        }
        *active = Some(crate::local_models::ActiveDownload {
            id: None,
            speaker: Some(id),
            cancel: Arc::clone(&cancel),
        });
    }
    let client = crate::local_models::download_client()?;
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn(async move {
        let emit = {
            let app = app.clone();
            move |p: crate::local_models::Progress| {
                let _ = app.emit_to(
                    settings_window::LABEL,
                    EVENT_SPEAKERS,
                    &SpeakersProgress {
                        model: id,
                        progress: p,
                    },
                );
            }
        };
        tracing::info!(model = ?id, "a descarregar o modelo de oradores");
        let result = crate::notes::speakers::download(
            &client,
            &state.models_dir(),
            id,
            &cancel,
            emit.clone(),
        )
        .await;
        *state
            .download
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let (state_name, message) = match &result {
            Ok(()) => ("done", None),
            Err(crate::local_models::DownloadError::Cancelled) => ("cancelled", None),
            Err(e) => ("error", Some(e.to_string())),
        };
        if let Err(e) = &result {
            tracing::warn!(error = %e, "descarga do modelo de oradores não concluída");
        }
        let total = id.total_bytes();
        emit(crate::local_models::Progress {
            id: "speakers",
            file: String::new(),
            done: if result.is_ok() { total } else { 0 },
            total,
            state: state_name,
            message,
        });
    });
    Ok(())
}

/// Apaga um modelo de oradores descarregado (e o que ficou a meio).
#[tauri::command]
pub async fn speakers_delete(state: AppStateRef<'_>, id: speakers::Model) -> CmdResult<()> {
    let dir = state.models_dir();
    blocking(move || crate::notes::speakers::delete(&dir, id)).await??;
    Ok(())
}

/// Dá um nome a um orador separado ("Orador 2" → "Ana"). Nome vazio volta ao "Orador N".
#[tauri::command]
pub async fn notes_rename_speaker(
    state: AppStateRef<'_>,
    id: String,
    speaker: u32,
    name: String,
) -> CmdResult<Meta> {
    not_active(&state, &id)?;
    let dir = state.data_dir.clone();
    blocking(move || -> Result<Meta, store::StoreError> {
        let store = NotesStore::new(&dir);
        let mut meta = store.meta(&id)?;
        let name: String = name.trim().chars().take(80).collect();
        if name.is_empty() {
            meta.speaker_names.remove(&speaker);
        } else {
            meta.speaker_names.insert(speaker, name);
        }
        store.save_meta(&meta)?;
        Ok(meta)
    })
    .await?
    .map_err(Into::into)
}
