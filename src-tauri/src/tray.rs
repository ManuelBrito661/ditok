//! Ícone e menu da bandeja do sistema. O ícone reflete o estado do ditado com as
//! poses da mascote (ver design/mascot e scripts/gen-icons.mjs). Cada mudança de
//! estado também é enviada à barra flutuante (`overlay.rs`), para os dois nunca
//! mostrarem coisas diferentes.
//!
//! O mesmo menu abre com o clique direito no ícone e na pílula da barra
//! (`overlay::popup_menu`). No Windows, o clique esquerdo no ícone abre a janela.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Manager, Wry,
    image::Image,
    menu::{CheckMenuItem, CheckMenuItemBuilder, Menu, MenuBuilder, MenuItem, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    config::Config,
    dictation::{DictationEvent, failure::FailReason},
    focus, hotkey,
    state::{AppState, EVENT_SETTINGS_CHANGED, Status},
};

const MENU_OPEN_SETTINGS: &str = "open_settings";
const MENU_OPEN_ASSISTANT: &str = "open_assistant";
const MENU_PASTE_LAST: &str = "paste_last";
const MENU_RETRY: &str = "retry";
const MENU_SHORTCUT: &str = "shortcut";
const MENU_COMMAND: &str = "command";
/// Onde fica a linha do atalho de comando: logo a seguir à do atalho de ditado
/// (abrir, assistente, colar, tentar de novo, separador, atalho → posição 6).
const COMMAND_POSITION: usize = 6;
const MENU_MUTE: &str = "mute";
const MENU_RESET_OVERLAY: &str = "reset_overlay";
const MENU_QUIT: &str = "quit";
const MENU_NOTES_STOP: &str = "notes_stop";

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

/// As partes do menu que mudam fora do orquestrador (o atalho, os sons) e o
/// próprio menu, para a barra o abrir com o clique direito. Fica guardado no
/// Tauri (`app.manage`): os itens de menu são "pegas" baratas de clonar que
/// podem ser usadas de qualquer thread.
pub struct TrayMenu {
    menu: Menu<Wry>,
    shortcut: MenuItem<Wry>,
    /// Só está no menu com o modo comando ligado (`command_shown`).
    command: MenuItem<Wry>,
    command_shown: AtomicBool,
    mute: CheckMenuItem<Wry>,
    /// "Parar a gravação das notas": só está no menu (à cabeça) enquanto se grava.
    notes_stop: MenuItem<Wry>,
    notes_shown: AtomicBool,
}

/// A janela onde estavas antes de abrir o menu (tray ou barra), para o "Colar o
/// último ditado" colar lá e não no próprio menu. `Mutex` porque é escrita pela
/// thread de eventos e lida pelo orquestrador.
static MENU_TARGET: Mutex<Option<focus::Window>> = Mutex::new(None);

/// Chamado quando o rato chega ao ícone do tray ou ao abrir o menu da barra:
/// nesse instante a janela em primeiro plano ainda é a tua.
pub fn remember_menu_target() {
    if let Ok(mut slot) = MENU_TARGET.lock() {
        *slot = focus::foreground_for_retry();
    }
}

/// A janela guardada (e esquece-a, para não colar lá numa próxima vez por engano).
pub fn take_menu_target() -> Option<focus::Window> {
    MENU_TARGET.lock().ok().and_then(|mut slot| slot.take())
}

/// O menu, para o abrir noutro sítio (a pílula). `None` antes de o tray existir.
pub fn menu(app: &AppHandle) -> Option<Menu<Wry>> {
    app.try_state::<TrayMenu>().map(|m| m.menu.clone())
}

