//! Atalho global. O handler do plugin corre na thread de eventos do sistema e só
//! faz uma coisa: marcar o instante e enviar um evento pelo canal do orquestrador.
//! Toda a lógica (segurar vs alternar, cancelar) vive em `dictation`.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use tauri::{AppHandle, Runtime, plugin::TauriPlugin};
use tauri_plugin_global_shortcut::{
    Builder, Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    dictation::{DictationEvent, machine::Mode},
    modkeys::{self, Mods},
};

/// O atalho de ditado por omissão: só modificadores, como no Wispr Flow. No
/// macOS é a tecla Fn; no Windows, Ctrl+Win (ver `modkeys`).
#[cfg(target_os = "macos")]
pub const DEFAULT_SHORTCUT: &str = "Fn";
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_SHORTCUT: &str = "Control+Super";
/// O de antes da 0.6. Quem o tem no config.json (nunca o mudou) passa para o
/// atual; ver `config::migrate`.
pub const LEGACY_SHORTCUT: &str = "CommandOrControl+Shift+Space";
/// O do modo comando. `CommandOrControl` é Ctrl no Windows e Cmd no macOS; o
/// Windows só dispara um atalho se os modificadores forem exatamente os
/// registados, por isso não se confunde com outros.
pub const DEFAULT_COMMAND_SHORTCUT: &str = "CommandOrControl+Shift+Alt+Space";
/// O de "perguntar sobre o ecrã" (ver `screen/`). Como o de comando, tem Shift e Alt para
/// não coincidir com os atalhos das outras apps.
pub const DEFAULT_SCREEN_SHORTCUT: &str = "CommandOrControl+Shift+Alt+KeyS";

/// O `id()` do atalho de comando registado, ou `NO_COMMAND`. O handler do plugin
/// corre noutra thread e só recebe o atalho premido: compara-o com isto para
/// saber o modo. Um atómico chega (é só um número) e dispensa um `Mutex`.
static COMMAND_ID: AtomicU64 = AtomicU64::new(NO_COMMAND);
/// O mesmo para o atalho do histórico rápido.
static QUICK_ID: AtomicU64 = AtomicU64::new(NO_COMMAND);
/// O mesmo para o atalho de perguntar sobre o ecrã.
static SCREEN_ID: AtomicU64 = AtomicU64::new(NO_COMMAND);
/// Fora do intervalo de um `u32`, por isso nunca é o `id()` de um atalho.
const NO_COMMAND: u64 = u64::MAX;

#[derive(Debug, thiserror::Error)]
pub enum HotkeyError {
    #[error("atalho inválido \"{shortcut}\": {reason}")]
    Invalid { shortcut: String, reason: String },
    #[error(
        "não foi possível registar \"{shortcut}\" (provavelmente já está em uso por outra app): {source}"
    )]
    Register {
        shortcut: String,
        #[source]
        source: tauri_plugin_global_shortcut::Error,
    },
    /// Atalho só de modificadores que o sistema não deixa ouvir.
    #[error("não foi possível ouvir \"{shortcut}\": {reason}")]
    Unavailable { shortcut: String, reason: String },
}

/// Um atalho lido: uma combinação com tecla (do plugin) ou só modificadores
/// (do `modkeys`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parsed {
    Key(Shortcut),
    Mods(Mods),
}

fn cancel_shortcut() -> Shortcut {
    Shortcut::new(None, Code::Escape)
}

