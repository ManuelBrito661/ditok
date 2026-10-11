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
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_dialog::DialogExt;

use crate::{
    audio::decode,
    config::Config,
    dictation::DictationEvent,
    files::{self, JobView},
    history::HistoryEntry,
    hotkey,
    metrics::Stats,
    overlay, paste, secrets, settings_window,
    state::{AppState, MISSING_KEY},
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
    autostart: bool,
    paths: Paths,
}

async fn settings_view(app: &AppHandle, state: &Arc<AppState>) -> CmdResult<SettingsView> {
    let api_key_set = blocking(|| secrets::get().map(|k| k.is_some())).await??;
    Ok(SettingsView {
        config: (*state.config()).clone(),
        defaults: Config::default(),
        api_key_set,
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
    if config.shortcut != current.shortcut {
        hotkey::change_dictation(&app, &current.shortcut, &config.shortcut)?;
    }
    let transport_changed = config.transport != current.transport;
    let overlay_changed = config.show_overlay != current.show_overlay;
    let show_overlay = config.show_overlay;
    let new_shortcut = config.shortcut.clone();
    let state_ = Arc::clone(&state);
    let saved = blocking(move || state_.replace_config(config)).await?;
    if let Err(e) = saved {
        // Não conseguimos gravar: repõe o atalho antigo para que o que está ativo
        // continue igual ao que está no disco.
        let _ = hotkey::change_dictation(&app, &new_shortcut, &current.shortcut);
        return Err(e.into());
    }
    if transport_changed {
        let _ = state.events.send(DictationEvent::ProviderChanged);
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
        Err(e @ SttError::Unauthorized(_)) => return Err(e.into()),
        // Sem rede (ou outro erro do servidor): guardamos na mesma e avisamos no log.
        Err(e) => {
            tracing::warn!(error = %e, "não foi possível validar a chave; guardada sem validação");
            None
        }
    };
    blocking(move || secrets::set(&key)).await??;
    let _ = state.events.send(DictationEvent::ProviderChanged);
    tracing::info!("chave do OpenRouter guardada");
    Ok(info)
}

#[tauri::command]
pub async fn delete_api_key(state: AppStateRef<'_>) -> CmdResult<()> {
    blocking(secrets::delete).await??;
    let _ = state.events.send(DictationEvent::ProviderChanged);
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
pub async fn get_history(state: AppStateRef<'_>) -> CmdResult<Vec<HistoryEntry>> {
    let state = Arc::clone(&state);
    blocking(move || state.history.list()).await
}

#[tauri::command]
pub async fn clear_history(state: AppStateRef<'_>) -> CmdResult<()> {
    let state = Arc::clone(&state);
    blocking(move || state.history.clear()).await??;
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
