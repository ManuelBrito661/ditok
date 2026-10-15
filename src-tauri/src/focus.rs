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

/// Onde vai parar um ditado: a janela (para saber se ainda se pode colar) e o
/// nome da app dona dela (para escolher o estilo, ver `Config::style_for`).
#[derive(Debug, Clone, Default)]
pub struct Target {
    pub window: Option<Window>,
    /// Nome da app em minúsculas: o executável no Windows ("whatsapp.exe"), o
    /// nome do Dock no macOS ("whatsapp").
    pub app: Option<String>,
    /// Para onde o assistente devolve o foco, se pedires para pôr lá a resposta
    /// de um comando (ver `Origin`).
    pub origin: Option<Origin>,
}

impl Target {
    /// Junta à janela já capturada o nome da app dela (não volta a perguntar
    /// qual é a janela em primeiro plano).
    pub fn of(window: Option<Window>) -> Self {
        Self {
            window,
            app: window.and_then(app_name),
            origin: origin(window),
        }
    }
}

/// Para onde voltar mais tarde: a app onde estavas quando deste um comando, para o
/// assistente lhe devolver o foco e colar lá a resposta ("Substituir"). No Windows é a
/// janela; no macOS, a app em primeiro plano (pelo número do processo, porque as
/// janelas não se verificam aqui, ver o início deste ficheiro). Noutros sistemas não há.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin(imp::OriginId);

/// A app onde estás agora, ou `None` se for o próprio Ditok, a barra de tarefas ou se o
/// sistema não deixar saber. `window`: a janela já capturada ao começar o comando.
pub fn origin(window: Option<Window>) -> Option<Origin> {
    imp::origin(window).map(Origin)
}

/// Bloqueante. Volta à app de `origin` e diz se ela ficou em primeiro plano (pode ter
/// fechado, ou o sistema não deixar). Quem chama deve ter acabado de receber um clique
/// do utilizador: o Windows só deixa trocar de janela a quem o recebeu.
pub fn restore(origin: Origin) -> bool {
    imp::restore(origin.0)
}

/// A janela em primeiro plano agora. `None` se não houver nenhuma (acontece por
/// instantes, a meio de um Alt+Tab).
pub fn foreground() -> Option<Window> {
    imp::foreground()
}

/// O nome do executável dono da janela, em minúsculas e sem caminho
/// ("whatsapp.exe"). `None` se a janela já fechou ou o sistema não deixar ver o
/// processo (ex.: uma app a correr como administrador). No macOS é o nome da app
/// em primeiro plano, também em minúsculas ("whatsapp", "microsoft outlook").
pub fn app_name(w: Window) -> Option<String> {
    imp::app_name(w)
}

/// "C:\Program Files\WhatsApp\WhatsApp.exe" → "whatsapp.exe".
#[cfg_attr(not(windows), allow(dead_code))]
fn exe_name(path: &str) -> Option<String> {
    path.rsplit(['\\', '/'])
        .next()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_lowercase)
}

/// Como `foreground`, mas `None` se a janela for do próprio Ditok ou a barra de
/// tarefas. Usado no "Tentar de novo": se o pedires no menu do tray, quem fica em
/// primeiro plano é o tray (ou o Ditok), e colar aí não serve para nada.
pub fn foreground_for_retry() -> Option<Window> {
    foreground().filter(|w| !imp::is_ours_or_shell(*w))
}

