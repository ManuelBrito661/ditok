//! Comandos Tauri chamados pela janela de definições (`invoke` em `src/api.ts`).
//!
//! Regras:
//! - A chave da API nunca sai daqui para a UI: só "configurada / não configurada".
//! - Nada aqui está no caminho crítico do ditado; o orquestrador é avisado por
//!   mensagem (`DictationEvent::ProviderChanged`) quando algo que o afeta muda.
//! - Trabalho bloqueante (disco, cofre, lista de processos) vai para
//!   `spawn_blocking` para não ocupar as threads do runtime assíncrono.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;

use crate::{
    config::Config,
    dictation::DictationEvent,
    history::HistoryEntry,
    hotkey,
    metrics::Stats,
    secrets,
    state::AppState,
    stt::{
        SttError,
        models::{self, ModelKind, ModelList},
        openrouter::{self, KeyInfo},
    },
    sysmem::{self, MemoryReport},
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
