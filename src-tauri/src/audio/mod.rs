//! Áudio: captura do microfone → mono → 16 kHz → WAV em memória.

mod capture;
pub mod resample;
pub mod wav;

pub use capture::{AudioError, LevelSink, Recorder, Recording};
