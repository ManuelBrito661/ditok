//! Perguntar sobre o ecrã: um atalho tira uma fotografia do monitor onde está o cursor, a
//! janela de seleção mostra-a "congelada", escolhes a área e escreves (ou dizes) o que queres
//! saber, e a área segue para o modelo com a pergunta. A resposta abre no assistente, que
//! continua a conversa sobre a mesma imagem.
//!
//! O caminho:
//! 1. `start` (atalho, menu do tray): permissões, fotografia (`grab`), sessão e janela
//!    (`window`). A fotografia fica na memória, em `Screen`, e é a única cópia.
//! 2. A página (`screen.ts`) pede-a (`screen_image`), desenha-a, e diz que está pronta
//!    (`screen_ready`): só então a janela aparece.
//! 3. O texto da pergunta escreve-se na caixa ou dita-se: o microfone é o do ditado
//!    (`DictationEvent::Click` ou o atalho de sempre), mas com a janela aberta o texto vem
//!    para a caixa (`screen:transcript`) em vez de ser colado numa app.
//! 4. `submit`: a área é convertida em píxeis da imagem (`shot`), recortada e codificada, a
//!    janela fecha e o assistente começa a responder (`assistant::start_screen`).
//!
//! A fotografia nunca vai para o disco. Só se envia à xAI o recorte escolhido.

pub mod grab;
pub mod shot;
pub mod window;

use std::{
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use image::RgbaImage;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    assistant::{self, ScreenStart},
    focus, hotkey,
    state::{AppState, MISSING_KEY},
    xai::chat::Image,
};
use grab::Bounds;
use shot::{Area, Viewport};

/// A janela fica aberta, no máximo, este tempo. Se te esqueceres dela, fecha sozinha.
const MAX_OPEN: Duration = Duration::from_secs(10 * 60);
/// A página tem de dizer que desenhou a fotografia neste tempo; senão desiste-se.
const READY_WITHIN: Duration = Duration::from_secs(6);

const EVENT_TRANSCRIPT: &str = "screen:transcript";
const EVENT_PARTIAL: &str = "screen:partial";

#[cfg(target_os = "macos")]
const NO_ACCESS: &str = "O Ditok precisa da permissão «Gravação do ecrã» para ver o ecrã. Dá-lha em Definições do Sistema → Privacidade e Segurança → Gravação do ecrã e do áudio do sistema; depois fecha e abre o Ditok.";
#[cfg(not(target_os = "macos"))]
const NO_ACCESS: &str = "O sistema não deixou o Ditok ver o ecrã.";

/// Uma captura em curso: a fotografia inteira do monitor e de onde veio o pedido.
struct Session {
    id: u64,
    image: Arc<RgbaImage>,
    /// O JPEG que a página mostra. Começa a ser codificado assim que a captura existe
    /// (`prepare`), enquanto o WebView carrega; quem o pedir a meio espera por ele, em vez de o
    /// codificar outra vez.
    display: Arc<OnceLock<Option<Arc<Vec<u8>>>>>,
    /// A app onde estavas: "Inserir no cursor" devolve-lhe a resposta.
    target: focus::Target,
}

#[derive(Default)]
pub struct Screen {
    session: Mutex<Option<Session>>,
    next: AtomicU64,
}

impl Screen {
    fn lock(&self) -> MutexGuard<'_, Option<Session>> {
        self.session.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn begin(&self, image: RgbaImage, target: focus::Target) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        *self.lock() = Some(Session {
            id,
            image: Arc::new(image),
            display: Arc::default(),
            target,
        });
        id
    }

    /// Esquece a fotografia (liberta ~30 MB num ecrã 4K).
    fn clear(&self) {
        *self.lock() = None;
    }

    /// Esquece a fotografia, se for a da captura `id`.
    fn clear_if(&self, id: u64) {
        let mut session = self.lock();
        if session.as_ref().is_some_and(|s| s.id == id) {
            *session = None;
        }
    }

    fn id(&self) -> Option<u64> {
        self.lock().as_ref().map(|s| s.id)
    }

    fn image(&self) -> Option<Arc<RgbaImage>> {
        self.lock().as_ref().map(|s| Arc::clone(&s.image))
    }

    fn target(&self) -> Option<focus::Target> {
        self.lock().as_ref().map(|s| s.target.clone())
    }

    /// A fotografia e o sítio onde o JPEG para a página fica guardado.
    fn display_parts(&self) -> Option<(Arc<RgbaImage>, Arc<OnceLock<Option<Arc<Vec<u8>>>>>)> {
        let guard = self.lock();
        let session = guard.as_ref()?;
        Some((Arc::clone(&session.image), Arc::clone(&session.display)))
    }
}

