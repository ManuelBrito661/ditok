//! Colar texto na app em foco: guardar o clipboard → pôr o texto → simular
//! Ctrl+V / Cmd+V → (mais tarde) repor o clipboard anterior.
//!
//! Tudo aqui é bloqueante (APIs do sistema), por isso é chamado a partir de
//! `spawn_blocking` e nunca diretamente numa tarefa assíncrona.

use std::time::{Duration, Instant};

use arboard::Clipboard;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};

/// Tempo que damos à app de destino para ler o clipboard antes de o repor. Se for
/// curto demais, há apps que colariam o conteúdo antigo.
pub const RESTORE_AFTER: Duration = Duration::from_millis(300);

#[derive(Debug, thiserror::Error)]
pub enum PasteError {
    #[error("clipboard: {0}")]
    Clipboard(#[from] arboard::Error),
    #[error(
        "não foi possível simular o atalho de colar: {0}. No macOS, dá acesso em Definições do Sistema → Privacidade e Segurança → Acessibilidade."
    )]
    Input(String),
}

/// O que é preciso para repor o clipboard depois. Devolvido ao chamador para que a
/// reposição aconteça fora do caminho crítico.
pub struct Restore {
    previous: Option<String>,
    pasted: String,
}

/// Coloca `text` no clipboard e envia o atalho de colar. Devolve o tempo gasto e
/// um `Restore` para repor o clipboard anterior mais tarde.
pub fn paste(text: &str) -> Result<(Duration, Restore), PasteError> {
    let started = Instant::now();
    let mut clipboard = Clipboard::new()?;
    // Só sabemos guardar texto; se o clipboard tinha uma imagem, perde-se (fica no log).
    let previous = clipboard.get_text().ok();
    if previous.is_none() {
        tracing::debug!("clipboard anterior não era texto (ou estava vazio); não será reposto");
    }

    set_text(&mut clipboard, text)?;
    let mut enigo =
        Enigo::new(&Settings::default()).map_err(|e| PasteError::Input(e.to_string()))?;
    send_paste_shortcut(&mut enigo).map_err(|e| PasteError::Input(e.to_string()))?;

    Ok((
        started.elapsed(),
        Restore {
            previous,
            pasted: text.to_owned(),
        },
    ))
}

#[cfg(target_os = "windows")]
fn set_text(clipboard: &mut Clipboard, text: &str) -> Result<(), arboard::Error> {
    use arboard::SetExtWindows;
    // O ditado não fica no histórico do Win+V nem na sincronização na nuvem.
    clipboard
        .set()
        .exclude_from_history()
        .exclude_from_cloud()
        .text(text)
}

#[cfg(not(target_os = "windows"))]
fn set_text(clipboard: &mut Clipboard, text: &str) -> Result<(), arboard::Error> {
    clipboard.set_text(text)
}

fn send_paste_shortcut(enigo: &mut Enigo) -> enigo::InputResult<()> {
    // No modo alternar, o utilizador pode ainda estar a segurar Shift do atalho
    // (Ctrl+Shift+Espaço) quando o texto chega; Ctrl+Shift+V faz outra coisa em
    // muitas apps. Largar o Shift antes de colar é inofensivo.
    enigo.key(Key::Shift, Direction::Release)?;

    #[cfg(target_os = "macos")]
    let (modifier, v) = (Key::Meta, Key::Other(9)); // kVK_ANSI_V: independente do layout
    #[cfg(target_os = "windows")]
    let (modifier, v) = (Key::Control, Key::V); // tecla virtual, independente do layout
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let (modifier, v) = (Key::Control, Key::Unicode('v'));

    enigo.key(modifier, Direction::Press)?;
    let result = enigo.key(v, Direction::Click);
    // Largar o modificador mesmo que o V falhe, para não deixar a tecla "presa".
    enigo.key(modifier, Direction::Release)?;
    result
}

impl Restore {
    /// Repõe o clipboard anterior, mas só se ainda tiver o nosso texto: se entretanto
    /// copiaste outra coisa, não a apagamos.
    pub fn run(self) {
        let Some(previous) = self.previous else {
            return;
        };
        let result = Clipboard::new().and_then(|mut cb| {
            if cb.get_text().ok().as_deref() == Some(self.pasted.as_str()) {
                cb.set_text(previous)
            } else {
                tracing::debug!("clipboard mudou entretanto; não foi reposto");
                Ok(())
            }
        });
        if let Err(e) = result {
            tracing::warn!(error = %e, "não foi possível repor o clipboard anterior");
        }
    }
}
