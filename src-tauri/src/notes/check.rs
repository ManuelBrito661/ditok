//! Teste de áudio da página Notas: N segundos de microfone e de áudio do computador,
//! com os níveis ao vivo. Não grava nem transcreve nada, e só corre um de cada vez.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::unbounded_channel;

use super::capture::{Capture, CaptureConfig, CaptureEvent, Track};
use crate::settings_window;

/// Evento para a janela; o payload é o `TestEvent` com o campo `kind`.
pub const EVENT_TEST: &str = "notes:test";

/// Nível (0–1) acima do qual se considera que se ouviu alguma coisa.
pub const HEARD: f32 = 0.01;

static RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TestEvent {
    Level { track: Track, level: f32 },
    Device { track: Track, name: String },
    Lost { track: Track, why: String },
    Failed { why: String },
    Done { mic_peak: f32, system_peak: f32 },
}

/// Começa o teste numa thread própria. Erro se já houver um a correr.
pub fn start(app: AppHandle, config: CaptureConfig, seconds: u64) -> Result<(), String> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err("já há um teste de áudio a correr".to_owned());
    }
    let spawned = thread::Builder::new()
        .name("ditok-notes-test".into())
        .spawn(move || {
            run(&app, &config, Duration::from_secs(seconds));
            RUNNING.store(false, Ordering::SeqCst);
        });
    if let Err(e) = spawned {
        RUNNING.store(false, Ordering::SeqCst);
        return Err(e.to_string());
    }
    Ok(())
}

fn send(app: &AppHandle, event: TestEvent) {
    let _ = app.emit_to(settings_window::LABEL, EVENT_TEST, event);
}

fn run(app: &AppHandle, config: &CaptureConfig, length: Duration) {
    let (tx, mut rx) = unbounded_channel();
    let capture = match Capture::start(config, tx) {
        Ok(capture) => capture,
        Err(e) => {
            send(app, TestEvent::Failed { why: e.to_string() });
            return;
        }
    };

    let end = Instant::now() + length;
    // [microfone, sistema]: o maior nível de cada faixa durante o teste.
    let mut peaks = [0.0f32; 2];
    while Instant::now() < end {
        while let Ok(event) = rx.try_recv() {
            match event {
                CaptureEvent::Level(track, level) => {
                    let i = usize::from(track == Track::System);
                    peaks[i] = peaks[i].max(level);
                    send(app, TestEvent::Level { track, level });
                }
                CaptureEvent::Device(track, name) => send(app, TestEvent::Device { track, name }),
                CaptureEvent::Lost(track, why) => send(app, TestEvent::Lost { track, why }),
                // Enunciados e o aviso de silêncio (que só sai aos 15 s) não interessam aqui.
                CaptureEvent::Utterance(_) | CaptureEvent::SystemMute => {}
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    capture.stop();
    send(
        app,
        TestEvent::Done {
            mic_peak: peaks[0],
            system_peak: peaks[1],
        },
    );
}
