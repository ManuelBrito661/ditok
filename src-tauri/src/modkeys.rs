//! Atalhos feitos só de modificadores: Ctrl+Win no Windows, Fn no macOS.
//!
//! O plugin de atalhos globais precisa de uma tecla "normal" (Espaço, D…), por
//! isso não os consegue registar. Aqui ouvimos o teclado ao nível do sistema
//! (um hook de teclado no Windows, um "event tap" no macOS) e seguimos quais
//! modificadores estão premidos. Os backends só traduzem o sistema para
//! `on_mods` / `on_other_key`; a lógica de quando o atalho "dispara" é toda
//! daqui e testa-se sem teclado.

use std::{
    ops::BitOr,
    sync::{Mutex, MutexGuard, PoisonError},
    time::Instant,
};

use tokio::sync::mpsc::UnboundedSender;

use crate::dictation::{DictationEvent, machine::Mode};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
use macos as backend;
#[cfg(target_os = "windows")]
use windows as backend;

/// Sem backend (só o Windows e o macOS são alvos): ouvir o teclado não existe.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod backend {
    pub fn ensure_started() -> Result<(), String> {
        Err("atalhos só de modificadores não existem nesta plataforma".into())
    }
}

/// Um conjunto de modificadores. `Fn` só existe no macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods(u8);

impl Mods {
    pub const NONE: Mods = Mods(0);
    pub const CTRL: Mods = Mods(1);
    pub const ALT: Mods = Mods(2);
    pub const SHIFT: Mods = Mods(4);
    pub const SUPER: Mods = Mods(8);
    pub const FN: Mods = Mods(16);

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, other: Mods) -> bool {
        self.0 & other.0 == other.0
    }

    const fn count(self) -> u32 {
        self.0.count_ones()
    }

    /// `self` está dentro de `other` mas não é igual: "ainda falta carregar
    /// em alguma tecla". É o que distingue *chegar* ao atalho de *sair* dele.
    const fn is_strict_subset_of(self, other: Mods) -> bool {
        self.0 != other.0 && self.0 & !other.0 == 0
    }
}

impl BitOr for Mods {
    type Output = Mods;
    fn bitor(self, rhs: Mods) -> Mods {
        Mods(self.0 | rhs.0)
    }
}

/// Lê um atalho feito só de modificadores ("Control+Super", "Fn"). `None` se
/// tiver alguma tecla "normal" (é para o plugin) ou for vazio.
pub fn parse(shortcut: &str) -> Option<Mods> {
    let mut mods = Mods::NONE;
    let mut any = false;
    for part in shortcut.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        any = true;
        mods = mods
            | match part.to_ascii_lowercase().as_str() {
                "control" | "ctrl" => Mods::CTRL,
                "commandorcontrol" | "cmdorctrl" | "commandorctrl" | "cmdorcontrol" => {
                    if cfg!(target_os = "macos") {
                        Mods::SUPER
                    } else {
                        Mods::CTRL
                    }
                }
                "alt" | "option" => Mods::ALT,
                "shift" => Mods::SHIFT,
                "super" | "command" | "cmd" | "meta" | "win" => Mods::SUPER,
                "fn" => Mods::FN,
                _ => return None,
            };
    }
    any.then_some(mods)
}

/// Um atalho só de modificadores serve se for difícil de premir sem querer:
/// o Fn sozinho (só no Mac, onde quase ninguém o usa) ou dois ou mais.
pub fn validate(mods: Mods) -> Result<(), &'static str> {
    if mods.contains(Mods::FN) && !cfg!(target_os = "macos") {
        return Err("a tecla Fn só pode ser usada no Mac");
    }
    if mods != Mods::FN && mods.count() < 2 {
        return Err("um atalho só de modificadores precisa de pelo menos dois (ex.: Ctrl+Win)");
    }
    Ok(())
}

// ---------------------------------------------------------------- lógica

/// Um dos dois atalhos (ditado, comando) e se está a ser premido.
struct Slot {
    target: Option<Mods>,
    active: bool,
    mode: Mode,
}

struct State {
    tx: Option<UnboundedSender<DictationEvent>>,
    dictation: Slot,
    command: Slot,
    /// Os modificadores premidos agora.
    held: Mods,
    /// Carregou-se numa tecla "normal" com modificadores premidos (Ctrl+C…):
    /// acrescentar o resto do atalho a seguir não o dispara. Limpa-se quando
    /// se larga tudo.
    tainted: bool,
}

impl State {
    const fn new() -> Self {
        Self {
            tx: None,
            dictation: Slot {
                target: None,
                active: false,
                mode: Mode::Dictation,
            },
            command: Slot {
                target: None,
                active: false,
                mode: Mode::Command,
            },
            held: Mods::NONE,
            tainted: false,
        }
    }

