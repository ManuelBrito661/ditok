//! Colar texto na app em foco: guardar o clipboard → pôr o texto → simular
//! Ctrl+V / Cmd+V → (mais tarde, se a opção estiver desligada) repor o clipboard
//! anterior. Também copia texto sem colar (botão "Copiar" do histórico).
//!
//! Tudo aqui é bloqueante (APIs do sistema), por isso é chamado a partir de
//! `spawn_blocking` e nunca diretamente numa tarefa assíncrona.

use std::time::{Duration, Instant};

use arboard::{Clipboard, ImageData};
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

/// O que estava no clipboard antes de o Ditok lhe mexer: texto, ou uma imagem
/// quando não havia texto. Vazio (nem uma coisa nem outra) também é um valor.
pub struct Saved {
    text: Option<String>,
    image: Option<ImageData<'static>>,
}

/// Lê o que está no clipboard para o poder repor. A imagem só se guarda quando
/// não há texto (uma imagem grande ocupa muita memória).
fn read_saved(clipboard: &mut Clipboard) -> Saved {
    let text = clipboard.get_text().ok().filter(|t| !t.is_empty());
    let image = match text {
        Some(_) => None,
        None => clipboard.get_image().ok(),
    };
    Saved { text, image }
}

/// O que é preciso para repor o clipboard depois. Devolvido ao chamador para que a
/// reposição aconteça fora do caminho crítico.
pub struct Restore {
    previous: Saved,
    /// O que o clipboard tem enquanto é "nosso" (o texto colado, ou a seleção
    /// copiada no modo comando). `None` = vazio ou sem texto.
    ours: Option<String>,
}

/// O que estava no clipboard antes de o Ditok lhe mexer.
pub enum Before {
    /// Lê-o agora (o ditado normal, ou quando nada foi copiado).
    ReadNow,
    /// Já foi lido antes (o modo comando lê-o antes de copiar a seleção).
    Known(Saved),
}

/// Coloca `text` no clipboard e envia o atalho de colar. Devolve o tempo gasto e
/// um `Restore` para repor o clipboard anterior mais tarde.
pub fn paste(text: &str, before: Before) -> Result<(Duration, Restore), PasteError> {
    let started = Instant::now();
    let mut clipboard = Clipboard::new()?;
    let previous = match before {
        Before::ReadNow => read_saved(&mut clipboard),
        Before::Known(previous) => previous,
    };
    if previous.text.is_none() && previous.image.is_none() {
        tracing::debug!("clipboard anterior vazio ou de tipo não suportado; fica vazio ao repor");
    }

    set_text(&mut clipboard, text)?;
    let mut enigo =
        Enigo::new(&Settings::default()).map_err(|e| PasteError::Input(e.to_string()))?;
    send_shortcut(&mut enigo, Letter::V).map_err(|e| PasteError::Input(e.to_string()))?;

    Ok((
        started.elapsed(),
        Restore {
            previous,
            ours: Some(text.to_owned()),
        },
    ))
}

/// Quanto tempo esperamos que a app copie a seleção depois do Ctrl+C. As apps
/// respondem em poucos milissegundos; sem seleção, não copiam nada e esperamos
/// isto tudo.
const COPY_WAIT: Duration = Duration::from_millis(300);
/// De quanto em quanto tempo olhamos para o clipboard enquanto esperamos.
const COPY_POLL: Duration = Duration::from_millis(15);

/// O texto selecionado na app em foco (modo comando) e o que estava no
/// clipboard antes, para o repor no fim.
pub struct Captured {
    /// `None`: não havia seleção (ou não era texto).
    pub text: Option<String>,
    /// O que ficou no clipboard depois da cópia (pode ser só espaços: é isso que
    /// se repõe, por isso compara-se com o valor bruto).
    ours: Option<String>,
    /// `None`: o clipboard nem chegou a ser mexido (nada foi copiado).
    saved: Option<Saved>,
}

impl Captured {
    /// Nada foi copiado (ex.: a janela mudou): o clipboard não foi tocado.
    pub fn nothing() -> Self {
        Self {
            text: None,
            ours: None,
            saved: None,
        }
    }

    /// Como colar o resultado: com o clipboard de antes da cópia (para o repor
    /// depois), ou lendo o atual se a cópia nunca mexeu nele.
    pub fn into_before(self) -> Before {
        match self.saved {
            Some(saved) => Before::Known(saved),
            None => Before::ReadNow,
        }
    }

    /// O comando não chegou a colar (falhou, foi cancelado…): repõe o clipboard
    /// de antes. Bloqueante.
    pub fn restore(self) {
        if let Some(previous) = self.saved {
            Restore {
                previous,
                ours: self.ours,
            }
            .run();
        }
    }
}

