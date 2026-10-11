//! Logs com `tracing`: consola (em dev) + ficheiro diário em `app_log_dir`
//! (Windows: %LOCALAPPDATA%\com.manuelbrito.ditok\logs), guardando 7 dias.
//! Nível ajustável com a variável de ambiente `RUST_LOG`.

use std::{path::Path, sync::Mutex};

use anyhow::Context;
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[cfg(debug_assertions)]
const DEFAULT_FILTER: &str = "ditok_lib=debug,ditok=debug,warn";
#[cfg(not(debug_assertions))]
const DEFAULT_FILTER: &str = "ditok_lib=info,ditok=info,warn";

/// A escrita no ficheiro é feita por uma thread de fundo (`non_blocking`), para que
/// um log nunca atrase o caminho crítico. O `WorkerGuard` garante que o que está em
/// buffer é escrito quando é largado (drop), por isso guardamo-lo até à saída.
static GUARD: Mutex<Option<WorkerGuard>> = Mutex::new(None);

pub fn init(log_dir: &Path) -> anyhow::Result<()> {
    // O appender limpa ficheiros antigos ao arrancar e queixa-se se a pasta não existir.
    std::fs::create_dir_all(log_dir)
        .with_context(|| format!("criar a pasta de logs {}", log_dir.display()))?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("ditok")
        .filename_suffix("log")
        .max_log_files(7)
        .build(log_dir)
        .with_context(|| format!("criar logs em {}", log_dir.display()))?;
    let (file_writer, guard) = tracing_appender::non_blocking(appender);

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(fmt::layer().with_ansi(false).with_writer(file_writer))
        .try_init()
        .context("inicializar tracing")?;

    if let Ok(mut slot) = GUARD.lock() {
        *slot = Some(guard);
    }
    tracing::info!(dir = %log_dir.display(), "logs em ficheiro ativos");
    Ok(())
}

/// Chamado em `RunEvent::Exit`: escreve o que falta no ficheiro.
pub fn shutdown() {
    if let Ok(mut slot) = GUARD.lock() {
        slot.take();
    }
}
