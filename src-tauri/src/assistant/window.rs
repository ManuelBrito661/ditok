//! A janela do assistente: pequena, com a sua própria página (`assistant.html`), criada
//! a pedido e destruída ao fechar, como a janela principal, para o WebView só gastar
//! RAM enquanto a usas. A conversa não vive nela mas no backend (`Assistant`), por
//! isso fechar e abrir outra vez continua onde estavas.

use std::time::Instant;

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

pub const LABEL: &str = "assistant";

/// Maior do que um telemóvel, mais pequena do que um ecrã: cabe ao lado da app onde
/// estás a escrever.
const WIDTH: f64 = 440.0;
const HEIGHT: f64 = 660.0;
/// Abaixo disto o compositor e o histórico já não cabem.
const MIN_WIDTH: f64 = 340.0;
const MIN_HEIGHT: f64 = 420.0;

/// Abre a janela, ou traz para a frente a que já está aberta.
pub fn open(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }

    let started = Instant::now();
    let builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("assistant.html".into()))
        .title("Ditok")
        .inner_size(WIDTH, HEIGHT)
        .min_inner_size(MIN_WIDTH, MIN_HEIGHT)
        .center()
        .focused(true)
        .additional_browser_args(crate::overlay::BROWSER_ARGS);
    // No macOS a barra de título some e os três botões ficam por cima da página
    // (a página deixa-lhes espaço, ver `html[data-platform="mac"]` em assistant.css).
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);

    match builder.build() {
        Ok(window) => {
            tracing::info!(
                elapsed_ms = started.elapsed().as_millis() as u64,
                "janela do assistente aberta"
            );
            window.on_window_event(|event| {
                if let WindowEvent::Destroyed = event {
                    tracing::info!("janela do assistente destruída");
                }
            });
        }
        Err(e) => tracing::error!(error = %e, "não foi possível abrir a janela do assistente"),
    }
}

/// Fecha a janela (a conversa continua no backend).
pub fn close(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.close();
    }
}

/// A janela está aberta e é a que recebe o teclado? Serve ao modo comando: com ela em
/// primeiro plano, o que dizes continua a conversa em vez de começar outra.
pub fn is_focused(app: &AppHandle) -> bool {
    app.get_webview_window(LABEL)
        .and_then(|window| window.is_focused().ok())
        .unwrap_or(false)
}
