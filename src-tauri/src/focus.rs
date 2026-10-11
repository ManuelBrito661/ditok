//! Que janela está em primeiro plano. Guardamo-la ao começar o ditado e voltamos
//! a perguntar antes de colar: se mudaste de janela entretanto (ex.: foste ao
//! browser enquanto transcrevia), o Ctrl+V iria parar ao sítio errado, por isso o
//! texto fica só no clipboard.
//!
//! No Windows é o `HWND` devolvido por `GetForegroundWindow`. No macOS não
//! verificamos (precisaria de APIs do AppKit): a janela conta sempre como "a
//! mesma", que é o comportamento de antes.

/// Identifica uma janela. No Windows guarda o `HWND` como número: um ponteiro
/// cru (`*mut c_void`) não é `Send`, e só o queremos comparar, nunca usar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// No macOS o número nunca é lido (não há verificação), daí o `allow`.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Window(usize);

/// A janela em primeiro plano agora. `None` se não houver nenhuma (acontece por
/// instantes, a meio de um Alt+Tab).
pub fn foreground() -> Option<Window> {
    imp::foreground()
}

/// Como `foreground`, mas `None` se a janela for do próprio Ditok ou a barra de
/// tarefas. Usado no "Tentar de novo": se o pedires no menu do tray, quem fica em
/// primeiro plano é o tray (ou o Ditok), e colar aí não serve para nada.
pub fn foreground_for_retry() -> Option<Window> {
    foreground().filter(|w| !imp::is_ours_or_shell(*w))
}

/// Decide se ainda se pode colar. Sem janela de destino (`start` = `None`) ou sem
/// janela agora, não se cola: o texto fica no clipboard e o utilizador cola onde
/// quiser.
pub fn should_paste(start: Option<Window>, now: Option<Window>) -> bool {
    matches!((start, now), (Some(a), Some(b)) if a == b)
}

#[cfg(windows)]
mod imp {
    use super::Window;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId,
    };

    pub fn foreground() -> Option<Window> {
        // SAFETY: não recebe argumentos e só lê estado do sistema; devolve NULL se
        // não houver janela em primeiro plano.
        let hwnd = unsafe { GetForegroundWindow() };
        (!hwnd.is_null()).then_some(Window(hwnd as usize))
    }

    pub fn is_ours_or_shell(w: Window) -> bool {
        let hwnd = w.0 as windows_sys::Win32::Foundation::HWND;
        let mut pid = 0u32;
        // SAFETY: `pid` é uma variável local válida; um `hwnd` que entretanto
        // fechou não é perigoso, a função só devolve 0.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid == std::process::id() {
            return true;
        }
        let mut class = [0u16; 64];
        // SAFETY: o buffer tem o tamanho que passamos em `nmaxcount`.
        let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
        let class = String::from_utf16_lossy(&class[..len.max(0) as usize]);
        // Barra de tarefas (principal e dos outros monitores) e ambiente de trabalho.
        matches!(
            class.as_str(),
            "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW"
        )
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Window;

    pub fn foreground() -> Option<Window> {
        Some(Window(0))
    }

    pub fn is_ours_or_shell(_: Window) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pastes_only_into_the_same_window() {
        let (a, b) = (Window(1), Window(2));
        assert!(should_paste(Some(a), Some(a)));
        assert!(!should_paste(Some(a), Some(b)));
        assert!(!should_paste(Some(a), None));
        assert!(!should_paste(None, Some(a)));
        assert!(!should_paste(None, None));
    }
}
