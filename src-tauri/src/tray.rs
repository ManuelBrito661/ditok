//! Ícone e menu da bandeja do sistema. O ícone reflete o estado do ditado com as
//! poses da mascote (ver design/mascot e scripts/gen-icons.mjs). Cada mudança de
//! estado também é enviada à barra flutuante (`overlay.rs`), para os dois nunca
//! mostrarem coisas diferentes.

use serde::Serialize;
use tauri::{
    AppHandle, Wry,
    image::Image,
    menu::{MenuBuilder, MenuItem, MenuItemBuilder},
    tray::{TrayIcon, TrayIconBuilder},
};
use tokio::sync::mpsc::UnboundedSender;

use crate::dictation::{DictationEvent, failure::FailReason};

const MENU_OPEN_SETTINGS: &str = "open_settings";
const MENU_RETRY: &str = "retry";
const MENU_QUIT: &str = "quit";

/// Serializado em minúsculas ("recording", "nothing-heard", …) porque é o que a
/// barra recebe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrayState {
    Idle,
    Recording,
    Processing,
    Done,
    /// Não se colou (a janela em foco mudou, ou o sistema não deixou), mas o texto
    /// ficou no clipboard, pronto para Ctrl+V.
    Copied,
    /// A gravação, a transcrição ou a colagem falhou (o motivo curto vai à
    /// parte, em `Tray::fail`; a notificação explica o resto).
    Failed,
    /// Não havia voz na gravação (ou o modelo não ouviu nada): nada foi colado.
    NothingHeard,
}

#[cfg(target_os = "macos")]
const COPIED_TOOLTIP: &str = "Ditok: texto no clipboard, cola com ⌘V";
#[cfg(not(target_os = "macos"))]
const COPIED_TOOLTIP: &str = "Ditok: texto no clipboard, cola com Ctrl+V";

impl TrayState {
    fn tooltip(self) -> &'static str {
        match self {
            Self::Idle => "Ditok: à espera do atalho",
            Self::Recording => "Ditok: a ouvir…",
            Self::Processing => "Ditok: a transcrever…",
            Self::Done => "Ditok: texto colado",
            Self::Copied => COPIED_TOOLTIP,
            Self::Failed => "Ditok: o último ditado falhou",
            Self::NothingHeard => "Ditok: não ouvi nada",
        }
    }
}

/// Os PNG são embutidos no binário com `include_bytes!` (sem ler do disco no
/// arranque) e descodificados uma única vez; trocar de estado só troca o ícone.
pub struct Tray {
    app: AppHandle,
    icon: TrayIcon,
    /// Guardado para o podermos ativar/desativar conforme haja gravação falhada.
    retry: MenuItem<Wry>,
    idle: Image<'static>,
    recording: Image<'static>,
    processing: Image<'static>,
    done: Image<'static>,
    state: TrayState,
    /// Motivo do último erro, mostrado na tooltip até ao próximo ditado.
    last_error: Option<FailReason>,
}

