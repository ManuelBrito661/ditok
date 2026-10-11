//! Barra flutuante com a mascote (estilo "Flow Bar" do Wispr Flow): sempre visível,
//! minúscula em repouso, cresce durante o ditado e mostra a ouvir (com o nível do
//! microfone) → a processar → pronto.
//!
//! É uma janela WebView criada no arranque (por isso aparece instantaneamente) que:
//! - nunca recebe foco: senão o Ctrl+V colava dentro dela em vez de na tua app;
//! - deixa os cliques passar através dela (`set_ignore_cursor_events`);
//! - é transparente, sem moldura, por cima de tudo e fora da barra de tarefas.
//!
//! O backend só lhe envia eventos (`overlay:state`, `overlay:level`); toda a
//! animação é CSS na página `overlay.html`.

use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

use crate::tray::TrayState;

pub const LABEL: &str = "overlay";
/// Tamanho lógico da janela: cabe a pílula no maior estado (a ouvir). Fora da
/// pílula é transparente e os cliques passam.
const WIDTH: f64 = 260.0;
const HEIGHT: f64 = 72.0;
/// Argumentos do WebView2 (Windows) para TODAS as janelas da app: janelas com
/// argumentos diferentes teriam de usar pastas de dados diferentes.
/// - Os três `--disable-features` são os que o Tauri passa por omissão (ao
///   definirmos os nossos, temos de os repetir).
/// - `--disable-gpu`: desenha por software. Poupa o processo de GPU do WebView2
///   (~65 MB medidos), e uma pílula com animações CSS simples não precisa dele.
pub const BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-gpu --disable-gpu-compositing";

/// Distância à barra de tarefas / fundo do ecrã.
const MARGIN: f64 = 10.0;

pub fn create(app: &AppHandle) {
    if app.get_webview_window(LABEL).is_some() {
        return;
    }
    let built = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("overlay.html".into()))
        .title("Ditok")
        .inner_size(WIDTH, HEIGHT)
        .decorations(false)
        .resizable(false)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible_on_all_workspaces(true)
        .transparent(true)
        // Atenção à ordem: `transparent(true)` liga `focused(true)` por dentro, por
        // isso estas duas têm de vir depois.
        .focused(false)
        .focusable(false)
        .additional_browser_args(BROWSER_ARGS)
        .build();

    match built {
        Ok(window) => {
            if let Err(e) = window.set_ignore_cursor_events(true) {
                tracing::warn!(error = %e, "a barra não deixa passar os cliques");
            }
            reposition(&window);
            tracing::info!("barra flutuante criada");
        }
        Err(e) => tracing::error!(error = %e, "não foi possível criar a barra flutuante"),
    }
}

pub fn destroy(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL)
        && let Err(e) = window.destroy()
    {
        tracing::warn!(error = %e, "não foi possível fechar a barra flutuante");
    }
}

/// Coloca a barra em baixo ao centro do monitor onde está o cursor, por cima da
/// barra de tarefas (a "work area" exclui-a). Chamado ao criar e a cada ditado,
/// para seguir o monitor onde estás a trabalhar.
pub fn reposition_for(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        reposition(&window);
    }
}

fn reposition(window: &WebviewWindow) {
    let monitor = window
        .cursor_position()
        .ok()
        .and_then(|p| window.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else { return };

    // Contas em píxeis físicos com a escala do monitor de destino (um portátil a
    // 150% e um monitor externo a 100% têm escalas diferentes).
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let (w, h, margin) = (WIDTH * scale, HEIGHT * scale, MARGIN * scale);
    let x = f64::from(area.position.x) + (f64::from(area.size.width) - w) / 2.0;
    let y = f64::from(area.position.y) + f64::from(area.size.height) - h - margin;
    if let Err(e) = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32)) {
        tracing::debug!(error = %e, "não foi possível posicionar a barra");
    }
}

/// Envia o estado à barra (se existir). Barato: um evento para uma janela.
pub fn emit_state(app: &AppHandle, state: TrayState) {
    let _ = app.emit_to(LABEL, "overlay:state", state);
}

/// Nível do microfone (0–1), ~20 vezes por segundo durante a gravação.
pub fn emit_level(app: &AppHandle, level: f32) {
    let _ = app.emit_to(LABEL, "overlay:level", level);
}
