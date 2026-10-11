//! Janela principal (Início, Métricas e o modal de definições), criada a pedido a partir do tray e destruída ao fechar,
//! para que o WebView (de longe o que mais RAM gasta) só exista enquanto a usas.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::sysmem;

pub const LABEL: &str = "settings";
/// Avisa a janela (já aberta) de que há um sítio pedido para abrir.
const EVENT_OPEN: &str = "main:open";

/// Onde a janela deve abrir ("key": a chave; "shortcut": o atalho), até a
/// página o ir buscar (`main_take_open`). Guardado aqui porque uma janela acabada
/// de criar ainda não está a ouvir eventos.
static PENDING: Mutex<Option<String>> = Mutex::new(None);

/// A página pede o sítio pendente (uma só vez: depois fica `None`).
pub fn take_pending() -> Option<String> {
    PENDING.lock().ok().and_then(|mut p| p.take())
}

/// Abre a janela e leva-a a um sítio: ao guia ou às definições da chave
/// (`"key"`), ou ao separador do atalho (`"shortcut"`).
pub fn open_at(app: &AppHandle, target: &str) {
    if let Ok(mut p) = PENDING.lock() {
        *p = Some(target.to_owned());
    }
    open(app);
    // Se a janela já existia, a página está a ouvir e vai buscar o sítio pendente.
    let _ = app.emit_to(LABEL, EVENT_OPEN, ());
}

pub fn open(app: &AppHandle) {
    // Já aberta: traz para a frente em vez de criar outra.
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }

    let started = Instant::now();
    let built = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title("Ditok")
        .inner_size(1040.0, 720.0)
        // Abaixo disto a barra lateral e o modal de definições já não cabem.
        .min_inner_size(860.0, 600.0)
        .center()
        .additional_browser_args(crate::overlay::BROWSER_ARGS)
        .build();

    match built {
        Ok(window) => {
            tracing::info!(
                elapsed_ms = started.elapsed().as_millis() as u64,
                "janela de definições aberta"
            );
            // O fecho por omissão destrói a janela (e o WebView). O `prevent_exit` em
            // lib.rs impede que isso termine a app.
            let handle = app.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::Destroyed = event {
                    tracing::info!("janela de definições destruída");
                    // Se fechaste a janela no passo do microfone do guia, o medidor
                    // não fica a ouvir (também pararia sozinho, ao fim de 6 s).
                    if let Some(recorder) = handle.try_state::<crate::audio::Recorder>() {
                        recorder.monitor_stop();
                    }
                    sysmem::log_after(Duration::from_secs(3), "depois de fechar a janela");
                }
            });
            sysmem::log_after(Duration::from_secs(3), "com a janela aberta");
        }
        Err(e) => tracing::error!(error = %e, "não foi possível abrir a janela de definições"),
    }
}