impl Tray {
    /// `events`: o menu "Tentar de novo" avisa o orquestrador por aqui.
    pub fn new(app: &AppHandle, events: UnboundedSender<DictationEvent>) -> tauri::Result<Self> {
        let idle = Image::from_bytes(include_bytes!("../icons/tray/idle.png"))?;
        let recording = Image::from_bytes(include_bytes!("../icons/tray/recording.png"))?;
        let processing = Image::from_bytes(include_bytes!("../icons/tray/processing.png"))?;
        let done = Image::from_bytes(include_bytes!("../icons/tray/done.png"))?;

        let open = MenuItemBuilder::with_id(MENU_OPEN_SETTINGS, "Abrir o Ditok").build(app)?;
        // Desativado até haver uma gravação falhada para reenviar.
        let retry = MenuItemBuilder::with_id(MENU_RETRY, "Tentar de novo o último ditado")
            .enabled(false)
            .build(app)?;
        let quit = MenuItemBuilder::with_id(MENU_QUIT, "Sair").build(app)?;
        let menu = MenuBuilder::new(app)
            .item(&open)
            .item(&retry)
            .separator()
            .item(&quit)
            .build()?;

        let icon = TrayIconBuilder::with_id("main")
            .icon(idle.clone())
            .tooltip(TrayState::Idle.tooltip())
            .menu(&menu)
            .show_menu_on_left_click(true)
            // `move` dá ao closure a sua cópia do `Sender`.
            .on_menu_event(
                move |app: &AppHandle<Wry>, event| match event.id().as_ref() {
                    MENU_OPEN_SETTINGS => {
                        crate::settings_window::open(app);
                    }
                    MENU_RETRY => {
                        tracing::info!("menu: tentar de novo");
                        let _ = events.send(DictationEvent::Retry);
                    }
                    MENU_QUIT => {
                        tracing::info!("menu: sair");
                        // `exit(0)` passa um código ao ExitRequested; é assim que o
                        // distinguimos do "fechou-se a última janela" (ver lib.rs).
                        app.exit(0);
                    }
                    other => tracing::warn!(id = other, "item de menu desconhecido"),
                },
            )
            .build(app)?;

        Ok(Self {
            app: app.clone(),
            icon,
            retry,
            idle,
            recording,
            processing,
            done,
            state: TrayState::Idle,
            last_error: None,
        })
    }

    pub fn state(&self) -> TrayState {
        self.state
    }

    /// Muda o ícone. Um erro aqui é só cosmético, por isso regista-se e segue-se em
    /// frente em vez de interromper o ditado.
    pub fn set(&mut self, state: TrayState) {
        if self.state == state {
            return;
        }
        // Um novo ditado "apaga" o último erro da tooltip.
        if matches!(state, TrayState::Recording | TrayState::Processing) {
            self.last_error = None;
        }
        self.apply(state);
    }

    /// Mostra uma falha com o seu motivo curto. `retry`: a barra oferece "Tentar
    /// de novo" (há uma gravação guardada para reenviar).
    pub fn fail(&mut self, reason: FailReason, retry: bool) {
        self.last_error = Some(reason);
        // O motivo vai antes do estado, para a barra já o ter quando mudar.
        crate::overlay::emit_failure(&self.app, reason.label(), retry);
        // Mesmo que já estivesse em "falhou" (duas falhas seguidas), atualiza.
        self.apply(TrayState::Failed);
    }

    fn apply(&mut self, state: TrayState) {
        let image = match state {
            TrayState::Idle | TrayState::Failed | TrayState::NothingHeard => &self.idle,
            TrayState::Recording => &self.recording,
            TrayState::Processing => &self.processing,
            TrayState::Done | TrayState::Copied => &self.done,
        };
        // `set_icon` fica com a imagem (ownership), por isso clonamos: são 16 KB de
        // RGBA (64×64×4), e só acontece numa mudança de estado.
        if let Err(e) = self.icon.set_icon(Some(image.clone())) {
            tracing::warn!(error = %e, ?state, "não foi possível mudar o ícone do tray");
        }
        let tooltip = match (state, self.last_error) {
            (TrayState::Failed, Some(reason)) => format!("Ditok: falhou ({})", reason.label()),
            (TrayState::Idle, Some(reason)) => {
                format!("{} · último erro: {}", state.tooltip(), reason.label())
            }
            _ => state.tooltip().to_owned(),
        };
        self.set_tooltip(&tooltip);
        self.state = state;
        crate::overlay::emit_state(&self.app, state);
    }

    /// Ativa ou desativa o "Tentar de novo" do menu.
    pub fn set_retry_available(&self, available: bool) {
        if let Err(e) = self.retry.set_enabled(available) {
            tracing::warn!(error = %e, "não foi possível mudar o menu do tray");
        }
    }

    pub fn set_tooltip(&self, text: &str) {
        if let Err(e) = self.icon.set_tooltip(Some(text)) {
            tracing::warn!(error = %e, "não foi possível mudar a tooltip do tray");
        }
    }
}