/// Atualiza a linha do atalho e o visto de "Silenciar sons".
pub fn sync_menu(app: &AppHandle, status: &Status, sounds: bool) {
    let Some(m) = app.try_state::<TrayMenu>() else {
        return;
    };
    let text = if status.shortcut_ok {
        format!("Atalho: {}", status.shortcut_label)
    } else {
        format!("Atalho ocupado: {}", status.shortcut_label)
    };
    if let Err(e) = m.shortcut.set_text(text) {
        tracing::warn!(error = %e, "não foi possível mudar o menu do tray");
    }
    // A linha do modo comando só aparece com ele ligado. `swap` devolve o valor
    // antigo: só se mexe no menu quando muda.
    let show = status.command_label.is_some();
    let was_shown = m.command_shown.swap(show, Ordering::Relaxed);
    let result = match (was_shown, show) {
        (false, true) => m.menu.insert(
            &m.command,
            COMMAND_POSITION + usize::from(m.notes_shown.load(Ordering::Relaxed)),
        ),
        (true, false) => m.menu.remove(&m.command),
        _ => Ok(()),
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, "não foi possível mudar o menu do tray");
    }
    if let Some(label) = &status.command_label {
        let text = if status.command_ok {
            format!("Comando: {label}")
        } else {
            format!("Comando ocupado: {label}")
        };
        if let Err(e) = m.command.set_text(text) {
            tracing::warn!(error = %e, "não foi possível mudar o menu do tray");
        }
    }
    let notes = status.notes_recording;
    let result = match (m.notes_shown.swap(notes, Ordering::Relaxed), notes) {
        (false, true) => m.menu.insert(&m.notes_stop, 0),
        (true, false) => m.menu.remove(&m.notes_stop),
        _ => Ok(()),
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, "não foi possível mudar o menu do tray");
    }
    if let Err(e) = m.mute.set_checked(!sounds) {
        tracing::warn!(error = %e, "não foi possível mudar o menu do tray");
    }
}

/// "Silenciar sons": muda a mesma opção dos sons das definições.
fn toggle_sounds(app: &AppHandle) {
    let Some(state) = app.try_state::<Arc<AppState>>() else {
        return;
    };
    let state = Arc::clone(&state);
    // Gravar o config.json é disco: fora da thread de eventos.
    tauri::async_runtime::spawn_blocking(move || {
        let mut config = (*state.config()).clone();
        config.sounds = !config.sounds;
        let sounds = config.sounds;
        match state.replace_config(config) {
            Ok(_) => tracing::info!(sounds, "menu: sons"),
            Err(e) => tracing::warn!(error = %e, "não foi possível guardar os sons"),
        }
        // Repõe o visto com o valor que ficou guardado (mesmo que a gravação falhe)
        // e avisa a janela, se estiver aberta.
        state.publish();
        let _ = state.app.emit(EVENT_SETTINGS_CHANGED, ());
    });
}

/// Os PNG são embutidos no binário com `include_bytes!` (sem ler do disco no
/// arranque) e descodificados uma única vez; trocar de estado só troca o ícone.
pub struct Tray {
    app: AppHandle,
    icon: TrayIcon,
    /// Guardado para o podermos ativar/desativar conforme haja gravação falhada.
    retry: MenuItem<Wry>,
    /// Ativo quando há um ditado desta sessão (ou do histórico) para colar.
    paste_last: MenuItem<Wry>,
    idle: Image<'static>,
    recording: Image<'static>,
    processing: Image<'static>,
    done: Image<'static>,
    state: TrayState,
    /// Motivo do último erro, mostrado na tooltip até ao próximo ditado.
    last_error: Option<FailReason>,
}

