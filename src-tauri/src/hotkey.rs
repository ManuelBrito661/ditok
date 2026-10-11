//! Atalho global. O handler do plugin corre na thread de eventos do sistema e só
//! faz uma coisa: marcar o instante e enviar um evento pelo canal do orquestrador.
//! Toda a lógica (segurar vs alternar, cancelar) vive em `dictation`.

use std::time::Instant;

use tauri::{AppHandle, Runtime, plugin::TauriPlugin};
use tauri_plugin_global_shortcut::{
    Builder, Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::dictation::DictationEvent;

/// `CommandOrControl` é Ctrl no Windows e Cmd no macOS.
pub const DEFAULT_SHORTCUT: &str = "CommandOrControl+Shift+Space";

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
}

fn cancel_shortcut() -> Shortcut {
    Shortcut::new(None, Code::Escape)
}

/// Cria o plugin com o handler ligado ao canal. O `move` dá ao closure a sua própria
/// cópia do `Sender` (os `Sender` do tokio são baratos de clonar e partilham o canal).
pub fn plugin<R: Runtime>(tx: UnboundedSender<DictationEvent>) -> TauriPlugin<R> {
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
            } else {
                DictationEvent::Hotkey { pressed, at }
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
    app.global_shortcut()
        .register(parsed)
        .map_err(|source| HotkeyError::Register {
            shortcut: shortcut.to_owned(),
            source,
        })?;
    tracing::info!(shortcut, "atalho de ditado registado");
    Ok(())
}

fn parse(shortcut: &str) -> Result<Shortcut, HotkeyError> {
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
    let old_parsed = old.parse::<Shortcut>().ok();
    if old_parsed == Some(new_parsed) {
        return Ok(());
    }
    let gs = app.global_shortcut();
    gs.register(new_parsed)
        .map_err(|source| HotkeyError::Register {
            shortcut: new.to_owned(),
            source,
        })?;
    if let Some(old) = old_parsed
        && let Err(e) = gs.unregister(old)
    {
        tracing::warn!(error = %e, "não foi possível largar o atalho antigo");
    }
    tracing::info!(old, new, "atalho de ditado alterado");
    Ok(())
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