/// Cria o plugin com o handler ligado ao canal. O `move` dá ao closure a sua própria
/// cópia do `Sender` (os `Sender` do tokio são baratos de clonar e partilham o canal).
pub fn plugin<R: Runtime>(tx: UnboundedSender<DictationEvent>) -> TauriPlugin<R> {
    // Os atalhos só de modificadores não passam pelo plugin: enviam pelo mesmo canal.
    modkeys::init(tx.clone());
    Builder::new()
        .with_handler(move |_app, shortcut, event| {
            // Medimos aqui, e não quando o orquestrador recebe a mensagem, para que
            // o "soltar a tecla" das métricas seja o mais fiel possível.
            let at = Instant::now();
            let pressed = event.state() == ShortcutState::Pressed;
            let ev = if shortcut.matches(Modifiers::empty(), Code::Escape) {
                if !pressed {
                    return;
                }
                DictationEvent::Cancel
            } else if u64::from(shortcut.id()) == QUICK_ID.load(Ordering::Relaxed) {
                if !pressed {
                    return;
                }
                DictationEvent::QuickHistory
            } else if u64::from(shortcut.id()) == SCREEN_ID.load(Ordering::Relaxed) {
                if !pressed {
                    return;
                }
                DictationEvent::Screen {
                    settle: Duration::ZERO,
                }
            } else {
                let mode = if u64::from(shortcut.id()) == COMMAND_ID.load(Ordering::Relaxed) {
                    Mode::Command
                } else {
                    Mode::Dictation
                };
                DictationEvent::Hotkey { pressed, at, mode }
            };
            // `send` só falha se o orquestrador já terminou (a app está a sair).
            if tx.send(ev).is_err() {
                tracing::debug!("orquestrador terminado; evento de atalho descartado");
            }
        })
        .build()
}

pub fn register_dictation<R: Runtime>(
    app: &AppHandle<R>,
    shortcut: &str,
) -> Result<(), HotkeyError> {
    let parsed = parse(shortcut)?;
    activate(app, shortcut, parsed, Slot::Dictation)?;
    tracing::info!(shortcut, "atalho de ditado registado");
    Ok(())
}

/// Qual dos dois atalhos.
#[derive(Clone, Copy)]
enum Slot {
    Dictation,
    Command,
}

/// Liga `parsed`, pelo plugin ou pelo `modkeys`. No `modkeys` isto também
/// substitui o atalho anterior do mesmo `slot`; no plugin, largar o antigo é
/// com `deactivate`.
fn activate<R: Runtime>(
    app: &AppHandle<R>,
    text: &str,
    parsed: Parsed,
    slot: Slot,
) -> Result<(), HotkeyError> {
    match parsed {
        Parsed::Key(key) => {
            app.global_shortcut()
                .register(key)
                .map_err(|source| HotkeyError::Register {
                    shortcut: text.to_owned(),
                    source,
                })
        }
        Parsed::Mods(mods) => {
            set_mods(slot, Some(mods)).map_err(|reason| HotkeyError::Unavailable {
                shortcut: text.to_owned(),
                reason,
            })
        }
    }
}

/// Larga `old` (que deixou de ser o atalho do `slot`), a não ser que `new` o
/// tenha substituído no mesmo sítio.
fn deactivate<R: Runtime>(app: &AppHandle<R>, old: Parsed, new: Option<Parsed>, slot: Slot) {
    match old {
        Parsed::Key(key) => {
            if let Err(e) = app.global_shortcut().unregister(key) {
                tracing::warn!(error = %e, "não foi possível largar o atalho antigo");
            }
        }
        // Um novo atalho de modificadores já trocou o antigo.
        Parsed::Mods(_) if matches!(new, Some(Parsed::Mods(_))) => {}
        Parsed::Mods(_) => {
            let _ = set_mods(slot, None);
        }
    }
}

fn set_mods(slot: Slot, mods: Option<Mods>) -> Result<(), String> {
    match slot {
        Slot::Dictation => modkeys::set_dictation(mods),
        Slot::Command => modkeys::set_command(mods),
    }
}

/// Lê um atalho: só modificadores (`modkeys`) ou com tecla (plugin).
fn parse(shortcut: &str) -> Result<Parsed, HotkeyError> {
    let Some(mods) = modkeys::parse(shortcut) else {
        return parse_key(shortcut).map(Parsed::Key);
    };
    modkeys::validate(mods).map_err(|reason| HotkeyError::Invalid {
        shortcut: shortcut.to_owned(),
        reason: reason.to_owned(),
    })?;
    Ok(Parsed::Mods(mods))
}

