//! Orquestrador do ditado: uma única tarefa assíncrona que recebe eventos por um
//! canal e é a única dona do estado (máquina de estados + tray).
//!
//! Porquê assim (padrão "actor"): como só esta tarefa toca no estado, não precisamos
//! de `Mutex` nem de nos preocupar com duas threads a mudá-lo ao mesmo tempo. Quem
//! quer influenciar o ditado (atalho, áudio, temporizadores, mais tarde a rede)
//! envia uma mensagem. O `rx.recv().await` adormece a tarefa até chegar uma
//! mensagem: não há polling.

pub mod machine;

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use tauri::AppHandle;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::{
    audio::{Recorder, Recording},
    hotkey,
    tray::{Tray, TrayState},
};
use machine::{Action, Input, Machine, Phase};

/// Quanto tempo o ícone "pronto" fica visível depois de colar.
const DONE_ICON_FOR: Duration = Duration::from_millis(1200);
/// Gravações mais curtas do que isto são quase de certeza toques acidentais.
const MIN_AUDIO: Duration = Duration::from_millis(250);

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

/// O que o orquestrador precisa do resto da app. Agrupar num struct evita funções
/// com muitos argumentos e deixa claro o que pertence à tarefa.
pub struct Deps {
    pub app: AppHandle,
    pub tray: Tray,
    pub recorder: Recorder,
    /// Onde guardar `last.wav` para inspeção.
    pub debug_dir: PathBuf,
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

    while let Some(event) = rx.recv().await {
        let (input, at) = match event {
            DictationEvent::Hotkey { pressed: true, at } => (Input::Pressed, at),
            DictationEvent::Hotkey { pressed: false, at } => (Input::Released, at),
            DictationEvent::Cancel => (Input::Cancel, Instant::now()),
            DictationEvent::AudioFailed(reason) => {
                tracing::error!(%reason, "falha de áudio");
                // Para a máquina é igual a cancelar; depois mostramos o motivo.
                if machine.handle(Input::Cancel, Instant::now()).is_some() {
                    hotkey::set_cancel_enabled(&deps.app, false);
                    deps.recorder.cancel();
                }
                deps.tray.set(TrayState::Idle);
                deps.tray.set_tooltip(&format!("Ditok: {reason}"));
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
                // Primeiro o microfone (é o que tem latência), depois o resto.
                deps.recorder.start(at);
                deps.tray.set(TrayState::Recording);
                hotkey::set_cancel_enabled(&deps.app, true);
                tracing::info!("a gravar");
            }
            Some(Action::StopAndProcess) => {
                // `stop()` envia já o pedido à thread de áudio; o future que devolve
                // só entrega a gravação quando estiver pronta.
                let recording = deps.recorder.stop();
                hotkey::set_cancel_enabled(&deps.app, false);
                deps.tray.set(TrayState::Processing);

                // O processamento corre numa tarefa à parte para o orquestrador
                // continuar a responder (ex.: ignorar premires enquanto processa).
                let tx = tx.clone();
                let debug_dir = deps.debug_dir.clone();
                tauri::async_runtime::spawn(async move {
                    let ok = match recording.await {
                        Ok(rec) => {
                            log_recording(&rec, at);
                            save_debug_wav(debug_dir, &rec).await;
                            true
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "gravação falhou");
                            false
                        }
                    };
                    let _ = tx.send(DictationEvent::ProcessingFinished { ok });
                });
            }
            Some(Action::CancelRecording) => {
                deps.recorder.cancel();
                hotkey::set_cancel_enabled(&deps.app, false);
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

fn finish_processing(tray: &mut Tray, tx: &UnboundedSender<DictationEvent>, ok: bool) {
    if !ok {
        tray.set(TrayState::Idle);
        return;
    }
    tray.set(TrayState::Done);
    tracing::info!("ditado concluído");
    let tx = tx.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(DONE_ICON_FOR).await;
        let _ = tx.send(DictationEvent::DoneIconElapsed);
    });
}

fn log_recording(rec: &Recording, released_at: Instant) {
    let ms = |d: Duration| d.as_millis() as u64;
    tracing::info!(
        audio_ms = ms(rec.audio),
        device = %rec.device,
        device_rate = rec.device_rate,
        device_channels = rec.device_channels,
        mic_start_ms = rec.mic_start.map(ms),
        finalize_ms = ms(rec.finalize),
        release_to_wav_ms = ms(released_at.elapsed()),
        wav_kb = rec.wav.len() / 1024,
        dropped = rec.dropped_samples,
        "gravação pronta"
    );
    if rec.audio < MIN_AUDIO {
        tracing::warn!(
            audio_ms = ms(rec.audio),
            "gravação demasiado curta (na fase 3 não será enviada)"
        );
    }
    if rec.dropped_samples > 0 {
        tracing::warn!(
            dropped = rec.dropped_samples,
            "perderam-se amostras de áudio"
        );
    }
}

/// Escrever no disco bloqueia, por isso vai para o pool de threads de bloqueio em
/// vez de ocupar uma thread do runtime assíncrono.
async fn save_debug_wav(dir: PathBuf, rec: &Recording) {
    // Clonar `Bytes` só incrementa um contador de referências; os dados não são copiados.
    let wav = rec.wav.clone();
    let path = dir.join("last.wav");
    let result = tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&path, &wav)?;
        Ok::<_, std::io::Error>(path)
    })
    .await;
    match result {
        Ok(Ok(path)) => tracing::debug!(path = %path.display(), "WAV de teste guardado"),
        Ok(Err(e)) => tracing::warn!(error = %e, "não foi possível guardar o WAV de teste"),
        Err(e) => tracing::warn!(error = %e, "tarefa de gravação do WAV falhou"),
    }
}
