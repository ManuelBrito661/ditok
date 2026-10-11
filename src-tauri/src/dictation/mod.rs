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
    stt::SttProvider,
    tray::{Tray, TrayState},
};
use machine::{Action, Input, Machine, Phase};
use pipeline::Pipeline;

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
}

/// O que o orquestrador recebe do resto da app. Agrupar num struct evita funções
/// com muitos argumentos e deixa claro o que passa a pertencer à tarefa.
pub struct Deps {
    pub tray: Tray,
    pub recorder: Recorder,
    pub pipeline: Arc<Pipeline>,
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
    let app = deps.pipeline.app.clone();
    // Criado já se a chave existir; senão, volta a tentar-se a cada ditado (assim
    // podes guardar a chave sem reiniciar a app).
    let mut stt: Option<Arc<dyn SttProvider>> = load_provider(&deps.pipeline);

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
                deps.pipeline.notify(&reason);
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
                if stt.is_none() {
                    stt = load_provider(&deps.pipeline);
                }
                let Some(provider) = &stt else {
                    // Sem chave não vale a pena gravar: volta a "parado" e explica porquê.
                    machine.handle(Input::Cancel, at);
                    deps.pipeline.notify(&deps.pipeline.missing_key_message());
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

                let Some(provider) = stt.clone() else {
                    // Não acontece (sem provider não se começa a gravar), mas o
                    // compilador obriga-nos a decidir o que fazer.
                    let _ = tx.send(DictationEvent::ProcessingFinished { ok: false });
                    continue;
                };
                // O processamento corre numa tarefa à parte para o orquestrador
                // continuar a responder (ex.: ignorar premires enquanto processa).
                let pipeline = Arc::clone(&deps.pipeline);
                let tx = tx.clone();
                tauri::async_runtime::spawn(async move {
                    let ok = pipeline.process(provider, recording, at).await;
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

fn load_provider(pipeline: &Pipeline) -> Option<Arc<dyn SttProvider>> {
    match pipeline.load_provider() {
        Ok(Some(p)) => {
            tracing::info!(provider = p.name(), "provider de STT pronto");
            Some(p)
        }
        Ok(None) => {
            tracing::warn!("{}", pipeline.missing_key_message());
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