fn parse_key(shortcut: &str) -> Result<Shortcut, HotkeyError> {
    let parsed: Shortcut = shortcut.parse().map_err(|e| HotkeyError::Invalid {
        shortcut: shortcut.to_owned(),
        reason: format!("{e}"),
    })?;
    // Uma tecla "normal" sem modificadores (ex.: só "A") deixaria de funcionar em
    // todas as outras apps. Só as teclas F13–F24, que quase ninguém usa, podem ir sozinhas.
    let lone_ok = matches!(
        parsed.key,
        Code::F13
            | Code::F14
            | Code::F15
            | Code::F16
            | Code::F17
            | Code::F18
            | Code::F19
            | Code::F20
            | Code::F21
            | Code::F22
            | Code::F23
            | Code::F24
    );
    if parsed.mods.is_empty() && !lone_ok {
        return Err(HotkeyError::Invalid {
            shortcut: shortcut.to_owned(),
            reason: "inclui pelo menos um modificador (Ctrl, Alt, Shift ou Cmd), ou usa F13–F24"
                .into(),
        });
    }
    if parsed.key == Code::Escape {
        return Err(HotkeyError::Invalid {
            shortcut: shortcut.to_owned(),
            reason: "o Esc está reservado para cancelar a gravação".into(),
        });
    }
    Ok(parsed)
}

/// Troca o atalho de ditado. Regista o novo ANTES de largar o antigo: se o novo
/// estiver ocupado por outra app (o `register` falha), o antigo continua ativo e
/// o utilizador recebe o erro.
pub fn change_dictation<R: Runtime>(
    app: &AppHandle<R>,
    old: &str,
    new: &str,
) -> Result<(), HotkeyError> {
    let new_parsed = parse(new)?;
    let old_parsed = parse_loose(old);
    if old_parsed == Some(new_parsed) {
        return Ok(());
    }
    activate(app, new, new_parsed, Slot::Dictation)?;
    if let Some(old) = old_parsed {
        deactivate(app, old, Some(new_parsed), Slot::Dictation);
    }
    tracing::info!(old, new, "atalho de ditado alterado");
    Ok(())
}

/// Regista o atalho do modo comando (no arranque).
pub fn register_command<R: Runtime>(app: &AppHandle<R>, shortcut: &str) -> Result<(), HotkeyError> {
    change_command(app, None, Some(shortcut))
}

/// Troca o atalho de comando: `old` é o que está registado agora (`None` se
/// nenhum) e `new` o que passa a estar (`None` = desligar o modo comando). Como
/// no ditado, regista o novo antes de largar o antigo: se falhar, fica tudo
/// como estava.
pub fn change_command<R: Runtime>(
    app: &AppHandle<R>,
    old: Option<&str>,
    new: Option<&str>,
) -> Result<(), HotkeyError> {
    let old_parsed = old.and_then(parse_loose);
    let new_parsed = new.map(parse).transpose()?;
    if old_parsed == new_parsed {
        return Ok(());
    }
    if let (Some(parsed), Some(text)) = (new_parsed, new) {
        activate(app, text, parsed, Slot::Command)?;
    }
    if let Some(old) = old_parsed {
        deactivate(app, old, new_parsed, Slot::Command);
    }
    // Só os atalhos com tecla passam pelo handler do plugin, que usa isto para
    // saber o modo; os de modificadores já levam o modo com eles.
    let id = match new_parsed {
        Some(Parsed::Key(key)) => u64::from(key.id()),
        _ => NO_COMMAND,
    };
    COMMAND_ID.store(id, Ordering::Relaxed);
    tracing::info!(?old, ?new, "atalho de comando");
    Ok(())
}

