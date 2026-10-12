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
    config::Config,
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
            return Err(e.into());
        }
        now_ok = true;
    } else if !was_ok && hotkey::register_dictation(&app, &config.shortcut).is_ok() {
        // O atalho estava ocupado e a outra app largou-o entretanto: aproveito
        // qualquer gravação para voltar a tentar, sem incomodar se falhar.
        now_ok = true;
    }
    let transport_changed = config.transport != current.transport;
    let overlay_changed = config.show_overlay != current.show_overlay;
    let show_overlay = config.show_overlay;
    // Desligar "Guardar o áudio" apaga o que já estava guardado.
    let audio_turned_off = current.keep_audio && !config.keep_audio;
    let new_shortcut = config.shortcut.clone();
    let state_ = Arc::clone(&state);
    let saved = blocking(move || state_.replace_config(config)).await?;
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
        if now_ok != was_ok {
            state.set_shortcut_ok(now_ok);
        }
        return Err(e.into());
    }
    // Avisa a barra e o tray (atalho novo, sons) mesmo que o estado do atalho
    // não tenha mudado.
    if now_ok != was_ok {
        state.set_shortcut_ok(now_ok);
    } else {
        state.publish();
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
    let removed = blocking(move || state_.history.remove(&id_)).await??;
    if removed.is_some() {
        tracing::info!("ditado apagado do histórico");
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(UNDO_AUDIO_FOR).await;
            let _ = blocking(move || {
                if let Err(e) = state.history.drop_pending_audio(&id) {
                    tracing::warn!(error = %e, "não foi possível apagar o áudio do ditado");
                }
            })
            .await;
        });
    }
    Ok(removed)
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
    let openrouter = blocking(move || state_.load_openrouter())
        .await??
        .ok_or(MISSING_KEY)?;
    let new = pipeline::reprocess(Arc::clone(&state), openrouter, wav.into(), &entry).await?;
    let state_ = Arc::clone(&state);
    let updated = blocking(move || {
        state_.history.update(&id, |e| {
            e.text = new.text;
            e.raw_text = new.raw_text;
            e.model = new.model;
            e.total_ms = Some(new.total_ms);
        })
    })
    .await??
    .ok_or("o ditado foi apagado entretanto")?;
    // Métricas e Início atualizam-se como depois de um ditado.
    let _ = state.app.emit(pipeline::EVENT_DICTATION_RECORDED, ());
    Ok(updated)
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
    let openrouter = blocking(move || state_.load_openrouter())
        .await??
        .ok_or(MISSING_KEY)?;
    let probe_path = path.clone();
    let info = blocking(move || decode::probe(&probe_path)).await??;
    Ok(files::start(state.inner(), openrouter, path, info)?)
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
