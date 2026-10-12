//! Windows: um hook de teclado de baixo nível (`WH_KEYBOARD_LL`) numa thread
//! com um ciclo de mensagens. O callback tem de ser rápido (o Windows desliga o
//! hook se demorar), por isso só atualiza o estado e envia um evento.

use std::{cell::Cell, ptr::null_mut, sync::Mutex};

use windows_sys::Win32::{
    Foundation::{GetLastError, LPARAM, LRESULT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL,
            VK_RMENU, VK_RSHIFT, VK_RWIN,
        },
        WindowsAndMessaging::{
            CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, SetWindowsHookExW,
            UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
        },
    },
};

use super::{Mods, on_mods, on_other_key};

/// As teclas que contam, esquerda e direita à parte. A posição de cada uma é o
/// seu bit em `KEYS`; as duas Win são as últimas.
const TRACKED: [(u16, Mods); 8] = [
    (VK_LCONTROL, Mods::CTRL),
    (VK_RCONTROL, Mods::CTRL),
    (VK_LSHIFT, Mods::SHIFT),
    (VK_RSHIFT, Mods::SHIFT),
    (VK_LMENU, Mods::ALT),
    (VK_RMENU, Mods::ALT),
    (VK_LWIN, Mods::SUPER),
    (VK_RWIN, Mods::SUPER),
];
const FIRST_WIN: usize = 6;

thread_local! {
    // O hook corre sempre na thread que o instalou, por isso isto não precisa de locks.
    /// Bit `i` = a tecla `TRACKED[i]` está premida.
    static KEYS: Cell<u8> = const { Cell::new(0) };
    /// A tecla Win que completou o atalho foi engolida: engole também as
    /// repetições e o "largar", para o Windows nunca ver a Win (e não abrir o
    /// menu Iniciar quando o utilizador larga).
    static SWALLOWING_WIN: Cell<bool> = const { Cell::new(false) };
}

static STARTED: Mutex<bool> = Mutex::new(false);

pub fn ensure_started() -> Result<(), String> {
    let mut started = STARTED.lock().unwrap_or_else(|e| e.into_inner());
    if *started {
        return Ok(());
    }
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("ditok-modkeys".into())
        .spawn(move || {
            // SAFETY: `hook_proc` tem a assinatura certa e vive até ao fim do
            // programa; sem `hmod` basta para hooks de baixo nível.
            let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), null_mut(), 0) };
            if hook.is_null() {
                // SAFETY: só lê o último erro desta thread.
                let code = unsafe { GetLastError() };
                let _ = ready_tx.send(Err(format!(
                    "o Windows não deixou ouvir o teclado (erro {code})"
                )));
                return;
            }
            let _ = ready_tx.send(Ok(()));
            // O hook só recebe teclas enquanto a thread bombeia mensagens.
            // SAFETY: `MSG` aceita tudo a zeros; `GetMessageW` escreve nele.
            let mut msg: MSG = unsafe { std::mem::zeroed() };
            while unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } > 0 {}
            // SAFETY: `hook` é o que instalámos acima.
            unsafe { UnhookWindowsHookEx(hook) };
        })
        .map_err(|e| format!("não foi possível iniciar a thread do teclado: {e}"))?;
    ready_rx
        .recv()
        .map_err(|_| "a thread do teclado terminou antes de arrancar".to_owned())??;
    *started = true;
    tracing::info!("a ouvir modificadores (hook de teclado)");
    Ok(())
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // `code < 0`: o Windows pede para não tratar o evento.
    if code >= 0 && handle(wparam as u32, lparam) {
        return 1; // engolido: nenhuma app o vê
    }
    // SAFETY: passa o evento ao próximo hook, como manda a documentação.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

/// `true` se o evento deve ser engolido.
fn handle(message: u32, lparam: LPARAM) -> bool {
    // SAFETY: para `WH_KEYBOARD_LL` com `code >= 0`, `lparam` aponta para uma
    // `KBDLLHOOKSTRUCT` válida durante o callback.
    let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
    // Teclas simuladas (a colagem do próprio Ditok, outras apps) não contam.
    if info.flags & LLKHF_INJECTED != 0 {
        return false;
    }
    let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
    if !down && !matches!(message, WM_KEYUP | WM_SYSKEYUP) {
        return false;
    }
    let vk = info.vkCode as u16;
    let Some(index) = TRACKED.iter().position(|&(k, _)| k == vk) else {
        if down {
            on_other_key(vk == VK_ESCAPE);
        }
        return false;
    };
    let is_win = index >= FIRST_WIN;
    let bit = 1u8 << index;
    let mut keys = KEYS.get();
    let was_down = keys & bit != 0;
    keys = if down { keys | bit } else { keys & !bit };
    // Um "largar" que nunca chegou (Win+L, Ctrl+Alt+Del) deixaria a tecla presa
    // no nosso estado: confirma as outras com o sistema.
    for (i, &(k, _)) in TRACKED.iter().enumerate() {
        // SAFETY: só lê o estado do teclado.
        if i != index && keys & (1 << i) != 0 && unsafe { GetAsyncKeyState(i32::from(k)) } >= 0 {
            keys &= !(1 << i);
        }
    }
    KEYS.set(keys);

    if is_win && down && !was_down {
        SWALLOWING_WIN.set(false); // uma Win nova: o que sobrou de antes já não vale
    }
    let swallow_now = is_win && SWALLOWING_WIN.get();
    if swallow_now && !down {
        SWALLOWING_WIN.set(false);
    }

    let held = TRACKED
        .iter()
        .enumerate()
        .filter(|&(i, _)| keys & (1 << i) != 0)
        .fold(Mods::NONE, |acc, (_, &(_, m))| acc | m);
    let completed = on_mods(held);
    if is_win && down && completed.contains(Mods::SUPER) {
        SWALLOWING_WIN.set(true);
        return true;
    }
    swallow_now
}
