//! Ícone e menu da bandeja do sistema. O ícone reflete o estado do ditado com as
//! poses da mascote (ver design/mascot e scripts/gen-icons.mjs). Cada mudança de
//! estado também é enviada à barra flutuante (`overlay.rs`), para os dois nunca
//! mostrarem coisas diferentes.

use serde::Serialize;
use tauri::{
    AppHandle, Wry,
    image::Image,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{TrayIcon, TrayIconBuilder},
};

const MENU_OPEN_SETTINGS: &str = "open_settings";
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
    /// A transcrição ou a colagem falhou (a notificação explica porquê).
    Failed,
    /// Não havia voz na gravação (ou o modelo não ouviu nada): nada foi colado.
    NothingHeard,
}

impl TrayState {
    fn tooltip(self) -> &'static str {
        match self {
            Self::Idle => "Ditok: à espera do atalho",
            Self::Recording => "Ditok: a ouvir…",
            Self::Processing => "Ditok: a transcrever…",
            Self::Done => "Ditok: texto colado",
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
    idle: Image<'static>,
    recording: Image<'static>,
    processing: Image<'static>,
    done: Image<'static>,
    state: TrayState,
}

impl Tray {
    pub fn new(app: &AppHandle) -> tauri::Result<Self> {
        let idle = Image::from_bytes(include_bytes!("../icons/tray/idle.png"))?;
        let recording = Image::from_bytes(include_bytes!("../icons/tray/recording.png"))?;
        let processing = Image::from_bytes(include_bytes!("../icons/tray/processing.png"))?;
        let done = Image::from_bytes(include_bytes!("../icons/tray/done.png"))?;

        let open = MenuItemBuilder::with_id(MENU_OPEN_SETTINGS, "Abrir definições").build(app)?;
        let quit = MenuItemBuilder::with_id(MENU_QUIT, "Sair").build(app)?;
        let menu = MenuBuilder::new(app)
            .item(&open)
            .separator()
            .item(&quit)
            .build()?;

        let icon = TrayIconBuilder::with_id("main")
            .icon(idle.clone())
            .tooltip(TrayState::Idle.tooltip())
            .menu(&menu)
            .show_menu_on_left_click(true)
            .on_menu_event(|app: &AppHandle<Wry>, event| match event.id().as_ref() {
                MENU_OPEN_SETTINGS => {
                    crate::settings_window::open(app);
                }
                MENU_QUIT => {
                    tracing::info!("menu: sair");
                    // `exit(0)` passa um código ao ExitRequested; é assim que o
                    // distinguimos do "fechou-se a última janela" (ver lib.rs).
                    app.exit(0);
                }
                other => tracing::warn!(id = other, "item de menu desconhecido"),
            })
            .build(app)?;

        Ok(Self {
            app: app.clone(),
            icon,
            idle,
            recording,
            processing,
            done,
            state: TrayState::Idle,
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
        let image = match state {
            TrayState::Idle | TrayState::Failed | TrayState::NothingHeard => &self.idle,
            TrayState::Recording => &self.recording,
            TrayState::Processing => &self.processing,
            TrayState::Done => &self.done,
        };
        // `set_icon` fica com a imagem (ownership), por isso clonamos: são 16 KB de
        // RGBA (64×64×4), e só acontece numa mudança de estado.
        if let Err(e) = self.icon.set_icon(Some(image.clone())) {
            tracing::warn!(error = %e, ?state, "não foi possível mudar o ícone do tray");
        }
        if let Err(e) = self.icon.set_tooltip(Some(state.tooltip())) {
            tracing::warn!(error = %e, "não foi possível mudar a tooltip do tray");
        }
        self.state = state;
        crate::overlay::emit_state(&self.app, state);
    }

    pub fn set_tooltip(&self, text: &str) {
        if let Err(e) = self.icon.set_tooltip(Some(text)) {
            tracing::warn!(error = %e, "não foi possível mudar a tooltip do tray");
        }
    }
}
