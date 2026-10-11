//! Janela de definições, criada a pedido a partir do tray e destruída ao fechar,
//! para que o WebView (de longe o que mais RAM gasta) só exista enquanto a usas.

use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::sysmem;

pub const LABEL: &str = "settings";

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
        .title("Ditok — Definições")
        .inner_size(820.0, 660.0)
        .min_inner_size(640.0, 480.0)
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
            // lib.rs impede que isso termine a app. Aqui só medimos.
            window.on_window_event(|event| {
                if let WindowEvent::Destroyed = event {
                    tracing::info!("janela de definições destruída");
                    sysmem::log_after(Duration::from_secs(3), "depois de fechar a janela");
                }
            });
            sysmem::log_after(Duration::from_secs(3), "com a janela aberta");
        }
        Err(e) => tracing::error!(error = %e, "não foi possível abrir a janela de definições"),
    }
}
