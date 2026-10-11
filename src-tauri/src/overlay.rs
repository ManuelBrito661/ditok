//! Barra flutuante com a mascote (estilo "Flow Bar" do Wispr Flow): sempre visível,
//! minúscula em repouso, cresce durante o ditado e mostra a ouvir (com o nível do
//! microfone) → a processar → pronto.
//!
//! É uma janela WebView criada no arranque (por isso aparece instantaneamente) que:
//! - nunca recebe foco: senão o Ctrl+V colava dentro dela em vez de na tua app;
//! - deixa os cliques passar através dela (`set_ignore_cursor_events`), exceto
//!   quando o rato está mesmo por cima da pílula (ver `watch_hover`);
//! - é transparente, sem moldura, por cima de tudo e fora da barra de tarefas.
//!
//! O backend envia-lhe eventos (`overlay:state`, `overlay:level`, `overlay:latched`,
//! `overlay:failure`, `overlay:busy`, `overlay:hover`, `overlay:notice`) e recebe os cliques pelos
//! comandos `overlay_*` em commands.rs.
//! Toda a animação é CSS na página `overlay.html`.

use std::{
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::Deserialize;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

use crate::tray::TrayState;

pub const LABEL: &str = "overlay";
/// Tamanho lógico da janela: cabe a pílula no maior estado (a ouvir, com os botões)
/// e a dica que aparece por cima dela. Fora da pílula é transparente e os cliques
/// passam.
const WIDTH: f64 = 340.0;
const HEIGHT: f64 = 120.0;
/// Argumentos do WebView2 (Windows) para TODAS as janelas da app: janelas com
/// argumentos diferentes teriam de usar pastas de dados diferentes.
/// - Os três `--disable-features` são os que o Tauri passa por omissão (ao
///   definirmos os nossos, temos de os repetir).
/// - `--disable-gpu`: desenha por software. Poupa o processo de GPU do WebView2
///   (~65 MB medidos), e uma pílula com animações CSS simples não precisa dele.
pub const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-gpu --disable-gpu-compositing";

/// Distância à barra de tarefas / fundo do ecrã.
const MARGIN: f64 = 10.0;
/// De quanto em quanto tempo vemos se o rato está por cima da pílula. 60 ms chega
/// para a dica parecer imediata e custa uma leitura da posição do cursor.
const HOVER_POLL: Duration = Duration::from_millis(60);
/// Margem à volta da pílula que ainda conta como "por cima" (a pílula em repouso
/// tem poucos píxeis de altura).
const HOVER_PAD: f64 = 6.0;

/// Retângulo da pílula em píxeis lógicos, relativo à janela. Enviado pela página
/// sempre que a pílula muda de tamanho.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct HitRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Partilhado entre o comando (thread do Tauri) e a thread que vigia o rato:
/// por isso o `Mutex`. É só escrito quando a pílula muda de tamanho.
static HIT_RECT: Mutex<Option<HitRect>> = Mutex::new(None);
/// Cada barra criada recebe um número novo; a thread de uma barra antiga vê que o
/// número mudou e termina (evita duas threads se desligares e voltares a ligar).
static GENERATION: AtomicU64 = AtomicU64::new(0);

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
            watch_hover(app.clone());
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

/// A página diz onde está a pílula (muda de tamanho com o estado).
pub fn set_hit_rect(rect: HitRect) {
    if let Ok(mut slot) = HIT_RECT.lock() {
        *slot = Some(rect);
    }
}

/// Uma janela ou deixa passar TODOS os cliques ou nenhum; não há "só nesta zona".
/// Por isso uma thread pequena vê onde está o rato e só desliga o "deixar passar"
/// enquanto ele está por cima da pílula. Assim o resto da janela transparente
/// nunca bloqueia cliques nas apps por baixo.
fn watch_hover(app: AppHandle) {
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let spawned = std::thread::Builder::new()
        .name("overlay-hover".into())
        .spawn(move || {
            let mut hovering = false;
            loop {
                std::thread::sleep(HOVER_POLL);
                if GENERATION.load(Ordering::Relaxed) != generation {
                    break;
                }
                let Some(window) = app.get_webview_window(LABEL) else {
                    break; // a barra foi desligada
                };
                let inside = cursor_over_pill(&app, &window).unwrap_or(false);
                if inside != hovering {
                    hovering = inside;
                    if let Err(e) = window.set_ignore_cursor_events(!inside) {
                        tracing::debug!(error = %e, "não foi possível mudar os cliques da barra");
                    }
                    let _ = app.emit_to(LABEL, "overlay:hover", inside);
                }
            }
            tracing::debug!("vigia do rato da barra terminada");
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "sem vigia do rato: a barra não reage ao rato");
    }
}

/// `None` quando ainda não sabemos onde está a pílula ou o sistema não responde.
fn cursor_over_pill(app: &AppHandle, window: &WebviewWindow) -> Option<bool> {
    let rect = (*HIT_RECT.lock().ok()?)?;
    let cursor = app.cursor_position().ok()?;
    let origin = window.inner_position().ok()?;
    let scale = window.scale_factor().ok()?;
    // Cursor em píxeis físicos do ecrã → píxeis lógicos relativos à janela.
    let x = (cursor.x - f64::from(origin.x)) / scale;
    let y = (cursor.y - f64::from(origin.y)) / scale;
    Some(
        x >= rect.x - HOVER_PAD
            && x <= rect.x + rect.width + HOVER_PAD
            && y >= rect.y - HOVER_PAD
            && y <= rect.y + rect.height + HOVER_PAD,
    )
}

/// Envia o estado à barra (se existir). Barato: um evento para uma janela.
pub fn emit_state(app: &AppHandle, state: TrayState) {
    let _ = app.emit_to(LABEL, "overlay:state", state);
}

/// O ditado ficou em modo alternar: a barra mostra os botões ✕ e ■.
pub fn emit_latched(app: &AppHandle) {
    let _ = app.emit_to(LABEL, "overlay:latched", ());
}

/// O estilo deste ditado ("Formal"…), ou `None` se for o Predefinido. Enviado
/// antes do estado `recording`; a barra mostra-o enquanto ouve.
pub fn emit_style(app: &AppHandle, label: Option<&str>) {
    let _ = app.emit_to(LABEL, "overlay:style", label);
}

/// Nível do microfone (0–1), ~20 vezes por segundo durante a gravação.
pub fn emit_level(app: &AppHandle, level: f32) {
    let _ = app.emit_to(LABEL, "overlay:level", level);
}

/// Motivo curto de uma falha ("Sem rede"…) e se há "Tentar de novo". Enviado
/// antes do estado `failed`.
#[derive(Clone, serde::Serialize)]
struct Failure<'a> {
    label: &'a str,
    retry: bool,
}

pub fn emit_failure(app: &AppHandle, label: &str, retry: bool) {
    let _ = app.emit_to(LABEL, "overlay:failure", Failure { label, retry });
}

/// Premiste o atalho a meio do processamento: a pílula abana.
pub fn emit_busy(app: &AppHandle) {
    let _ = app.emit_to(LABEL, "overlay:busy", ());
}

/// Aviso persistente da pílula em repouso ("Falta a chave", "Atalho ocupado").
/// A barra também o pede ao carregar (`overlay_status`): pode ainda não existir
/// quando isto é enviado.
pub fn emit_notice(app: &AppHandle, status: &crate::state::Status) {
    let _ = app.emit_to(LABEL, "overlay:notice", status);
}

/// Clique direito na pílula: o mesmo menu do tray, por cima dela.
pub fn popup_menu(app: &AppHandle) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    let Some(menu) = crate::tray::menu(app) else {
        return;
    };
    if let Err(e) = window.popup_menu(&menu) {
        tracing::warn!(error = %e, "não foi possível abrir o menu da barra");
    }
}
