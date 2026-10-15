//! A janela de seleção: um ecrã inteiro, opaco, por cima de tudo, com a fotografia do ecrã
//! lá dentro (`screen.html`). Criada a pedido e destruída ao fechar, como as outras, para o
//! WebView só gastar RAM enquanto a usas.
//!
//! Nasce escondida e só aparece quando a página diz que já desenhou a fotografia
//! (`reveal`, chamado por `screen_ready`): assim não há um relâmpago preto entre carregares no
//! atalho e veres o ecrã "congelado". Se a página nunca o disser, `start` desiste.
//!
//! Uma janela por cima de tudo que não consegue ser fechada seria um problema sério, por isso
//! há três saídas: Esc e ✕ na página, perder o foco (Alt+Tab, outra janela) e um tempo limite.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use tauri::{
    AppHandle, Manager, Monitor, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
    WindowEvent, window::Color,
};

pub const LABEL: &str = "screen";

/// Depois de perder o foco, espera-se um pouco antes de fechar: ao entrar em ecrã inteiro o
/// sistema pode tirar-lhe e devolver-lhe o foco num instante.
const FOCUS_GRACE: Duration = Duration::from_millis(300);

/// A janela já recebeu o foco alguma vez (a perda de foco só fecha depois disso).
static FOCUSED_ONCE: AtomicBool = AtomicBool::new(false);
/// A página já desenhou a fotografia e a janela foi mostrada.
static REVEALED: AtomicBool = AtomicBool::new(false);

pub fn is_open(app: &AppHandle) -> bool {
    app.get_webview_window(LABEL).is_some()
}

/// `true` quando a janela já foi mostrada ao utilizador.
pub fn is_revealed() -> bool {
    REVEALED.load(Ordering::Relaxed)
}

/// Cria a janela (escondida) no monitor `monitor`, para a captura número `session`.
pub fn open(app: &AppHandle, monitor: &Monitor, session: u64) -> Result<(), String> {
    FOCUSED_ONCE.store(false, Ordering::Relaxed);
    REVEALED.store(false, Ordering::Relaxed);
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("screen.html".into()))
        .title("Ditok")
        .decorations(false)
        .resizable(false)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        // Preto enquanto a página não desenha: o ecrã inteiro nunca pisca a branco.
        .background_color(Color(0, 0, 0, 255))
        // Atenção à ordem: queremos o foco, para o Esc e o teclado funcionarem logo.
        .focused(true)
        .additional_browser_args(crate::overlay::BROWSER_ARGS)
        .build()
        .map_err(|e| format!("não foi possível abrir a captura do ecrã: {e}"))?;
    // Escondida, põe-se já no monitor certo: é nele que o ecrã inteiro vai acontecer.
    let position = monitor.position();
    let size = monitor.size();
    let _ = window.set_position(PhysicalPosition::new(position.x, position.y));
    let _ = window.set_size(PhysicalSize::new(size.width, size.height));

    let handle = app.clone();
    window.on_window_event(move |event| match event {
        // Só conta o foco depois de a janela ser mostrada: escondida, o sistema dá-o e tira-o
        // (o foco passa da janela para o WebView) e isso fechava a captura antes de aparecer.
        WindowEvent::Focused(true) if REVEALED.load(Ordering::Relaxed) => {
            FOCUSED_ONCE.store(true, Ordering::Relaxed);
        }
        WindowEvent::Focused(false)
            if REVEALED.load(Ordering::Relaxed) && FOCUSED_ONCE.load(Ordering::Relaxed) =>
        {
            let app = handle.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(FOCUS_GRACE).await;
                let still_away = app
                    .get_webview_window(LABEL)
                    .and_then(|w| w.is_focused().ok())
                    .is_some_and(|focused| !focused);
                if still_away {
                    tracing::info!("a captura do ecrã perdeu o foco; a fechar");
                    super::cancel(&app);
                }
            });
        }
        WindowEvent::Destroyed => {
            super::forget(&handle, session);
            tracing::info!("janela da captura do ecrã destruída");
        }
        _ => {}
    });
    Ok(())
}

/// Mostra a janela em ecrã inteiro e dá-lhe o foco.
pub fn reveal(app: &AppHandle) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    // No macOS não abre um espaço novo (o ecrã inteiro "a sério" demorava ~1 s com uma
    // animação); no Windows é o ecrã inteiro sem moldura, que tapa a barra de tarefas.
    if let Err(e) = window.set_simple_fullscreen(true) {
        tracing::warn!(error = %e, "não foi possível pôr a captura em ecrã inteiro");
    }
    FOCUSED_ONCE.store(false, Ordering::Relaxed);
    REVEALED.store(true, Ordering::Relaxed);
    let _ = window.show();
    let _ = window.set_focus();
}

/// Fecha a janela. Sai do ecrã inteiro primeiro: no macOS isso devolve a barra de menus e o
/// Dock; escondê-la antes evita ver a janela encolher.
pub fn close(app: &AppHandle) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    let _ = window.hide();
    let _ = window.set_simple_fullscreen(false);
    if let Err(e) = window.destroy() {
        tracing::warn!(error = %e, "não foi possível fechar a captura do ecrã");
    }
}