/// Regista o atalho do histórico rápido (no arranque).
pub fn register_quick<R: Runtime>(app: &AppHandle<R>, shortcut: &str) -> Result<(), HotkeyError> {
    change_quick(app, None, Some(shortcut))
}

/// Troca o atalho do histórico rápido: `old` é o que está registado agora e `new`
/// o que passa a estar (`None` = desligar). Como nos outros, regista o novo antes
/// de largar o antigo. Só aceita combinações com tecla: os atalhos só de
/// modificadores (`modkeys`) têm dois lugares, o do ditado e o do comando.
pub fn change_quick<R: Runtime>(
    app: &AppHandle<R>,
    old: Option<&str>,
    new: Option<&str>,
) -> Result<(), HotkeyError> {
    change_key_only(app, old, new, &QUICK_ID, parse_quick)?;
    tracing::info!(?old, ?new, "atalho do histórico rápido");
    Ok(())
}

fn parse_quick(shortcut: &str) -> Result<Shortcut, HotkeyError> {
    parse_key_only(shortcut, "o histórico rápido", "Ctrl+Shift+H")
}

/// Regista o atalho de perguntar sobre o ecrã (no arranque).
pub fn register_screen<R: Runtime>(app: &AppHandle<R>, shortcut: &str) -> Result<(), HotkeyError> {
    change_screen(app, None, Some(shortcut))
}

/// Troca o atalho de perguntar sobre o ecrã, como `change_quick`: `old` é o que está
/// registado agora e `new` o que passa a estar (`None` = desligar).
pub fn change_screen<R: Runtime>(
    app: &AppHandle<R>,
    old: Option<&str>,
    new: Option<&str>,
) -> Result<(), HotkeyError> {
    change_key_only(app, old, new, &SCREEN_ID, parse_screen)?;
    tracing::info!(?old, ?new, "atalho de perguntar sobre o ecrã");
    Ok(())
}

fn parse_screen(shortcut: &str) -> Result<Shortcut, HotkeyError> {
    parse_key_only(shortcut, "perguntar sobre o ecrã", "Ctrl+Shift+Alt+S")
}

/// A parte comum dos atalhos que só aceitam combinações com tecla: regista `new` antes de
/// largar `old` (se falhar, fica tudo como estava) e guarda o `id` do novo em `slot`, que
/// é o que o handler do plugin compara para saber que atalho foi premido.
fn change_key_only<R: Runtime>(
    app: &AppHandle<R>,
    old: Option<&str>,
    new: Option<&str>,
    slot: &AtomicU64,
    parse_new: fn(&str) -> Result<Shortcut, HotkeyError>,
) -> Result<(), HotkeyError> {
    let old_key = old.and_then(parse_loose).and_then(|p| match p {
        Parsed::Key(key) => Some(key),
        Parsed::Mods(_) => None,
    });
    let new_key = new.map(parse_new).transpose()?;
    if old_key == new_key {
        return Ok(());
    }
    if let Some(key) = new_key {
        app.global_shortcut()
            .register(key)
            .map_err(|source| HotkeyError::Register {
                shortcut: new.unwrap_or_default().to_owned(),
                source,
            })?;
    }
    if let Some(key) = old_key
        && let Err(e) = app.global_shortcut().unregister(key)
    {
        tracing::warn!(error = %e, "não foi possível largar o atalho antigo");
    }
    slot.store(
        new_key.map_or(NO_COMMAND, |key| u64::from(key.id())),
        Ordering::Relaxed,
    );
    Ok(())
}

fn parse_key_only(shortcut: &str, what: &str, example: &str) -> Result<Shortcut, HotkeyError> {
    match parse(shortcut)? {
        Parsed::Key(key) => Ok(key),
        Parsed::Mods(_) => Err(HotkeyError::Invalid {
            shortcut: shortcut.to_owned(),
            reason: format!(
                "{what} precisa de uma tecla (ex.: {example}), não só de modificadores"
            ),
        }),
    }
}