    fn slots(&mut self) -> [&mut Slot; 2] {
        [&mut self.dictation, &mut self.command]
    }

    /// Os modificadores premidos mudaram. Devolve os que acabaram de completar
    /// um atalho (para o Windows engolir a tecla Win que o completou).
    fn on_mods(&mut self, held: Mods, send: &mut impl FnMut(DictationEvent)) -> Mods {
        let prev = std::mem::replace(&mut self.held, held);
        let tainted = self.tainted;
        let at = Instant::now();
        let mut activated = Mods::NONE;
        for slot in self.slots() {
            let Some(target) = slot.target else { continue };
            let mode = slot.mode;
            if slot.active {
                if held != target {
                    slot.active = false;
                    send(DictationEvent::Hotkey {
                        pressed: false,
                        at,
                        mode,
                    });
                }
            } else if !tainted && held == target && prev.is_strict_subset_of(target) {
                slot.active = true;
                activated = activated | target;
                send(DictationEvent::Hotkey {
                    pressed: true,
                    at,
                    mode,
                });
            }
        }
        if held.is_empty() {
            self.tainted = false;
        }
        activated
    }

    /// Uma tecla que não é modificador foi premida. Se um atalho estava a
    /// meio, era outra coisa (Ctrl+Win+← muda de ambiente de trabalho): cancela
    /// em vez de deixar o ditado a correr. `escape`: o Esc já cancela por si.
    fn on_other_key(&mut self, escape: bool, send: &mut impl FnMut(DictationEvent)) {
        if self.held.is_empty() {
            return;
        }
        let mut was_active = false;
        for slot in self.slots() {
            was_active |= std::mem::take(&mut slot.active);
        }
        if was_active {
            if !escape {
                send(DictationEvent::Cancel);
            }
        } else {
            self.tainted = true;
        }
    }
}

static STATE: Mutex<State> = Mutex::new(State::new());

fn state() -> MutexGuard<'static, State> {
    // Um pânico noutra thread não pode desligar os atalhos para sempre.
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Guarda o canal do orquestrador (chamado uma vez, ao criar o plugin).
pub fn init(tx: UnboundedSender<DictationEvent>) {
    state().tx = Some(tx);
}

/// Liga (`Some`) ou desliga (`None`) o atalho de ditado só de modificadores.
pub fn set_dictation(target: Option<Mods>) -> Result<(), String> {
    set(target, |s| &mut s.dictation)
}

/// O mesmo para o atalho do modo comando.
pub fn set_command(target: Option<Mods>) -> Result<(), String> {
    set(target, |s| &mut s.command)
}

fn set(target: Option<Mods>, slot: impl FnOnce(&mut State) -> &mut Slot) -> Result<(), String> {
    if target.is_some() {
        backend::ensure_started()?;
    }
    let mut s = state();
    let slot = slot(&mut s);
    slot.target = target;
    slot.active = false;
    Ok(())
}

/// Algum atalho usa o Fn? Se sim, o backend do Mac engole a tecla para o
/// sistema não abrir os emojis ao largá-la.
#[cfg(target_os = "macos")]
fn wants_fn() -> bool {
    let s = state();
    [&s.dictation, &s.command]
        .iter()
        .any(|slot| slot.target.is_some_and(|t| t.contains(Mods::FN)))
}

/// Chamado pelos backends: os modificadores premidos agora são `held`.
fn on_mods(held: Mods) -> Mods {
    let mut s = state();
    let tx = s.tx.clone();
    s.on_mods(held, &mut |ev| send(tx.as_ref(), ev))
}

/// Chamado pelos backends: carregou-se numa tecla que não é modificador.
fn on_other_key(escape: bool) {
    let mut s = state();
    let tx = s.tx.clone();
    s.on_other_key(escape, &mut |ev| send(tx.as_ref(), ev));
}

