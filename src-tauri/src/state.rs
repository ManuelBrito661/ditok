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
    config::Config, dictation::DictationEvent, files::FileJobs, history::HistoryStore,
    metrics::MetricsStore, secrets, stt::openrouter::OpenRouter, text::TextRules,
};

pub struct AppState {
    pub app: AppHandle,
    /// A configuração atual. Quem a lê recebe um `Arc<Config>` (uma "fotografia"):
    /// um ditado em curso usa a mesma configuração do princípio ao fim, mesmo que a
    /// guardes na janela a meio. O `RwLock` só é tocado durante a troca do `Arc`,
    /// por isso nunca bloqueia o caminho crítico de forma percetível.
    current: RwLock<Current>,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub log_dir: PathBuf,
    pub http: reqwest::Client,
    pub metrics: MetricsStore,
    pub history: HistoryStore,
    /// O ficheiro a ser transcrito (ou o último), ver `files.rs`.
    pub files: FileJobs,
    /// Para avisar o orquestrador (ex.: a chave mudou).
    pub events: UnboundedSender<DictationEvent>,
}

/// A configuração e as regras de texto compiladas a partir dela. Andam juntas
/// para nunca haver um ditado com o dicionário de uma versão e o resto de outra.
struct Current {
    config: Arc<Config>,
    rules: Arc<TextRules>,
}

impl Current {
    fn new(config: Config) -> Self {
        Self {
            rules: Arc::new(TextRules::new(&config)),
            config: Arc::new(config),
        }
    }
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
            current: RwLock::new(Current::new(config)),
            metrics: MetricsStore::new(dirs.data.clone()),
            history: HistoryStore::new(dirs.data.clone()),
            files: FileJobs::default(),
            config_dir: dirs.config,
            data_dir: dirs.data,
            cache_dir: dirs.cache,
            log_dir: dirs.log,
            http,
            events,
        }
    }

    pub fn config(&self) -> Arc<Config> {
        Arc::clone(&self.read().config)
    }

    /// O dicionário, as expansões e os comandos, já compilados.
    pub fn text_rules(&self) -> Arc<TextRules> {
        Arc::clone(&self.read().rules)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Current> {
        // Um `RwLock` fica "envenenado" se uma thread entrar em pânico com ele
        // trancado. O valor lá dentro continua válido (só trocamos `Arc`s), por isso
        // recuperamo-lo em vez de propagar o pânico.
        self.current.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Grava no disco e só depois troca em memória. O write lock serializa duas
    /// gravações simultâneas vindas da UI.
    pub fn replace_config(&self, new: Config) -> std::io::Result<Arc<Config>> {
        // As regras compilam-se antes de trancar: quem lê a configuração (um
        // ditado a começar) nunca fica à espera do regex.
        let new = Current::new(new);
        let mut guard = self.current.write().unwrap_or_else(PoisonError::into_inner);
        new.config.save(&self.config_dir)?;
        *guard = new;
        Ok(Arc::clone(&guard.config))
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