/// Os dois textos são o mesmo atalho? ("Control+Shift+Space" e
/// "CommandOrControl+Shift+Space" são, no Windows.) Textos inválidos nunca são
/// iguais: a validação deles é feita ao registar.
pub fn same_shortcut(a: &str, b: &str) -> bool {
    matches!((parse_loose(a), parse_loose(b)), (Some(a), Some(b)) if a == b)
}

/// Lê um atalho sem o validar: serve para comparar com o que está registado.
fn parse_loose(shortcut: &str) -> Option<Parsed> {
    match modkeys::parse(shortcut) {
        Some(mods) => Some(Parsed::Mods(mods)),
        None => shortcut.parse::<Shortcut>().ok().map(Parsed::Key),
    }
}

/// Larga o atalho de ditado (só usado para desfazer um registo quando gravar as
/// definições falha).
pub fn unregister_dictation<R: Runtime>(app: &AppHandle<R>, shortcut: &str) {
    if let Some(parsed) = parse_loose(shortcut) {
        deactivate(app, parsed, None, Slot::Dictation);
    }
}

/// "CommandOrControl+Shift+Space" → "Ctrl+Shift+Espaço" (no macOS, "Cmd+…"). Para
/// o menu do tray e as notificações; a janela tem a sua versão em `format.ts`.
pub fn pretty(shortcut: &str) -> String {
    // Os nomes das teclas de comando, de sistema e Alt mudam com a plataforma.
    let (cmd_or_ctrl, sup, alt) = if cfg!(target_os = "macos") {
        ("Cmd", "Cmd", "Option")
    } else {
        ("Ctrl", "Win", "Alt")
    };
    shortcut
        .split('+')
        .filter(|p| !p.is_empty())
        .map(|part| {
            let label = match part.to_ascii_lowercase().as_str() {
                "commandorcontrol" | "cmdorctrl" | "commandorctrl" | "cmdorcontrol" => cmd_or_ctrl,
                "control" | "ctrl" => "Ctrl",
                "super" | "command" | "cmd" | "meta" | "win" => sup,
                "fn" => "Fn",
                "alt" | "option" => alt,
                "shift" => "Shift",
                "space" => "Espaço",
                // `strip_prefix` devolve `Some(resto)` só se o prefixo estiver lá.
                _ => {
                    return part
                        .strip_prefix("Key")
                        .or_else(|| part.strip_prefix("Digit"))
                        .unwrap_or(part)
                        .to_owned();
                }
            };
            label.to_owned()
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// O Esc só fica registado enquanto se grava: um atalho global "rouba" a tecla às
/// outras apps, e não queremos isso no resto do tempo.
pub fn set_cancel_enabled<R: Runtime>(app: &AppHandle<R>, enabled: bool) {
    let gs = app.global_shortcut();
    let esc = cancel_shortcut();
    let result = if enabled {
        if gs.is_registered(esc) {
            return;
        }
        gs.register(esc)
    } else {
        if !gs.is_registered(esc) {
            return;
        }
        gs.unregister(esc)
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, enabled, "não foi possível (des)registar o Esc para cancelar");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_combos_and_lone_f13_to_f24() {
        assert!(parse("CommandOrControl+Shift+Space").is_ok());
        assert!(parse("Control+Alt+KeyD").is_ok());
        assert!(parse("F13").is_ok());
    }

    #[test]
    fn pretty_names_for_the_tray() {
        let ctrl = if cfg!(target_os = "macos") {
            "Cmd"
        } else {
            "Ctrl"
        };
        assert_eq!(
            pretty("CommandOrControl+Shift+Space"),
            format!("{ctrl}+Shift+Espaço")
        );
        assert_eq!(pretty("Control+Shift+KeyD"), "Ctrl+Shift+D");
        assert_eq!(pretty("Control+Digit5"), "Ctrl+5");
        assert_eq!(pretty("F13"), "F13");
    }

    #[test]
    fn command_default_is_valid_and_differs_from_dictation() {
        assert!(parse(DEFAULT_COMMAND_SHORTCUT).is_ok());
        assert!(!same_shortcut(DEFAULT_SHORTCUT, DEFAULT_COMMAND_SHORTCUT));
        assert!(same_shortcut("Control+Shift+KeyD", "Shift+Control+KeyD"));
        assert!(!same_shortcut("Control+Banana", "Control+Banana"));
    }

    #[test]
    fn the_default_is_a_valid_modifier_only_shortcut() {
        assert!(matches!(parse(DEFAULT_SHORTCUT), Ok(Parsed::Mods(_))));
        assert!(pretty("Control+Super").starts_with("Ctrl+"));
        assert_eq!(pretty("Fn"), "Fn");
    }

    #[test]
    fn modifier_only_shortcuts_need_two_modifiers_and_fn_is_mac_only() {
        assert!(matches!(parse("Control+Alt"), Ok(Parsed::Mods(_))));
        assert!(matches!(parse("Control"), Err(HotkeyError::Invalid { .. })));
        assert_eq!(parse("Fn").is_ok(), cfg!(target_os = "macos"));
    }

    #[test]
    fn same_shortcut_compares_modifier_only_ones_too() {
        assert!(same_shortcut("Control+Super", "Super+Ctrl"));
        assert!(!same_shortcut("Control+Super", "Control+Alt"));
        assert!(!same_shortcut("Control+Super", "Control+Shift+Space"));
        assert!(!same_shortcut(DEFAULT_SHORTCUT, DEFAULT_COMMAND_SHORTCUT));
    }

    #[test]
    fn quick_history_needs_a_key_not_only_modifiers() {
        assert!(parse_quick("Control+Shift+KeyH").is_ok());
        assert!(parse_quick("F13").is_ok());
        assert!(matches!(
            parse_quick("Control+Alt"),
            Err(HotkeyError::Invalid { .. })
        ));
        assert!(matches!(
            parse_quick("KeyH"),
            Err(HotkeyError::Invalid { .. })
        ));
    }

    #[test]
    fn the_screen_shortcut_default_is_a_key_combination() {
        assert!(parse(DEFAULT_SCREEN_SHORTCUT).is_ok());
        assert!(parse_screen(DEFAULT_SCREEN_SHORTCUT).is_ok());
        assert_eq!(
            pretty(DEFAULT_SCREEN_SHORTCUT),
            if cfg!(target_os = "macos") {
                "Cmd+Shift+Option+S"
            } else {
                "Ctrl+Shift+Alt+S"
            }
        );
        assert!(!same_shortcut(
            DEFAULT_SCREEN_SHORTCUT,
            DEFAULT_COMMAND_SHORTCUT
        ));
        assert!(!same_shortcut(DEFAULT_SCREEN_SHORTCUT, DEFAULT_SHORTCUT));
        // Só modificadores não serve: ficaria a competir com o ditado.
        let err = parse_screen("Control+Alt").unwrap_err().to_string();
        assert!(
            err.contains("perguntar sobre o ecrã precisa de uma tecla"),
            "{err}"
        );
        assert!(matches!(
            parse_screen("KeyS"),
            Err(HotkeyError::Invalid { .. })
        ));
    }

    #[test]
    fn rejects_lone_keys_escape_and_garbage() {
        assert!(matches!(parse("KeyA"), Err(HotkeyError::Invalid { .. })));
        assert!(matches!(parse("Space"), Err(HotkeyError::Invalid { .. })));
        assert!(matches!(
            parse("Control+Escape"),
            Err(HotkeyError::Invalid { .. })
        ));
        assert!(matches!(
            parse("Control+Banana"),
            Err(HotkeyError::Invalid { .. })
        ));
    }
}
