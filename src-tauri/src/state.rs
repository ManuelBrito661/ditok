//! Estado partilhado entre o orquestrador, as tarefas de processamento e os
//! comandos da janela de definições. Vive num `Arc<AppState>` gerido pelo Tauri.

use std::{
    path::PathBuf,
    sync::{Arc, PoisonError, RwLock},
};

use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    config::Config, dictation::DictationEvent, history::HistoryStore, metrics::MetricsStore,
    secrets, stt::openrouter::OpenRouter,
};

pub struct AppState {
    pub app: AppHandle,
    /// A configuração atual. Quem a lê recebe um `Arc<Config>` (uma "fotografia"):
    /// um ditado em curso usa a mesma configuração do princípio ao fim, mesmo que a
    /// guardes na janela a meio. O `RwLock` só é tocado durante a troca do `Arc`,
    /// por isso nunca bloqueia o caminho crítico de forma percetível.
    config: RwLock<Arc<Config>>,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub log_dir: PathBuf,
    pub http: reqwest::Client,
    pub metrics: MetricsStore,
    pub history: HistoryStore,
    /// Para avisar o orquestrador (ex.: a chave mudou).
    pub events: UnboundedSender<DictationEvent>,
}

pub struct Dirs {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub log: PathBuf,
}

impl AppState {
    pub fn new(
        app: AppHandle,
        config: Config,
        dirs: Dirs,
        http: reqwest::Client,
        events: UnboundedSender<DictationEvent>,
    ) -> Self {
        Self {
            app,
            config: RwLock::new(Arc::new(config)),
            metrics: MetricsStore::new(dirs.data.clone()),
            history: HistoryStore::new(dirs.data.clone()),
            config_dir: dirs.config,
            data_dir: dirs.data,
            cache_dir: dirs.cache,
            log_dir: dirs.log,
            http,
            events,
        }
    }

    pub fn config(&self) -> Arc<Config> {
        // Um `RwLock` fica "envenenado" se uma thread entrar em pânico com ele
        // trancado. O valor lá dentro continua válido (só trocamos `Arc`s), por isso
        // recuperamo-lo em vez de propagar o pânico.
        Arc::clone(&self.config.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Grava no disco e só depois troca em memória. O write lock serializa duas
    /// gravações simultâneas vindas da UI.
    pub fn replace_config(&self, new: Config) -> std::io::Result<Arc<Config>> {
        let mut guard = self.config.write().unwrap_or_else(PoisonError::into_inner);
        new.save(&self.config_dir)?;
        let new = Arc::new(new);
        *guard = Arc::clone(&new);
        Ok(new)
    }

    /// Cria o cliente do OpenRouter se a chave existir no cofre. `Ok(None)` = sem chave.
    pub fn load_openrouter(&self) -> Result<Option<Arc<OpenRouter>>, secrets::SecretError> {
        let transport = self.config().transport;
        Ok(secrets::get()?.map(|key| Arc::new(OpenRouter::new(self.http.clone(), key, transport))))
    }

    pub fn debug_dir(&self) -> PathBuf {
        self.data_dir.join("debug")
    }

    pub fn notify(&self, body: &str) {
        if let Err(e) = self
            .app
            .notification()
            .builder()
            .title("Ditok")
            .body(body)
            .show()
        {
            tracing::warn!(error = %e, "não foi possível mostrar a notificação");
        }
    }
}

pub const MISSING_KEY: &str =
    "Falta a chave do OpenRouter: abre as definições no ícone do Ditok e cola a tua chave.";