/// O que a página precisa para se desenhar (`screen_view`).
#[derive(Debug, Serialize)]
pub struct View {
    /// Tamanho, em píxeis, da fotografia.
    width: u32,
    height: u32,
    /// O atalho de ditar, legível ("Ctrl+Win"), para a dica.
    dictate: String,
}

// ---------------------------------------------------------------- começar e acabar

/// O atalho (ou o menu) pediu uma captura. `settle`: quanto esperar antes de fotografar, para
/// um menu que acabou de fechar não aparecer na fotografia. Se já houver uma aberta, fecha-a:
/// o mesmo atalho abre e fecha.
pub fn start(app: &AppHandle, settle: Duration) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(message) = begin(&app, settle) {
            tracing::warn!(error = %message, "a captura do ecrã não começou");
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                state.notify(&message);
            }
        }
    });
}

fn begin(app: &AppHandle, settle: Duration) -> Result<(), String> {
    let state = app
        .try_state::<Arc<AppState>>()
        .ok_or("O Ditok ainda está a arrancar.")?;
    let state = Arc::clone(&state);
    if window::is_open(app) {
        close(&state);
        return Ok(());
    }
    // A chave pode ter sido trocada fora da app: segue-se o cofre, como num ditado.
    state.refresh_key();
    if !state.key_set() {
        return Err(MISSING_KEY.to_owned());
    }
    assistant::check_screen(&state)?;
    if !grab::ensure_access() {
        return Err(NO_ACCESS.to_owned());
    }
    if !settle.is_zero() {
        std::thread::sleep(settle);
    }
    // A janela onde estavas, antes de o Ditok fazer o que quer que seja.
    let target = focus::Target::of(focus::foreground());

    let cursor = app.cursor_position().map_err(|e| e.to_string())?;
    let monitor = app
        .monitor_from_point(cursor.x, cursor.y)
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten())
        .ok_or("Não encontrei o monitor onde estás.")?;
    let bounds = Bounds {
        x: monitor.position().x,
        y: monitor.position().y,
        width: monitor.size().width,
        height: monitor.size().height,
    };
    let started = std::time::Instant::now();
    let image = grab::grab(bounds).map_err(|e| e.to_string())?;
    tracing::info!(
        width = image.width(),
        height = image.height(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "ecrã fotografado"
    );

    let id = state.screen.begin(image, target);
    // Criar o WebView demora; o JPEG do ecrã codifica-se entretanto (~0,1 a 0,5 s, conforme o ecrã).
    let preparing = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let _ = display_image(&preparing);
    });
    if let Err(e) = window::open(app, &monitor, id) {
        state.screen.clear();
        return Err(e);
    }
    watch(app.clone(), id);
    Ok(())
}

/// Fecha a captura se a página nunca a mostrar, ou se ficar aberta tempo demais.
fn watch(app: AppHandle, id: u64) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(READY_WITHIN).await;
        if same_session(&app, id) && !window::is_revealed() {
            tracing::error!("a página da captura do ecrã não chegou a mostrar-se");
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                state.notify("Não foi possível abrir a captura do ecrã.");
            }
            cancel(&app);
            return;
        }
        tokio::time::sleep(MAX_OPEN.saturating_sub(READY_WITHIN)).await;
        if same_session(&app, id) {
            tracing::info!("a captura do ecrã ficou aberta demasiado tempo; a fechar");
            cancel(&app);
        }
    });
}

fn same_session(app: &AppHandle, id: u64) -> bool {
    app.try_state::<Arc<AppState>>()
        .is_some_and(|state| state.screen.id() == Some(id))
}

/// Fecha a captura sem enviar nada.
pub fn cancel(app: &AppHandle) {
    if let Some(state) = app.try_state::<Arc<AppState>>() {
        close(&state);
    } else {
        window::close(app);
    }
}

fn close(state: &AppState) {
    state.screen.clear();
    window::close(&state.app);
}

/// A janela da captura `id` foi destruída (por nós ou de outra maneira): a fotografia vai-se
/// embora. Só a dessa captura: o aviso pode chegar depois de outra já ter começado.
fn forget(app: &AppHandle, id: u64) {
    if let Some(state) = app.try_state::<Arc<AppState>>() {
        state.screen.clear_if(id);
    }
}

// ---------------------------------------------------------------- a página pede

pub fn view(state: &AppState) -> Option<View> {
    let image = state.screen.image()?;
    Some(View {
        width: image.width(),
        height: image.height(),
        dictate: hotkey::pretty(&state.config().shortcut),
    })
}

