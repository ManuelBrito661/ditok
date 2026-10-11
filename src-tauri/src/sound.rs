//! Sons curtos ao começar e ao acabar de gravar, para saberes o que se passa sem
//! olhar para a barra. São sintetizados aqui (senos com um envelope), sem ficheiros.
//!
//! Tocam numa thread própria (`ditok-sound`): abrir a saída de áudio demora umas
//! dezenas de ms e não pode atrasar o microfone. A saída só fica aberta enquanto o
//! som toca, para não manter a placa de som acordada.
//!
//! Cuidado com o microfone: o som de início toca enquanto ele abre e, com colunas
//! em vez de auscultadores, fica na gravação. Por isso é curto (~150 ms) e a
//! deteção de voz é avisada de que tocou (`vad::detect(.., start_cue)`): ignora um
//! segmento curto no começo, para uma gravação acidental só com o "plim" não ir
//! para o modelo, que inventaria uma frase.

use std::{f32::consts::TAU, sync::mpsc, thread, time::Duration};

use cpal::{
    FromSample, SampleFormat, SizedSample, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// Começou a gravar: duas notas a subir.
    Start,
    /// Parou de gravar e vai transcrever: as mesmas notas a descer.
    Stop,
    /// Gravação cancelada: uma nota grave.
    Cancel,
}

/// Volume das notas (escala cheia = 1). Baixo de propósito: é um aviso, não um alarme.
const VOLUME: f32 = 0.16;
/// Dó e sol da 6.ª oitava: agudos o suficiente para se ouvirem bem por cima de
/// ruído, sem serem estridentes.
const LOW: f32 = 1046.5;
const HIGH: f32 = 1568.0;
const NOTE: Duration = Duration::from_millis(45);
const GAP: Duration = Duration::from_millis(55);
/// Tempo extra com a saída aberta depois do som, para a latência da placa de som.
const TAIL: Duration = Duration::from_millis(150);

/// A "pega" usada pelo orquestrador: só tem um `Sender`, por isso é barata e `Send`.
pub struct Sounds {
    tx: mpsc::Sender<Cue>,
}

impl Sounds {
    pub fn spawn() -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("ditok-sound".into())
            .spawn(move || sound_thread(rx))?;
        Ok(Self { tx })
    }

    /// Não espera: o som toca na thread de som.
    pub fn play(&self, cue: Cue) {
        let _ = self.tx.send(cue);
    }
}

fn sound_thread(rx: mpsc::Receiver<Cue>) {
    // Parada no `recv()` (zero CPU) até haver um som para tocar.
    while let Ok(mut cue) = rx.recv() {
        // Se se juntaram vários (ex.: começar e cancelar logo a seguir), só o último
        // interessa: tocá-los todos em fila ficaria atrasado em relação ao que se vê.
        while let Ok(next) = rx.try_recv() {
            cue = next;
        }
        if let Err(e) = play(cue) {
            tracing::debug!(error = %e, ?cue, "não foi possível tocar o som");
        }
    }
}

fn play(cue: Cue) -> Result<(), cpal::Error> {
    let Some(device) = cpal::default_host().default_output_device() else {
        tracing::debug!("sem saída de áudio; som ignorado");
        return Ok(());
    };
    let supported = device.default_output_config()?;
    let format = supported.sample_format();
    let config = supported.config();
    let samples = render(cue, config.sample_rate);
    let length = Duration::from_secs_f64(samples.len() as f64 / f64::from(config.sample_rate));

    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, config, samples),
        SampleFormat::I16 => build::<i16>(&device, config, samples),
        SampleFormat::I32 => build::<i32>(&device, config, samples),
        SampleFormat::U16 => build::<u16>(&device, config, samples),
        SampleFormat::F64 => build::<f64>(&device, config, samples),
        other => {
            tracing::debug!(?other, "formato de saída não suportado; som ignorado");
            return Ok(());
        }
    }?;
    stream.play()?;
    thread::sleep(length + TAIL);
    // Aqui o `stream` sai de âmbito e a saída de áudio é fechada.
    Ok(())
}

/// O callback é dono das amostras (`move`) e da posição: não há nada partilhado
/// com outras threads, por isso não precisa de `Arc` nem de atómicos.
fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    samples: Vec<f32>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let mut position = 0;
    device.build_output_stream::<T, _, _>(
        config,
        move |out: &mut [T], _info| {
            // Mono → todos os canais. Depois do fim, silêncio.
            for frame in out.chunks_mut(channels) {
                let value = samples.get(position).copied().unwrap_or(0.0);
                position += 1;
                frame.fill(T::from_sample(value));
            }
        },
        |err| tracing::debug!(error = %err, "erro na saída de áudio"),
        None,
    )
}

