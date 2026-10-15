//! A janela do assistente: um popup sem moldura, de cantos arredondados e fundo
//! translúcido, com a sua própria página (`assistant.html`). Criada a pedido e
//! destruída ao fechar, como a janela principal, para o WebView só gastar RAM enquanto
//! a usas. A conversa não vive nela mas no backend (`Assistant`), por isso fechar e
//! abrir outra vez continua onde estavas.
//!
//! A janela é transparente e maior do que o painel que se vê: a página desenha o painel
//! (cantos, borda, fundo translúcido) e deixa à volta uma margem transparente, onde cai a
//! sombra (`--margin` em assistant.css). É a mesma técnica da barra da mascote
//! (`overlay.rs`), que já provou funcionar em Windows e macOS. Sem moldura, a página
//! trata de arrastar (a barra de cima) e de fechar (o ✕ e o Esc).

use std::time::Instant;

use tauri::{
    AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    WindowEvent,
};

pub const LABEL: &str = "assistant";

/// O painel que se vê, em píxeis lógicos.
const PANEL_WIDTH: f64 = 420.0;
const PANEL_HEIGHT: f64 = 650.0;
/// A margem transparente à volta do painel, para a sombra. Tem de ser igual a `--margin`
/// em assistant.css.
const MARGIN: f64 = 24.0;
const WIDTH: f64 = PANEL_WIDTH + 2.0 * MARGIN;
const HEIGHT: f64 = PANEL_HEIGHT + 2.0 * MARGIN;
/// Distância do painel (não da janela) ao canto direito do ecrã.
const EDGE: f64 = 16.0;

/// Abre o popup, ou traz para a frente o que já está aberto.
pub fn open(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }

    let started = Instant::now();
    let built = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("assistant.html".into()))
        .title("Ditok")
        .inner_size(WIDTH, HEIGHT)
        .decorations(false)
        .resizable(false)
        // A sombra é desenhada pela página, dentro da margem transparente.
        .shadow(false)
        .transparent(true)
        // Um popup fica por cima da app onde estás a escrever e não ocupa um lugar na barra
        // de tarefas. Nasce escondido para ser posto no sítio certo antes de aparecer.
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        // Atenção à ordem: `transparent(true)` liga `focused(true)` por dentro; aqui
        // queremos o foco, para escreveres logo e o Esc fechar.
        .focused(true)
        .additional_browser_args(crate::overlay::BROWSER_ARGS)
        .build();

    match built {
        Ok(window) => {
            place(&window);
            let _ = window.show();
            let _ = window.set_focus();
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

/// Põe o popup junto ao canto direito do monitor onde está o cursor, a meio da altura:
/// o texto que estás a editar costuma estar mais ao centro, e assim não fica tapado.
fn place(window: &WebviewWindow) {
    let monitor = window
        .cursor_position()
        .ok()
        .and_then(|p| window.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        // Sem saber onde, fica onde o sistema o pôs.
        return;
    };
    // Contas em píxeis físicos com a escala do monitor de destino. O tamanho não se
    // pergunta à janela: escondida, ainda não o tem (`outer_size` dá 0×0 em Linux).
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let (width, height) = (WIDTH * scale, HEIGHT * scale);
    let right = f64::from(area.position.x) + f64::from(area.size.width);
    let top = f64::from(area.position.y);
    let x = right - width + (MARGIN - EDGE) * scale;
    // Nunca começa acima do ecrã, mesmo num monitor mais baixo do que o popup.
    let y = top + ((f64::from(area.size.height) - height) / 2.0).max(0.0);
    if let Err(e) = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32)) {
        tracing::debug!(error = %e, "não foi possível posicionar o assistente");
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