/// Copia o texto selecionado na app em foco: guarda o clipboard, esvazia-o,
/// simula Ctrl+C (Cmd+C) e espera, até `COPY_WAIT`, que a app lá ponha alguma
/// coisa. O clipboard fica com a seleção: quem chama tem de o repor no fim
/// (`Captured::restore` ou `paste` com `Before::Known`).
///
/// Porquê esvaziar primeiro: é a forma mais simples (e igual nas duas
/// plataformas) de saber se a app copiou alguma coisa. Sem seleção, a maior
/// parte das apps não mexe no clipboard, que continua vazio.
pub fn capture_selection() -> Result<Captured, PasteError> {
    // O Enigo cria-se antes de mexer no clipboard: se falhar, o clipboard fica como estava.
    let mut enigo =
        Enigo::new(&Settings::default()).map_err(|e| PasteError::Input(e.to_string()))?;
    let mut clipboard = Clipboard::new()?;
    let saved = read_saved(&mut clipboard);
    clipboard.clear()?;
    if let Err(e) = send_shortcut(&mut enigo, Letter::C) {
        // Sem Ctrl+C não há seleção; o clipboard volta ao que era.
        Captured {
            text: None,
            ours: None,
            saved: Some(saved),
        }
        .restore();
        return Err(PasteError::Input(e.to_string()));
    }
    let deadline = Instant::now() + COPY_WAIT;
    // O último valor não vazio que lemos (também se for só espaços).
    let mut last = None;
    loop {
        // A app pode ter o clipboard aberto a meio da cópia: um erro aqui só
        // quer dizer "ainda não", por isso tenta-se outra vez.
        if let Some(t) = clipboard.get_text().ok().filter(|t| !t.is_empty()) {
            last = Some(t);
            if last.as_deref().is_some_and(|t| !t.trim().is_empty()) {
                break;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(COPY_POLL);
    }
    // Só espaços não é seleção, mas o clipboard tem-nos, e é isso que se repõe.
    let text = last.as_ref().filter(|t| !t.trim().is_empty()).cloned();
    tracing::debug!(
        chars = text.as_ref().map(|t| t.chars().count()),
        "seleção copiada"
    );
    Ok(Captured {
        text,
        ours: last,
        saved: Some(saved),
    })
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

/// A letra do atalho a simular: V para colar, C para copiar.
#[derive(Debug, Clone, Copy)]
enum Letter {
    C,
    V,
}

/// A tecla de cada letra, independente do layout do teclado.
#[cfg(target_os = "macos")]
fn letter_key(letter: Letter) -> Key {
    // Códigos virtuais do macOS: kVK_ANSI_C = 8, kVK_ANSI_V = 9.
    match letter {
        Letter::C => Key::Other(8),
        Letter::V => Key::Other(9),
    }
}
#[cfg(target_os = "windows")]
fn letter_key(letter: Letter) -> Key {
    // Teclas virtuais (VK_C, VK_V): independentes do layout.
    match letter {
        Letter::C => Key::C,
        Letter::V => Key::V,
    }
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn letter_key(letter: Letter) -> Key {
    match letter {
        Letter::C => Key::Unicode('c'),
        Letter::V => Key::Unicode('v'),
    }
}

/// Simula Ctrl+letra (Cmd+letra no macOS).
fn send_shortcut(enigo: &mut Enigo, letter: Letter) -> enigo::InputResult<()> {
    // No modo alternar, o utilizador pode ainda estar a segurar modificadores do
    // atalho (Ctrl+Shift+Espaço) quando o texto chega: Ctrl+Shift+V ou Ctrl+Alt+V
    // fazem outra coisa em muitas apps, e Win+Ctrl+V abre um menu do Windows.
    release_modifiers(enigo)?;

    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modifier = Key::Control;
    let v = letter_key(letter);

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
        let result = Clipboard::new().and_then(|mut cb| {
            // Vazio (ou sem texto) conta como `None`, tal como em `ours`.
            let now = cb.get_text().ok().filter(|t| !t.is_empty());
            if now != self.ours {
                tracing::debug!("clipboard mudou entretanto; não foi reposto");
                return Ok(());
            }
            let Saved { text, image } = self.previous;
            match (text, image) {
                (Some(text), _) => cb.set_text(text),
                (None, Some(image)) => cb.set_image(image),
                // Estava vazio: o nosso texto não pode ficar lá.
                (None, None) => cb.clear(),
            }
        });
        if let Err(e) = result {
            tracing::warn!(error = %e, "não foi possível repor o clipboard anterior");
        }
    }
}
