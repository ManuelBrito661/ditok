//! Grava o microfone e o áudio do sistema durante N segundos e guarda os enunciados
//! em WAV. Serve para validar a captura do notetaker.
//!
//!     cargo run --features cli --bin notes-probe -- 30
//!     cargo run --features cli --bin notes-probe -- 30 --so-sistema
//!     cargo run --features cli --bin notes-probe -- 30 --so-microfone
//!
//! Os ficheiros ficam em `notes-probe/` na pasta atual.

use std::{path::Path, process::ExitCode};

use ditok_lib::notes::{capture::CaptureConfig, probe};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seconds = args
        .iter()
        .find_map(|a| a.parse::<u64>().ok())
        .unwrap_or(20);
    let only_system = args.iter().any(|a| a == "--so-sistema");
    let only_mic = args.iter().any(|a| a == "--so-microfone");
    let config = CaptureConfig {
        mic: !only_system,
        system: !only_mic,
        ..CaptureConfig::default()
    };
    match probe::run(seconds, Path::new("notes-probe"), &config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("erro: {e}");
            ExitCode::FAILURE
        }
    }
}