/// Bloqueante. A fotografia em JPEG, para a página a mostrar. Só se codifica uma vez por
/// captura: um pedido que chegue a meio espera pelo que já está a correr.
pub fn display_image(state: &AppState) -> Result<Arc<Vec<u8>>, String> {
    let (image, cell) = state
        .screen
        .display_parts()
        .ok_or("A captura já não existe.")?;
    cell.get_or_init(|| {
        shot::encode_display(&image)
            .map_err(|e| tracing::error!(error = %e, "não foi possível codificar o ecrã"))
            .ok()
            .map(Arc::new)
    })
    .clone()
    .ok_or_else(|| "Não foi possível preparar a captura do ecrã.".to_owned())
}

/// A página desenhou a fotografia: mostra a janela.
pub fn reveal(app: &AppHandle) {
    window::reveal(app);
}

/// Envia a área escolhida e a pergunta. Em caso de erro a janela fica aberta, para não
/// perderes a seleção nem o que escreveste.
pub async fn submit(
    state: &Arc<AppState>,
    prompt: String,
    area: Area,
    viewport: Viewport,
    spoken: bool,
) -> Result<(), String> {
    let image = state.screen.image().ok_or("A captura já não existe.")?;
    let rect = shot::to_pixels(area, viewport, (image.width(), image.height()))
        .map_err(|e| e.to_string())?;
    // Recortar, reduzir e codificar são centenas de milissegundos num ecrã grande: a miniatura
    // codifica-se ao mesmo tempo que a imagem para o modelo.
    let (encoded, thumb) = tauri::async_runtime::spawn_blocking(move || {
        let cropped = shot::crop(&image, rect);
        std::thread::scope(|scope| {
            let thumb = scope.spawn(|| shot::encode_thumbnail(&cropped));
            let encoded = shot::encode_for_model(&cropped);
            let thumb = thumb
                .join()
                .expect("a miniatura não devia entrar em pânico");
            Ok::<_, shot::ShotError>((encoded?, thumb?))
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    // Antes de fechar: se faltar a chave ou o modelo, a pessoa vê o motivo com a captura
    // ainda aberta.
    assistant::check_screen(state)?;
    let target = state.screen.target().unwrap_or_default();
    tracing::info!(
        width = encoded.width,
        height = encoded.height,
        bytes = encoded.bytes.len(),
        mime = encoded.mime,
        prompt_chars = prompt.chars().count(),
        spoken,
        "captura do ecrã enviada"
    );
    // A janela fecha primeiro: o assistente abre a seguir e fica com o foco.
    close(state);
    assistant::start_screen(
        state,
        ScreenStart {
            instruction: prompt,
            spoken,
            size: (encoded.width, encoded.height),
            image: Image {
                mime: encoded.mime,
                data: Arc::new(encoded.bytes),
            },
            thumb,
            app: target.app,
            origin: target.origin,
        },
    )
}

// ---------------------------------------------------------------- ditar a pergunta

/// O texto ditado, para a caixa de pergunta da janela.
pub fn emit_transcript(app: &AppHandle, text: &str) {
    let _ = app.emit_to(window::LABEL, EVENT_TRANSCRIPT, text);
}

/// O que o microfone já ouviu, enquanto se fala.
pub fn emit_partial(app: &AppHandle, text: &str) {
    let _ = app.emit_to(window::LABEL, EVENT_PARTIAL, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(w: u32, h: u32) -> RgbaImage {
        RgbaImage::new(w, h)
    }

    #[test]
    fn a_new_session_replaces_the_old_one_and_clear_forgets_it() {
        let screen = Screen::default();
        assert!(screen.image().is_none() && screen.id().is_none());
        let first = screen.begin(pixels(4, 4), focus::Target::default());
        let second = screen.begin(pixels(8, 6), focus::Target::default());
        assert!(second > first, "cada captura tem o seu número");
        assert_eq!(screen.image().unwrap().width(), 8);
        screen.clear();
        assert!(screen.image().is_none() && screen.target().is_none());
    }

    #[test]
    fn a_late_destroyed_event_does_not_wipe_a_newer_capture() {
        let screen = Screen::default();
        let old = screen.begin(pixels(4, 4), focus::Target::default());
        let new = screen.begin(pixels(8, 8), focus::Target::default());
        screen.clear_if(old);
        assert_eq!(screen.id(), Some(new), "a nova continua");
        screen.clear_if(new);
        assert!(screen.id().is_none());
    }

    #[test]
    fn each_session_has_its_own_display_slot() {
        let screen = Screen::default();
        screen.begin(pixels(4, 4), focus::Target::default());
        let (_, old) = screen.display_parts().unwrap();
        old.get_or_init(|| Some(Arc::new(vec![1])));
        // Uma captura nova começa sem JPEG: o da antiga não lhe serve.
        screen.begin(pixels(4, 4), focus::Target::default());
        let (_, fresh) = screen.display_parts().unwrap();
        assert!(fresh.get().is_none());
        assert_eq!(old.get().unwrap().as_deref().unwrap().as_slice(), [1]);
        screen.clear();
        assert!(screen.display_parts().is_none());
    }
}
