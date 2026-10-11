//! Orquestrador do ditado: uma única tarefa assíncrona que recebe eventos por um
//! canal e é a única dona do estado (máquina de estados, tray, provider de STT).
//!
//! Porquê assim (padrão "actor"): como só esta tarefa toca no estado, não precisamos
//! de `Mutex` nem de nos preocupar com duas threads a mudá-lo ao mesmo tempo. Quem
//! quer influenciar o ditado (atalho, áudio, temporizadores, processamento) envia
//! uma mensagem. O `rx.recv().await` adormece a tarefa até chegar uma mensagem:
//! não há polling.

pub mod machine;
pub mod pipeline;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::{
    audio::Recorder,
    hotkey,
    state::{AppState, MISSING_KEY},
    stt::{SttProvider, openrouter::OpenRouter},
    tray::{Tray, TrayState},
};
use machine::{Action, Input, Machine, Phase};

/// Quanto tempo o ícone "pronto" fica visível depois de colar.
const DONE_ICON_FOR: Duration = Duration::from_millis(1200);

#[derive(Debug)]
pub enum DictationEvent {
    Hotkey {
        pressed: bool,
        at: Instant,
    },
    Cancel,
    /// O microfone não abriu ou desligou-se a meio.
    AudioFailed(String),
    ProcessingFinished {
        ok: bool,
    },
    DoneIconElapsed,
    /// A chave ou o modo de envio mudaram nas definições: o cliente do OpenRouter
    /// tem de ser recriado no próximo ditado.
    ProviderChanged,
}

/// O que o orquestrador recebe do resto da app. Agrupar num struct evita funções
/// com muitos argumentos e deixa claro o que passa a pertencer à tarefa.
pub struct Deps {
    pub tray: Tray,
    pub recorder: Recorder,
    pub state: Arc<AppState>,
}

pub fn spawn(
    deps: Deps,
    tx: UnboundedSender<DictationEvent>,
    rx: UnboundedReceiver<DictationEvent>,
) {
    // `deps` e `rx` são movidos para dentro da tarefa: a partir daqui pertencem-lhe.
    tauri::async_runtime::spawn(run(deps, tx, rx));
}

async fn run(
    mut deps: Deps,
    tx: UnboundedSender<DictationEvent>,
    mut rx: UnboundedReceiver<DictationEvent>,
) {
    let mut machine = Machine::default();
    let app = deps.state.app.clone();
    // Criado já se a chave existir; senão, volta a tentar-se a cada ditado (assim
    // podes guardar a chave sem reiniciar a app).
    let mut openrouter: Option<Arc<OpenRouter>> = load_openrouter(&deps.state);

    while let Some(event) = rx.recv().await {
        let (input, at) = match event {
            DictationEvent::Hotkey { pressed: true, at } => (Input::Pressed, at),
            DictationEvent::Hotkey { pressed: false, at } => (Input::Released, at),
            DictationEvent::Cancel => (Input::Cancel, Instant::now()),
            DictationEvent::AudioFailed(reason) => {
                tracing::error!(%reason, "falha de áudio");
                // Para a máquina é igual a cancelar; depois mostramos o motivo.
                if machine.handle(Input::Cancel, Instant::now()).is_some() {
                    hotkey::set_cancel_enabled(&app, false);
                    deps.recorder.cancel();
                }
                deps.tray.set(TrayState::Idle);
                deps.state.notify(&reason);
                continue;
            }
            DictationEvent::ProviderChanged => {
                // Um ditado em curso já tem o seu `Arc` e termina com o cliente antigo;
                // o próximo cria um novo.
                openrouter = None;
                continue;
            }
            DictationEvent::ProcessingFinished { ok } => {
                let before = machine.phase();
                machine.handle(Input::ProcessingFinished, Instant::now());
                if before == Phase::Processing {
                    finish_processing(&mut deps.tray, &tx, ok);
                }
                continue;
            }
            DictationEvent::DoneIconElapsed => {
                // Só volta a "parado" se entretanto não começou outro ditado.
                if deps.tray.state() == TrayState::Done {
                    deps.tray.set(TrayState::Idle);
                }
                continue;
            }
        };

        let before = machine.phase();
        match machine.handle(input, at) {
            Some(Action::StartRecording) => {
                if openrouter.is_none() {
                    openrouter = load_openrouter(&deps.state);
                }
                let Some(provider) = &openrouter else {
                    // Sem chave não vale a pena gravar: volta a "parado" e explica porquê.
                    machine.handle(Input::Cancel, at);
                    deps.state.notify(MISSING_KEY);
                    continue;
                };
                // Primeiro o microfone (é o que tem latência), depois a ligação HTTP
                // (corre enquanto falas), depois o resto.
                deps.recorder.start(at);
                let provider = Arc::clone(provider);
                tauri::async_runtime::spawn(async move { provider.prewarm().await });
                deps.tray.set(TrayState::Recording);
                hotkey::set_cancel_enabled(&app, true);
                tracing::info!("a gravar");
            }
            Some(Action::StopAndProcess) => {
                // `stop()` envia já o pedido à thread de áudio; o future que devolve
                // só entrega a gravação quando estiver pronta.
                let recording = deps.recorder.stop();
                hotkey::set_cancel_enabled(&app, false);
                deps.tray.set(TrayState::Processing);

                let Some(provider) = openrouter.clone() else {
                    // Só acontece se a chave mudou a meio da gravação; o compilador
                    // obriga-nos a decidir o que fazer.
                    deps.recorder.cancel();
                    let _ = tx.send(DictationEvent::ProcessingFinished { ok: false });
                    continue;
                };
                // O processamento corre numa tarefa à parte para o orquestrador
                // continuar a responder (ex.: ignorar premires enquanto processa).
                let state = Arc::clone(&deps.state);
                let tx = tx.clone();
                tauri::async_runtime::spawn(async move {
                    let ok = pipeline::process(state, provider, recording, at).await;
                    let _ = tx.send(DictationEvent::ProcessingFinished { ok });
                });
            }
            Some(Action::CancelRecording) => {
                deps.recorder.cancel();
                hotkey::set_cancel_enabled(&app, false);
                deps.tray.set(TrayState::Idle);
                tracing::info!("gravação cancelada");
            }
            None if machine.phase() == Phase::Latched && before != Phase::Latched => {
                tracing::info!("modo alternar: prime o atalho outra vez para terminar");
            }
            None => tracing::trace!(?input, phase = ?machine.phase(), "evento ignorado"),
        }
    }
    tracing::debug!("orquestrador terminado");
}

fn load_openrouter(state: &AppState) -> Option<Arc<OpenRouter>> {
    match state.load_openrouter() {
        Ok(Some(p)) => {
            tracing::info!(provider = p.name(), "provider de STT pronto");
            Some(p)
        }
        Ok(None) => {
            tracing::warn!("{MISSING_KEY}");
            None
        }
        Err(e) => {
            tracing::error!(error = %e, "não foi possível ler a chave do cofre");
            None
        }
    }
}

fn finish_processing(tray: &mut Tray, tx: &UnboundedSender<DictationEvent>, ok: bool) {
    if !ok {
        tray.set(TrayState::Idle);
        return;
    }
    tray.set(TrayState::Done);
    let tx = tx.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(DONE_ICON_FOR).await;
        let _ = tx.send(DictationEvent::DoneIconElapsed);
    });
}
