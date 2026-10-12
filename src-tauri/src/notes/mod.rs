//! O notetaker: grava o microfone ("Eu") e o áudio do sistema ("Outros") de uma
//! reunião, parte cada faixa em enunciados e (mais tarde) transcreve-os.
//!
//! - `segmenter`: corta um fluxo de áudio em enunciados, nas pausas (puro, testável).
//! - `capture`: abre as duas faixas (microfone e loopback) e alimenta os segmentadores.

pub mod capture;
pub mod platform;
pub mod probe;
pub mod segmenter;
pub mod session;
pub mod store;
