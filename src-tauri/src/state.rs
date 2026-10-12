//! Estado partilhado entre o orquestrador, as tarefas de processamento e os
//! comandos da janela de definições. Vive num `Arc<AppState>` gerido pelo Tauri.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

/// Quantas apps recentes se lembram, para escolher numa lista ao criar um estilo.
const RECENT_APPS: usize = 8;

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    config::Config, dictation::DictationEvent, files::FileJobs, history::HistoryStore, hotkey,
    metrics::MetricsStore, overlay, secrets, stt::openrouter::OpenRouter, text::TextRules, tray,
};

/// Emitido para a janela principal quando as definições mudam por fora dela (ex.:
/// "Silenciar sons" no menu do tray), para não gravar por cima com valores velhos.
pub const EVENT_SETTINGS_CHANGED: &str = "settings-changed";

/// O que impede o ditado de funcionar, para os avisos persistentes da barra e a
/// linha do atalho no menu do tray.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub key_missing: bool,
    /// O atalho de ditado está registado (outra app pode tê-lo ocupado).
    pub shortcut_ok: bool,
    /// O atalho como está no config.json ("CommandOrControl+Shift+Space").
    pub shortcut: String,
    /// O mesmo, legível ("Ctrl+Shift+Espaço").
    pub shortcut_label: String,
    /// O atalho do modo comando, legível; `None` com o modo desligado.
    pub command_label: Option<String>,
    /// `false` só quando o modo comando está ligado e outra app tem o atalho.
    pub command_ok: bool,
    /// O atalho do histórico rápido, legível; `None` sem atalho.
    pub quick_label: Option<String>,
    /// `false` só quando há atalho do histórico rápido e outra app o tem.
    pub quick_ok: bool,
}

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
    /// Há chave no cofre? Guardado aqui para a barra não ter de ir ao cofre.
    /// `AtomicBool`: escrito pelos comandos, lido por quem quiser, sem `Mutex`.
    key_set: AtomicBool,
    shortcut_ok: AtomicBool,
    /// O atalho de comando não está em falta (desligado conta como "ok").
    command_ok: AtomicBool,
    /// O atalho do histórico rápido não está em falta (sem atalho conta como "ok").
    quick_ok: AtomicBool,
    /// As últimas apps onde ditaste, mais recente primeiro. Só em memória (nunca
    /// em disco): servem para a janela de definições propor "Usar esta app" sem
    /// teres de saber o nome do executável. Um `Mutex` chega: é tocado uma vez
    /// por ditado e quando abres o separador Estilos.
    recent_apps: Mutex<VecDeque<String>>,
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
            key_set: AtomicBool::new(false),
            shortcut_ok: AtomicBool::new(true),
            command_ok: AtomicBool::new(true),
            quick_ok: AtomicBool::new(true),
            recent_apps: Mutex::new(VecDeque::new()),
        }
    }

    pub fn status(&self) -> Status {
        let config = self.config();
        let shortcut = config.shortcut.clone();
        Status {
            key_missing: !self.key_set.load(Ordering::Relaxed),
            shortcut_ok: self.shortcut_ok(),
            shortcut_label: hotkey::pretty(&shortcut),
            shortcut,
            command_label: config
                .command_enabled
                .then(|| hotkey::pretty(&config.command_shortcut)),
            command_ok: self.command_ok(),
            quick_label: Some(config.quick_history_shortcut.trim())
                .filter(|s| !s.is_empty())
                .map(hotkey::pretty),
            quick_ok: self.quick_ok(),
        }
    }

    pub fn shortcut_ok(&self) -> bool {
        self.shortcut_ok.load(Ordering::Relaxed)
    }

    pub fn command_ok(&self) -> bool {
        self.command_ok.load(Ordering::Relaxed)
    }

    /// Só guarda o valor: quem chama faz `publish` (as definições mudam as duas
    /// coisas de uma vez e avisam uma só vez).
    pub fn set_command_ok(&self, ok: bool) {
        self.command_ok.store(ok, Ordering::Relaxed);
    }

    pub fn quick_ok(&self) -> bool {
        self.quick_ok.load(Ordering::Relaxed)
    }

    /// Como `set_command_ok`: só guarda; quem chama faz `publish`.
    pub fn set_quick_ok(&self, ok: bool) {
        self.quick_ok.store(ok, Ordering::Relaxed);
    }

    pub fn set_key_set(&self, set: bool) {
        self.key_set.store(set, Ordering::Relaxed);
        self.publish();
    }

    pub fn key_set(&self) -> bool {
        self.key_set.load(Ordering::Relaxed)
    }

    pub fn set_shortcut_ok(&self, ok: bool) {
        self.shortcut_ok.store(ok, Ordering::Relaxed);
        self.publish();
    }

    /// Conta à barra e ao menu do tray o estado atual (chave, atalho, sons).
    /// Barato: um evento e duas mudanças de texto no menu.
    pub fn publish(&self) {
        let status = self.status();
        overlay::emit_notice(&self.app, &status);
        tray::sync_menu(&self.app, &status, self.config().sounds);
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

    /// Lembra a app onde começou um ditado (sem repetir; a mais recente à frente).
    pub fn note_app(&self, app: &str) {
        let mut recent = self
            .recent_apps
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        push_recent(&mut recent, app);
    }

    pub fn recent_apps(&self) -> Vec<String> {
        let recent = self
            .recent_apps
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        recent.iter().cloned().collect()
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

/// Põe `app` à frente da lista, sem repetidos, e corta para `RECENT_APPS`.
fn push_recent(recent: &mut VecDeque<String>, app: &str) {
    recent.retain(|a| a != app);
    recent.push_front(app.to_owned());
    recent.truncate(RECENT_APPS);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_apps_are_unique_newest_first_and_capped() {
        let mut recent = VecDeque::new();
        for app in ["a.exe", "b.exe", "a.exe"] {
            push_recent(&mut recent, app);
        }
        assert_eq!(recent, ["a.exe", "b.exe"]);
        for i in 0..20 {
            push_recent(&mut recent, &format!("{i}.exe"));
        }
        assert_eq!(recent.len(), RECENT_APPS);
        assert_eq!(recent[0], "19.exe");
    }
}