/// As amostras (mono, `rate` Hz) de um som.
pub fn render(cue: Cue, rate: u32) -> Vec<f32> {
    let mut out = Vec::new();
    match cue {
        Cue::Start => {
            note(&mut out, LOW, NOTE, rate);
            silence(&mut out, GAP, rate);
            note(&mut out, HIGH, NOTE, rate);
        }
        Cue::Stop => {
            note(&mut out, HIGH, NOTE, rate);
            silence(&mut out, GAP, rate);
            note(&mut out, LOW, NOTE, rate);
        }
        Cue::Cancel => note(&mut out, LOW / 2.0, NOTE * 2, rate),
    }
    out
}

fn silence(out: &mut Vec<f32>, length: Duration, rate: u32) {
    out.resize(out.len() + samples_in(length, rate), 0.0);
}

/// Uma nota com ataque rápido e decaimento exponencial, como uma marimba. Um
/// pouco de segunda harmónica torna-a menos "bip de micro-ondas".
fn note(out: &mut Vec<f32>, freq: f32, length: Duration, rate: u32) {
    let n = samples_in(length, rate);
    let rate = rate as f32;
    let attack = 0.003 * rate;
    let decay = n as f32 / 4.0;
    out.extend((0..n).map(|i| {
        let t = i as f32 / rate;
        let i = i as f32;
        let envelope = (i / attack).min(1.0)
            * (-i / decay).exp()
            // Últimas amostras a zero, para não acabar num estalido.
            * ((n as f32 - 1.0 - i) / attack).min(1.0);
        let wave = (TAU * freq * t).sin() + 0.25 * (TAU * 2.0 * freq * t).sin();
        VOLUME * envelope * wave / 1.25
    }));
}

fn samples_in(length: Duration, rate: u32) -> usize {
    (length.as_secs_f64() * f64::from(rate)).round() as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::vad;

    const RATE: u32 = 16_000;

    /// Toca os três sons na saída de áudio por omissão, para os ouvir.
    /// `cargo test live_sounds -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_sounds() {
        for cue in [Cue::Start, Cue::Stop, Cue::Cancel] {
            let started = std::time::Instant::now();
            play(cue).unwrap();
            println!("{cue:?}: {:?} com a saída aberta", started.elapsed());
            thread::sleep(Duration::from_millis(400));
        }
    }

    #[test]
    fn sounds_are_short_and_never_clip() {
        for cue in [Cue::Start, Cue::Stop, Cue::Cancel] {
            let s = render(cue, 48_000);
            assert!(s.len() < 48_000 / 4, "{cue:?} tem {} amostras", s.len());
            assert!(s.iter().all(|v| v.abs() <= VOLUME), "{cue:?}");
            assert_eq!(s.last().copied(), Some(0.0), "{cue:?} acaba em silêncio");
        }
    }

    /// O som de início pode ficar gravado: nunca pode contar como fala, senão
    /// uma gravação "vazia" ia para o modelo (que inventaria uma frase). Testa
    /// com vários ganhos, alinhamentos com os blocos de 20 ms e ecos da sala.
    /// Ganho 1 = o som chega ao microfone tão forte como sai do Ditok (~-21
    /// dBFS), já mais alto do que a fala normal num microfone a 20 cm.
    #[test]
    fn the_start_sound_is_not_speech() {
        let cue = render(Cue::Start, RATE);
        for gain in [0.1, 0.3, 1.0, 3.0] {
            for offset_ms in [0, 7, 13, 90, 200] {
                for echo in [false, true] {
                    let mut audio = vec![0.0f32; RATE as usize * 2];
                    let at = offset_ms * RATE as usize / 1000;
                    for (i, s) in cue.iter().enumerate() {
                        audio[at + i] += s * gain;
                    }
                    if echo {
                        // Duas reflexões nas paredes: 30 ms (-6 dB) e 60 ms (-12 dB).
                        let dry = audio.clone();
                        let d = 30 * RATE as usize / 1000;
                        for i in d..audio.len() {
                            audio[i] += dry[i - d] * 0.5;
                        }
                        for i in 2 * d..audio.len() {
                            audio[i] += dry[i - 2 * d] * 0.25;
                        }
                    }
                    let v = vad::detect(&audio, RATE, true);
                    assert!(
                        !v.has_speech(),
                        "ganho {gain}, início {offset_ms} ms, eco {echo}: {v:?}"
                    );
                }
            }
        }
    }
}
