//! Ferramenta de desenvolvimento (`cargo run --features cli --bin notes-probe`): grava o
//! microfone e o áudio do sistema durante N segundos, imprime o que a captura vê e
//! guarda cada enunciado num WAV. Serve para validar a captura (dispositivos, loopback,
//! cortes) sem a interface; a app não a usa.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use tokio::sync::mpsc::unbounded_channel;

use super::capture::{Capture, CaptureConfig, CaptureEvent, Track};
use crate::audio::{resample::TARGET_RATE, wav};

pub fn run(seconds: u64, dir: &Path, config: &CaptureConfig) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let (tx, mut rx) = unbounded_channel();
    let capture = Capture::start(config, tx)?;
    println!("A gravar {seconds} s… fala e põe som a tocar (ctrl+c para desistir).");

    let end = Instant::now() + Duration::from_secs(seconds);
    let mut count = 0usize;
    let mut peak = [0.0f32; 2];
    let mut last_print = Instant::now();
    let mut handle = |event: CaptureEvent, peak: &mut [f32; 2]| match event {
        CaptureEvent::Utterance(u) => {
            count += 1;
            let name = format!("{}-{:07}.wav", u.track.label(), u.start_ms);
            let path = dir.join(&name);
            match std::fs::write(&path, wav::encode_pcm16_mono(&u.samples, TARGET_RATE)) {
                Ok(()) => println!(
                    "  enunciado {}  {:>6.1}s → {:>6.1}s  ({:.1}s de áudio)  {name}",
                    u.track.label(),
                    u.start_ms as f64 / 1000.0,
                    u.end_ms as f64 / 1000.0,
                    u.samples.len() as f64 / f64::from(TARGET_RATE),
                ),
                Err(e) => println!("  não consegui guardar {name}: {e}"),
            }
        }
        CaptureEvent::Level(track, level) => {
            let i = usize::from(track == Track::System);
            peak[i] = peak[i].max(level);
        }
        CaptureEvent::Device(track, name) => println!("  {}: {name}", track.label()),
        CaptureEvent::Lost(track, why) => println!("  {} PERDIDA: {why}", track.label()),
        CaptureEvent::SystemMute => println!(
            "  AVISO: o sistema entrega áudio, mas só silêncio exato (permissão em falta ou conteúdo protegido)"
        ),
    };

    while Instant::now() < end {
        while let Ok(event) = rx.try_recv() {
            handle(event, &mut peak);
        }
        if last_print.elapsed() >= Duration::from_secs(1) {
            last_print = Instant::now();
            println!("  níveis  mic {}  sistema {}", bar(peak[0]), bar(peak[1]));
            peak = [0.0; 2];
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    capture.stop();
    while let Ok(event) = rx.try_recv() {
        handle(event, &mut peak);
    }
    println!("Feito: {count} enunciados em {}", dir.display());
    Ok(())
}

fn bar(level: f32) -> String {
    let n = (level * 20.0).round() as usize;
    format!("[{:<20}]", "#".repeat(n.min(20)))
}