fn send(tx: Option<&UnboundedSender<DictationEvent>>, ev: DictationEvent) {
    // `send` só falha se o orquestrador já terminou (a app está a sair).
    if tx.is_none_or(|tx| tx.send(ev).is_err()) {
        tracing::debug!("orquestrador terminado; evento de atalho descartado");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL_WIN: Mods = Mods(Mods::CTRL.0 | Mods::SUPER.0);

    /// Um estado com o atalho de ditado ligado e o que a máquina de estados
    /// receberia (`true` = premido, `false` = largado, `None` = cancelar).
    struct Rig {
        state: State,
        events: Vec<Option<bool>>,
    }

    impl Rig {
        fn new(target: Mods) -> Self {
            let mut state = State::new();
            state.dictation.target = Some(target);
            Rig {
                state,
                events: Vec::new(),
            }
        }

        fn mods(&mut self, held: Mods) -> Mods {
            let events = &mut self.events;
            self.state.on_mods(held, &mut |ev| match ev {
                DictationEvent::Hotkey { pressed, .. } => events.push(Some(pressed)),
                DictationEvent::Cancel => events.push(None),
                _ => unreachable!(),
            })
        }

        fn key(&mut self, escape: bool) {
            let events = &mut self.events;
            self.state.on_other_key(escape, &mut |ev| match ev {
                DictationEvent::Cancel => events.push(None),
                _ => unreachable!(),
            });
        }

        fn take(&mut self) -> Vec<Option<bool>> {
            std::mem::take(&mut self.events)
        }
    }

    #[test]
    fn parses_modifier_only_shortcuts() {
        assert_eq!(parse("Control+Super"), Some(CTRL_WIN));
        assert_eq!(parse("Ctrl+Win"), Some(CTRL_WIN));
        assert_eq!(parse("Fn"), Some(Mods::FN));
        assert_eq!(parse("Control+Alt"), Some(Mods::CTRL | Mods::ALT));
        // Com uma tecla normal, é do plugin.
        assert_eq!(parse("Control+Shift+Space"), None);
        assert_eq!(parse("F13"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn validation_wants_two_modifiers_or_fn_on_the_mac() {
        assert!(validate(CTRL_WIN).is_ok());
        assert!(validate(Mods::CTRL).is_err());
        assert_eq!(validate(Mods::FN).is_ok(), cfg!(target_os = "macos"));
    }

    #[test]
    fn hold_ctrl_then_win_presses_and_releasing_either_releases() {
        let mut rig = Rig::new(CTRL_WIN);
        assert_eq!(rig.mods(Mods::CTRL), Mods::NONE);
        assert_eq!(rig.mods(CTRL_WIN), Mods::SUPER | Mods::CTRL);
        assert_eq!(rig.take(), vec![Some(true)]);
        rig.mods(Mods::CTRL);
        assert_eq!(rig.take(), vec![Some(false)]);
        // Largar o resto não faz mais nada.
        rig.mods(Mods::NONE);
        assert!(rig.take().is_empty());
    }

    #[test]
    fn the_order_does_not_matter() {
        let mut rig = Rig::new(CTRL_WIN);
        rig.mods(Mods::SUPER);
        rig.mods(CTRL_WIN);
        assert_eq!(rig.take(), vec![Some(true)]);
    }

    #[test]
    fn extra_modifiers_do_not_trigger_or_re_arm() {
        let mut rig = Rig::new(CTRL_WIN);
        let with_shift = CTRL_WIN | Mods::SHIFT;
        rig.mods(Mods::CTRL);
        rig.mods(with_shift);
        assert!(rig.take().is_empty());
        // Largar o Shift deixa exatamente Ctrl+Win, mas já não é "chegar" ao atalho.
        rig.mods(CTRL_WIN);
        assert!(rig.take().is_empty());
    }

    #[test]
    fn extra_modifier_while_active_releases() {
        let mut rig = Rig::new(CTRL_WIN);
        rig.mods(Mods::CTRL);
        rig.mods(CTRL_WIN);
        rig.mods(CTRL_WIN | Mods::SHIFT);
        assert_eq!(rig.take(), vec![Some(true), Some(false)]);
    }

    #[test]
    fn ctrl_c_then_win_does_not_trigger() {
        let mut rig = Rig::new(CTRL_WIN);
        rig.mods(Mods::CTRL);
        rig.key(false); // o C
        rig.mods(CTRL_WIN);
        assert!(rig.take().is_empty());
        // Largar tudo limpa a marca: a próxima vez dispara.
        rig.mods(Mods::NONE);
        rig.mods(Mods::CTRL);
        rig.mods(CTRL_WIN);
        assert_eq!(rig.take(), vec![Some(true)]);
    }

    #[test]
    fn another_key_while_active_cancels_but_escape_leaves_it_to_the_plugin() {
        let mut rig = Rig::new(CTRL_WIN);
        rig.mods(Mods::CTRL);
        rig.mods(CTRL_WIN);
        rig.key(false); // Ctrl+Win+←
        assert_eq!(rig.take(), vec![Some(true), None]);
        // Já não está ativo: largar não manda "largado".
        rig.mods(Mods::NONE);
        assert!(rig.take().is_empty());

        rig.mods(Mods::CTRL);
        rig.mods(CTRL_WIN);
        rig.key(true); // Esc
        assert_eq!(rig.take(), vec![Some(true)]);
    }

    #[test]
    fn typing_without_modifiers_is_ignored() {
        let mut rig = Rig::new(Mods::FN);
        rig.key(false);
        rig.mods(Mods::FN);
        assert_eq!(rig.take(), vec![Some(true)]);
    }

    #[test]
    fn fn_alone_presses_and_releases() {
        let mut rig = Rig::new(Mods::FN);
        rig.mods(Mods::FN);
        rig.mods(Mods::NONE);
        assert_eq!(rig.take(), vec![Some(true), Some(false)]);
    }
}