/// Bloqueante. Volta a pôr `w` em primeiro plano (ex.: depois de usares o menu do
/// tray ou da barra, que "roubam" o foco) e diz se conseguiu. O Windows só deixa
/// fazê-lo a quem recebeu o último clique, que é o caso logo a seguir ao menu.
pub fn activate(w: Window) -> bool {
    if foreground() == Some(w) {
        return true;
    }
    imp::activate(w);
    // A troca de janela não é instantânea: dou-lhe um instante antes de confirmar.
    std::thread::sleep(std::time::Duration::from_millis(80));
    foreground() == Some(w)
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
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HWND, LPARAM},
        System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
        UI::WindowsAndMessaging::{
            EnumChildWindows, GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId,
            SetForegroundWindow,
        },
    };

    fn pid_of(hwnd: HWND) -> u32 {
        let mut pid = 0u32;
        // SAFETY: `pid` é uma variável local válida; um `hwnd` que entretanto
        // fechou não é perigoso, a função só devolve 0.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        pid
    }

    /// O nome do executável de um processo, pelo seu número.
    fn exe_of(pid: u32) -> Option<String> {
        if pid == 0 {
            return None;
        }
        // PROCESS_QUERY_LIMITED_INFORMATION é o mínimo para ler o caminho e
        // funciona mesmo com processos de outros utilizadores ou protegidos.
        // SAFETY: só pede uma "pega"; devolve NULL se não houver permissão.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: `buf` tem `len` posições; a função escreve lá o caminho e põe
        // em `len` quantas usou.
        let ok = unsafe {
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
        };
        // SAFETY: a pega é nossa e fecha-se uma só vez (senão ficava aberta).
        unsafe { CloseHandle(process) };
        if ok == 0 {
            return None;
        }
        super::exe_name(&String::from_utf16_lossy(&buf[..len as usize]))
    }

    pub fn app_name(w: Window) -> Option<String> {
        let hwnd = w.0 as HWND;
        let pid = pid_of(hwnd);
        let name = exe_of(pid)?;
        if name != "applicationframehost.exe" {
            return Some(name);
        }
        // As apps da Microsoft Store (UWP) vivem dentro de uma moldura que é de
        // outro processo; a app a sério é a dona de uma das janelas filhas.
        struct Search {
            frame: u32,
            found: u32,
        }
        // `extern "system"`: o Windows chama esta função por cada janela filha.
        // Devolver 0 pára a procura.
        unsafe extern "system" fn visit(child: HWND, data: LPARAM) -> i32 {
            // SAFETY: `data` é o `&mut Search` que passámos abaixo, vivo durante
            // toda a chamada a `EnumChildWindows`.
            let search = unsafe { &mut *(data as *mut Search) };
            let pid = pid_of(child);
            if pid != 0 && pid != search.frame {
                search.found = pid;
                return 0;
            }
            1
        }
        let mut search = Search {
            frame: pid,
            found: 0,
        };
        // SAFETY: `visit` só usa `search` enquanto `EnumChildWindows` corre.
        unsafe { EnumChildWindows(hwnd, Some(visit), &mut search as *mut Search as LPARAM) };
        exe_of(search.found).or(Some(name))
    }

    pub type OriginId = Window;

    pub fn origin(window: Option<Window>) -> Option<OriginId> {
        window.filter(|w| !is_ours_or_shell(*w))
    }

    pub fn restore(w: OriginId) -> bool {
        super::activate(w)
    }

    pub fn activate(w: Window) {
        // SAFETY: um `hwnd` que entretanto fechou não é perigoso; a função só
        // devolve 0 (falhou), e quem chama confirma com `foreground()`.
        unsafe { SetForegroundWindow(w.0 as windows_sys::Win32::Foundation::HWND) };
    }

    pub fn foreground() -> Option<Window> {
        // SAFETY: não recebe argumentos e só lê estado do sistema; devolve NULL se
        // não houver janela em primeiro plano.
        let hwnd = unsafe { GetForegroundWindow() };
        (!hwnd.is_null()).then_some(Window(hwnd as usize))
    }

    pub fn is_ours_or_shell(w: Window) -> bool {
        let hwnd = w.0 as HWND;
        if pid_of(hwnd) == std::process::id() {
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

    /// No macOS, o número do processo da app.
    #[cfg(target_os = "macos")]
    pub type OriginId = i32;
    #[cfg(not(target_os = "macos"))]
    pub type OriginId = std::convert::Infallible;

    /// A app em primeiro plano, a não ser que seja o próprio Ditok.
    #[cfg(target_os = "macos")]
    pub fn origin(_: Option<Window>) -> Option<OriginId> {
        use objc2_app_kit::NSWorkspace;
        let pid = NSWorkspace::sharedWorkspace()
            .frontmostApplication()?
            .processIdentifier();
        (u32::try_from(pid).ok()? != std::process::id()).then_some(pid)
    }

    /// Ativa a app e espera (até ~0,6 s) que ela fique em primeiro plano.
    #[cfg(target_os = "macos")]
    pub fn restore(pid: OriginId) -> bool {
        use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
        use std::time::Duration;
        let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
            return false;
        };
        if app.isTerminated() {
            return false;
        }
        // Depreciado no macOS 14 (o "ignoring" deixa de contar), mas é o que existe desde o
        // macOS 11, e a app que está ativa (esta, logo após um clique) pode ativar outra.
        #[allow(deprecated)]
        app.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(30));
            let front = NSWorkspace::sharedWorkspace()
                .frontmostApplication()
                .map(|a| a.processIdentifier());
            if front == Some(pid) {
                return true;
            }
        }
        false
    }

    /// Sem janelas a sério, não há como saber nem voltar a elas: o assistente deixa o
    /// texto no clipboard.
    #[cfg(not(target_os = "macos"))]
    pub fn origin(_: Option<Window>) -> Option<OriginId> {
        None
    }

    #[cfg(not(target_os = "macos"))]
    pub fn restore(never: OriginId) -> bool {
        match never {}
    }

    pub fn foreground() -> Option<Window> {
        Some(Window(0))
    }

    pub fn is_ours_or_shell(_: Window) -> bool {
        false
    }

    pub fn activate(_: Window) {}

    /// Sem janelas a sério (ver `foreground`), pergunta-se a app em primeiro
    /// plano agora. Quem chama fá-lo logo ao começar o ditado, por isso é a
    /// mesma. Devolve o nome que aparece no Dock ("WhatsApp", "Microsoft
    /// Outlook"), não o do executável: o do VS Code, por exemplo, é "Electron".
    #[cfg(target_os = "macos")]
    pub fn app_name(_: Window) -> Option<String> {
        use objc2_app_kit::NSWorkspace;
        let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        let name = app.localizedName()?.to_string();
        let name = name.trim().to_lowercase();
        (!name.is_empty()).then_some(name)
    }

    #[cfg(not(target_os = "macos"))]
    pub fn app_name(_: Window) -> Option<String> {
        None
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

    #[test]
    fn exe_name_keeps_only_the_file_name_in_lowercase() {
        assert_eq!(
            exe_name(r"C:\Program Files\WindowsApps\WhatsApp.exe").as_deref(),
            Some("whatsapp.exe")
        );
        assert_eq!(exe_name("/usr/bin/Code").as_deref(), Some("code"));
        assert_eq!(exe_name("pwsh.exe").as_deref(), Some("pwsh.exe"));
        assert_eq!(exe_name(r"C:\pasta\"), None);
        assert_eq!(exe_name(""), None);
    }

    /// No Windows, a janela em primeiro plano durante os testes é de alguém (o
    /// terminal, normalmente): o nome tem de ser um executável.
    #[cfg(windows)]
    #[test]
    fn app_name_of_the_foreground_window_looks_like_an_exe() {
        if let Some(name) = foreground().and_then(app_name) {
            assert!(name.ends_with(".exe"), "{name}");
            assert_eq!(name, name.to_lowercase());
        }
    }
}
