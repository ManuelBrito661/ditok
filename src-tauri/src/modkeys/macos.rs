//! macOS: um "event tap" só de escuta (CoreGraphics) numa thread com o seu
//! próprio ciclo de eventos. É a única forma de ver a tecla Fn, que não gera
//! eventos de teclado normais, só uma mudança nos modificadores.
//!
//! Precisa da permissão "Monitorização de entrada" (Definições do Sistema →
//! Privacidade e Segurança). Sem ela o `CGEventTapCreate` devolve nulo.

use std::{
    ffi::c_void,
    ptr::null_mut,
    sync::{
        Mutex,
        atomic::{AtomicPtr, Ordering},
    },
};

use super::{Mods, on_mods, on_other_key};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: extern "C" fn(*mut c_void, u32, *mut c_void, *mut c_void) -> *mut c_void,
        user_info: *mut c_void,
    ) -> *mut c_void;
    fn CGEventTapEnable(tap: *mut c_void, enable: bool);
    fn CGEventGetFlags(event: *mut c_void) -> u64;
    fn CGEventGetIntegerValueField(event: *mut c_void, field: u32) -> i64;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: *const c_void;
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: *mut c_void,
        order: isize,
    ) -> *mut c_void;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopAddSource(run_loop: *mut c_void, source: *mut c_void, mode: *const c_void);
    fn CFRunLoopRun();
}

const SESSION_EVENT_TAP: u32 = 1; // kCGSessionEventTap
const HEAD_INSERT: u32 = 0; // kCGHeadInsertEventTap
const LISTEN_ONLY: u32 = 1; // kCGEventTapOptionListenOnly

const KEY_DOWN: u32 = 10;
const FLAGS_CHANGED: u32 = 12;
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

const FIELD_KEYCODE: u32 = 9; // kCGKeyboardEventKeycode
const KEYCODE_ESCAPE: i64 = 53;

// Bits de `CGEventFlags`.
const FLAG_SHIFT: u64 = 0x0002_0000;
const FLAG_CONTROL: u64 = 0x0004_0000;
const FLAG_ALTERNATE: u64 = 0x0008_0000;
const FLAG_COMMAND: u64 = 0x0010_0000;
const FLAG_FN: u64 = 0x0080_0000;

/// O tap, para o voltar a ligar se o sistema o desligar.
static TAP: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
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
            let mask = (1u64 << KEY_DOWN) | (1u64 << FLAGS_CHANGED);
            // SAFETY: `callback` tem a assinatura que o CoreGraphics espera e o
            // `user_info` não é usado.
            let tap = unsafe {
                CGEventTapCreate(
                    SESSION_EVENT_TAP,
                    HEAD_INSERT,
                    LISTEN_ONLY,
                    mask,
                    callback,
                    null_mut(),
                )
            };
            if tap.is_null() {
                let _ = ready_tx.send(Err(
                    "falta a permissão \"Monitorização de entrada\" (Definições do Sistema → \
                     Privacidade e Segurança). Dá-a ao Ditok e reinicia-o"
                        .into(),
                ));
                return;
            }
            TAP.store(tap, Ordering::Release);
            // SAFETY: `tap` é válido; a fonte e o run loop são desta thread.
            unsafe {
                let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
                CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
                CGEventTapEnable(tap, true);
            }
            let _ = ready_tx.send(Ok(()));
            // SAFETY: só corre o ciclo de eventos desta thread (não volta).
            unsafe { CFRunLoopRun() };
        })
        .map_err(|e| format!("não foi possível iniciar a thread do teclado: {e}"))?;
    ready_rx
        .recv()
        .map_err(|_| "a thread do teclado terminou antes de arrancar".to_owned())??;
    *started = true;
    tracing::info!("a ouvir modificadores (event tap)");
    Ok(())
}

extern "C" fn callback(
    _proxy: *mut c_void,
    kind: u32,
    event: *mut c_void,
    _user_info: *mut c_void,
) -> *mut c_void {
    match kind {
        FLAGS_CHANGED => {
            // SAFETY: `event` é válido durante o callback.
            let flags = unsafe { CGEventGetFlags(event) };
            on_mods(mods_from(flags));
        }
        KEY_DOWN => {
            // SAFETY: idem.
            let key = unsafe { CGEventGetIntegerValueField(event, FIELD_KEYCODE) };
            on_other_key(key == KEYCODE_ESCAPE);
        }
        // O sistema desliga o tap se o callback demorar, ou por razões de
        // segurança: voltamos a ligá-lo.
        TAP_DISABLED_BY_TIMEOUT | TAP_DISABLED_BY_USER_INPUT => {
            let tap = TAP.load(Ordering::Acquire);
            if !tap.is_null() {
                // SAFETY: o tap criado em `ensure_started`.
                unsafe { CGEventTapEnable(tap, true) };
            }
        }
        _ => {}
    }
    event // só escutamos: o evento segue o seu caminho
}

fn mods_from(flags: u64) -> Mods {
    [
        (FLAG_CONTROL, Mods::CTRL),
        (FLAG_ALTERNATE, Mods::ALT),
        (FLAG_SHIFT, Mods::SHIFT),
        (FLAG_COMMAND, Mods::SUPER),
        (FLAG_FN, Mods::FN),
    ]
    .into_iter()
    .filter(|&(flag, _)| flags & flag != 0)
    .fold(Mods::NONE, |acc, (_, m)| acc | m)
}
