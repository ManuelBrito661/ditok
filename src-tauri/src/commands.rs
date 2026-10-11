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
    overlay, paste, secrets,
    state::AppState,
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
