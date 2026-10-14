//! O notetaker: grava o microfone ("Eu") e o áudio do sistema ("Outros") de uma
//! reunião, parte cada faixa em enunciados e transcreve-os com a xAI.
//!
//! - `segmenter`: corta um fluxo de áudio em enunciados, nas pausas (puro, testável).
//! - `capture`: abre as duas faixas (microfone e loopback) e alimenta os segmentadores.
//! - `session`: a sessão em curso (fila de transcrição, linhas, custo).
//! - `speakers`: no fim, a separação de oradores da xAI sobre a faixa dos outros.

pub mod capture;
pub mod check;
pub mod platform;
pub mod probe;
pub mod segmenter;
pub mod session;
pub mod speakers;
pub mod store;
