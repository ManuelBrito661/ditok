//! Colar texto na app em foco: guardar o clipboard → pôr o texto → simular
//! Ctrl+V / Cmd+V → (mais tarde, se a opção estiver desligada) repor o clipboard
//! anterior. Também copia texto sem colar (botão "Copiar" do histórico).
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
    // Este erro só acontece depois de o texto já estar no clipboard.
    #[error(
        "não foi possível colar ({0}), mas o texto ficou no clipboard, pronto a colar. {hint}",
        hint = INPUT_HELP
    )]
    Input(String),
}

/// O que fazer quando o sistema não deixa simular o atalho de colar.
#[cfg(target_os = "macos")]
const INPUT_HELP: &str =
    "Dá acesso ao Ditok em Definições do Sistema → Privacidade e Segurança → Acessibilidade.";
// No Windows não há permissão para isto; o caso típico é a app de destino correr
// como administrador, e o Windows não deixa uma app normal enviar-lhe teclas.
#[cfg(target_os = "windows")]
const INPUT_HELP: &str = "Se a app de destino corre como administrador, o Windows não deixa o Ditok colar lá: cola com Ctrl+V.";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const INPUT_HELP: &str = "Cola com Ctrl+V.";

/// Quanto tempo, no máximo, esperamos que largues os modificadores do atalho
/// antes de colar.
const MODIFIERS_WAIT: Duration = Duration::from_millis(300);

/// Põe `text` no clipboard sem colar.
pub fn copy(text: &str) -> Result<(), PasteError> {
    Ok(set_text(&mut Clipboard::new()?, text)?)
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
    // No modo alternar, o utilizador pode ainda estar a segurar modificadores do
    // atalho (Ctrl+Shift+Espaço) quando o texto chega: Ctrl+Shift+V ou Ctrl+Alt+V
    // fazem outra coisa em muitas apps, e Win+Ctrl+V abre um menu do Windows.
    release_modifiers(enigo)?;

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

/// Windows: espera (até `MODIFIERS_WAIT`) que nenhum modificador esteja premido
/// e só "larga" à força os que continuarem em baixo. Enviar um "largar" de Alt ou
/// Win sem necessidade não é inofensivo (pode abrir menus), daí perguntarmos antes.
#[cfg(target_os = "windows")]
fn release_modifiers(enigo: &mut Enigo) -> enigo::InputResult<()> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    const MODIFIERS: [(VIRTUAL_KEY, Key); 5] = [
        (VK_SHIFT, Key::Shift),
        (VK_CONTROL, Key::Control),
        (VK_MENU, Key::Alt),
        (VK_LWIN, Key::LWin),
        (VK_RWIN, Key::RWin),
    ];
    // O bit mais alto (valor negativo em `i16`) quer dizer "premida agora".
    // SAFETY: `GetAsyncKeyState` só lê o estado do teclado.
    let held = |vk: VIRTUAL_KEY| unsafe { GetAsyncKeyState(i32::from(vk)) } < 0;

    let deadline = Instant::now() + MODIFIERS_WAIT;
    while MODIFIERS.iter().any(|&(vk, _)| held(vk)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    // Largar o Alt ou o Win "sozinhos" faz o Windows abrir a barra de menus da
    // app ou o menu Iniciar. O truque habitual (é o que o AutoHotkey faz) é
    // carregar antes numa tecla "máscara" sem efeito: 0xE8 é um código virtual
    // não atribuído, por isso o Windows deixa de ver o Alt/Win como premido sozinho.
    const MASK_KEY: u32 = 0xE8;
    if held(VK_MENU) || held(VK_LWIN) || held(VK_RWIN) {
        enigo.key(Key::Other(MASK_KEY), Direction::Click)?;
    }
    for (vk, key) in MODIFIERS {
        if held(vk) {
            tracing::debug!(?key, "modificador ainda premido; a largá-lo antes de colar");
            enigo.key(key, Direction::Release)?;
        }
    }
    Ok(())
}

/// macOS (e outros): não há forma simples de ler o estado das teclas sem mais
/// dependências, e aqui um "largar" a mais é inofensivo. O Cmd fica de fora
/// porque é premido logo a seguir para colar.
#[cfg(not(target_os = "windows"))]
fn release_modifiers(enigo: &mut Enigo) -> enigo::InputResult<()> {
    let _ = MODIFIERS_WAIT; // só usado no Windows
    for key in [Key::Shift, Key::Control, Key::Alt] {
        enigo.key(key, Direction::Release)?;
    }
    Ok(())
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
