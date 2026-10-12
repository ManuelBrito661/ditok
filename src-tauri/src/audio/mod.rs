//! Áudio: captura do microfone → mono → 16 kHz → corte do silêncio → WAV em memória.

mod capture;
pub mod decode;
pub mod resample;
pub mod vad;
pub mod wav;

pub use capture::{
    AudioError, InputDevice, LevelSink, MicSink, Recorder, Recording, input_devices,
};
pub(crate) use capture::{Session as RawInput, device_name};