impl Tray {
    /// `events`: os itens "Colar o último ditado" e "Tentar de novo" avisam o
    /// orquestrador por aqui. `config`: o atalho e os sons, para o menu nascer certo.
    pub fn new(
        app: &AppHandle,
        events: UnboundedSender<DictationEvent>,
        config: &Config,
    ) -> tauri::Result<Self> {
        let idle = Image::from_bytes(include_bytes!("../icons/tray/idle.png"))?;
        let recording = Image::from_bytes(include_bytes!("../icons/tray/recording.png"))?;
        let processing = Image::from_bytes(include_bytes!("../icons/tray/processing.png"))?;
        let done = Image::from_bytes(include_bytes!("../icons/tray/done.png"))?;

        let open = MenuItemBuilder::with_id(MENU_OPEN_SETTINGS, "Abrir o Ditok").build(app)?;
        let assistant =
            MenuItemBuilder::with_id(MENU_OPEN_ASSISTANT, "Abrir o assistente").build(app)?;
        // Desativado até haver um ditado para colar (o orquestrador ativa-o).
        let paste_last = MenuItemBuilder::with_id(MENU_PASTE_LAST, "Colar o último ditado")
            .enabled(false)
            .build(app)?;
        // Desativado até haver uma gravação falhada para reenviar.
        let retry = MenuItemBuilder::with_id(MENU_RETRY, "Tentar de novo o último ditado")
            .enabled(false)
            .build(app)?;
        // Só informativa: diz qual é o atalho (o texto muda com `sync_menu`).
        let shortcut = MenuItemBuilder::with_id(
            MENU_SHORTCUT,
            format!("Atalho: {}", hotkey::pretty(&config.shortcut)),
        )
        .enabled(false)
        .build(app)?;
        // Também só informativa; entra no menu em `sync_menu` se o modo estiver ligado.
        let command = MenuItemBuilder::with_id(MENU_COMMAND, "Comando")
            .enabled(false)
            .build(app)?;
        let mute = CheckMenuItemBuilder::with_id(MENU_MUTE, "Silenciar sons")
            .checked(!config.sounds)
            .build(app)?;
        let reset_overlay =
            MenuItemBuilder::with_id(MENU_RESET_OVERLAY, "Repor posição da barra").build(app)?;
        let notes_stop =
            MenuItemBuilder::with_id(MENU_NOTES_STOP, "● A gravar notas: parar").build(app)?;
        let quit = MenuItemBuilder::with_id(MENU_QUIT, "Sair").build(app)?;
        let menu = MenuBuilder::new(app)
            .item(&open)
            .item(&assistant)
            .item(&paste_last)
            .item(&retry)
            .separator()
            .item(&shortcut)
            .item(&mute)
            .item(&reset_overlay)
            .separator()
            .item(&quit)
            .build()?;

        let icon = TrayIconBuilder::with_id("main")
            .icon(idle.clone())
            .tooltip(TrayState::Idle.tooltip())
            .menu(&menu)
            // No Windows o clique esquerdo abre a janela e o menu fica no direito; no
            // macOS o habitual é o menu abrir com qualquer clique.
            .show_menu_on_left_click(cfg!(not(windows)))
            .on_tray_icon_event(|tray, event| match event {
                // Ainda antes do clique: a janela em primeiro plano é a tua.
                TrayIconEvent::Enter { .. } => remember_menu_target(),
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } if cfg!(windows) => crate::settings_window::open(tray.app_handle()),
                _ => {}
            })
            // Este handler é global: também recebe os cliques no menu aberto na
            // pílula. `move` dá ao closure a sua cópia do `Sender`.
            .on_menu_event(
                move |app: &AppHandle<Wry>, event| match event.id().as_ref() {
                    MENU_OPEN_SETTINGS => {
                        crate::settings_window::open(app);
                    }
                    MENU_OPEN_ASSISTANT => {
                        crate::assistant::window::open(app);
                    }
                    MENU_PASTE_LAST => {
                        tracing::info!("menu: colar o último ditado");
                        let _ = events.send(DictationEvent::PasteLast);
                    }
                    MENU_RETRY => {
                        tracing::info!("menu: tentar de novo");
                        let _ = events.send(DictationEvent::Retry);
                    }
                    MENU_MUTE => toggle_sounds(app),
                    MENU_RESET_OVERLAY => {
                        tracing::info!("menu: repor a posição da barra");
                        crate::overlay::reset_position(app);
                    }
                    MENU_NOTES_STOP => {
                        tracing::info!("menu: parar as notas");
                        if let Some(state) = app.try_state::<Arc<AppState>>() {
                            let state = Arc::clone(&state);
                            tauri::async_runtime::spawn(async move {
                                if let Err(e) = crate::notes::session::stop(&state).await {
                                    tracing::warn!(error = %e, "não foi possível parar as notas");
                                }
                            });
                        }
                    }
                    // Uma linha do histórico rápido (menu aberto por `quick_history::open`).
                    id if id.starts_with(crate::quick_history::ID_PREFIX) => {
                        crate::quick_history::chosen(
                            app,
                            &id[crate::quick_history::ID_PREFIX.len()..],
                        );
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

        app.manage(TrayMenu {
            menu,
            shortcut,
            command,
            command_shown: AtomicBool::new(false),
            mute,
            notes_stop,
            notes_shown: AtomicBool::new(false),
        });

        Ok(Self {
            app: app.clone(),
            icon,
            retry,
            paste_last,
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

    /// Ativa ou desativa o "Colar o último ditado".
    pub fn set_paste_last_available(&self, available: bool) {
        if let Err(e) = self.paste_last.set_enabled(available) {
            tracing::warn!(error = %e, "não foi possível mudar o menu do tray");
        }
    }

    pub fn set_tooltip(&self, text: &str) {
        if let Err(e) = self.icon.set_tooltip(Some(text)) {
            tracing::warn!(error = %e, "não foi possível mudar a tooltip do tray");
        }
    }
}
