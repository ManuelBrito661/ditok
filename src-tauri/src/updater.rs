//! Atualizações automáticas a partir das Releases do GitHub.
//!
//! - Ao arrancar (e depois de 6 em 6 horas) pergunta se há versão nova; se houver,
//!   avisa com uma notificação. Nunca instala sozinho: o utilizador pode estar a ditar.
//! - A instalação só acontece quando se carrega em "Atualizar" nas definições.
//! - O endpoint e a chave pública estão em `tauri.conf.json` (`plugins.updater`);
//!   o plugin recusa qualquer pacote que não esteja assinado com a chave privada.

use std::time::Duration;

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

use crate::state::AppState;

const FIRST_CHECK: Duration = Duration::from_secs(10);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// O que a janela de definições precisa de saber sobre uma versão nova.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub current: String,
    pub version: String,
    pub notes: Option<String>,
}

pub async fn check(app: &AppHandle) -> anyhow::Result<Option<UpdateInfo>> {
    let update = app.updater()?.check().await?;
    Ok(update.map(|u| UpdateInfo {
        current: u.current_version.clone(),
        version: u.version.clone(),
        notes: u.body.clone(),
    }))
}

/// Descarrega, instala e reinicia. No Windows o instalador fecha a app sozinho,
/// por isso o `restart` só chega a correr no macOS/Linux.
pub async fn install(app: &AppHandle) -> anyhow::Result<()> {
    let Some(update) = app.updater()?.check().await? else {
        anyhow::bail!("Já tens a versão mais recente.");
    };
    tracing::info!(version = %update.version, "a instalar atualização");
    update.download_and_install(|_chunk, _total| {}, || {}).await?;
    tracing::info!("atualização instalada, a reiniciar");
    app.restart();
}

/// Verificação periódica em segundo plano. Avisa uma vez por versão nova.
pub fn spawn_background_checks(app: AppHandle, state: std::sync::Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        let mut notified: Option<String> = None;
        loop {
            match check(&app).await {
                Ok(Some(info)) if notified.as_deref() != Some(&info.version) => {
                    tracing::info!(version = %info.version, "versão nova disponível");
                    state.notify(&format!(
                        "Há uma versão nova ({}). Abre as definições para atualizar.",
                        info.version
                    ));
                    notified = Some(info.version);
                }
                Ok(_) => {}
                // Sem rede ou ainda sem Releases: não é grave, tenta-se mais tarde.
                Err(e) => tracing::warn!(error = %e, "verificação de atualizações falhou"),
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}
