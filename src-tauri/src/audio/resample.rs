//! Downmix para mono e conversão da taxa do microfone (tipicamente 48 kHz) para
//! 16 kHz, que é o que os modelos de STT usam internamente. Enviar 16 kHz mono em
//! vez de 48 kHz estéreo reduz o upload 6 vezes.

use rubato::{Fft, FixedSync, Resampler, audioadapter_buffers::direct::InterleavedSlice};

pub const TARGET_RATE: u32 = 16_000;

#[derive(Debug, thiserror::Error)]
pub enum ResampleError {
    #[error("não foi possível criar o resampler {rate} Hz → 16000 Hz: {source}")]
    Construct {
        rate: u32,
        source: rubato::ResamplerConstructionError,
    },
    #[error("falha no resampling: {0}")]
    Process(#[from] rubato::ResampleError),
    #[error("buffer inválido para o resampler: {0}")]
    Buffer(String),
}

/// Faz a média dos canais de cada frame intercalado (L R L R …) e entrega cada
/// amostra mono a `push`. É usada dentro do callback de áudio, por isso não aloca:
/// o chamador decide para onde vai cada amostra.
pub fn downmix<T: Copy>(
    data: &[T],
    channels: usize,
    to_f32: impl Fn(T) -> f32,
    mut push: impl FnMut(f32),
) {
    if channels <= 1 {
        for &s in data {
            push(to_f32(s));
        }
        return;
    }
    let scale = 1.0 / channels as f32;
    for frame in data.chunks_exact(channels) {
        let sum: f32 = frame.iter().map(|&s| to_f32(s)).sum();
        push(sum * scale);
    }
}

/// Nível de áudio entre 0 e 1 para o indicador visual: RMS em dBFS, mapeado de
/// -60 dB (silêncio, 0.0) a 0 dB (máximo, 1.0). A escala logarítmica acompanha a
/// forma como ouvimos: com uma escala linear, a fala normal mal mexia as barras.
pub fn level(samples: &[f32]) -> f32 {
    ((super::vad::dbfs(samples) + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Converte gravações completas para 16 kHz. Criar um resampler FFT é caro (planeia
/// as FFT), por isso guardamos o último e só o recriamos se a taxa de entrada mudar
/// (por exemplo, se trocares de microfone).
#[derive(Default)]
pub struct To16k {
    cached: Option<(u32, Fft<f32>)>,
}

/// Frequência de corte do filtro passa-alto (80 Hz): abaixo da nota fundamental
/// mais grave da voz humana (~85 Hz), corta o zumbido da secretária, ventoinhas,
/// ar condicionado e o desvio DC do microfone.
const HIGH_PASS_HZ: f32 = 80.0;
/// Pico alvo após normalização (-3 dBFS ≈ 0.708): coloca as sílabas átonas e as
/// consoantes finais do português europeu na zona onde os modelos locais (Whisper
/// e Parakeet) foram treinados, deixando margem contra saturação.
const TARGET_PEAK: f32 = 0.708;
/// Ganho máximo aplicado (+20 dB = 10×): se o áudio for quase só silêncio ou um
/// sussurro muito distante, não amplifica o ruído de fundo até à escala cheia.
const MAX_GAIN: f32 = 10.0;
/// Abaixo disto (~-60 dBFS) não há fala útil para normalizar.
const MIN_PEAK: f32 = 0.001;

/// Prepara um trecho de fala a 16 kHz antes de o codificar em WAV para o STT:
/// 1. Filtro passa-alto de 1.ª ordem a 80 Hz (remove DC offset e ruído grave de
///    secretária/pop, que mascaram as consoantes surdas nos espetrogramas log-mel).
/// 2. Normalização de pico para -3 dBFS (com limite de +20 dB), apenas quando o
///    áudio está demasiado baixo (abaixo de -6 dBFS ≈ 0.5) ou a clipar (> 0.99).
pub fn condition_speech(samples: &mut [f32], rate: u32) {
    if samples.is_empty() || rate == 0 {
        return;
    }
    // Filtro passa-alto RC: y[n] = α · (y[n-1] + x[n] - x[n-1])
    let rc = 1.0 / (std::f32::consts::TAU * HIGH_PASS_HZ);
    let dt = 1.0 / rate as f32;
    let alpha = rc / (rc + dt);
    let mut prev_x = samples[0];
    let mut prev_y = 0.0f32;
    let mut peak = 0.0f32;
    for s in samples.iter_mut() {
        let x = *s;
        let y = alpha * (prev_y + x - prev_x);
        prev_x = x;
        prev_y = y;
        *s = y;
        peak = peak.max(y.abs());
    }

    if peak >= MIN_PEAK && !(0.5..=0.99).contains(&peak) {
        let gain = (TARGET_PEAK / peak).min(MAX_GAIN);
        for s in samples.iter_mut() {
            *s = (*s * gain).clamp(-1.0, 1.0);
        }
    }
}

impl To16k {
    pub fn convert(&mut self, input: &[f32], input_rate: u32) -> Result<Vec<f32>, ResampleError> {
        if input_rate == TARGET_RATE || input.is_empty() {
            return Ok(input.to_vec());
        }
        let resampler = match &mut self.cached {
            Some((rate, r)) if *rate == input_rate => r,
            slot => {
                let r = Fft::<f32>::new(
                    input_rate as usize,
                    TARGET_RATE as usize,
                    1024,
                    1,
                    FixedSync::Both,
                )
                .map_err(|source| ResampleError::Construct {
                    rate: input_rate,
                    source,
                })?;
                &mut slot.insert((input_rate, r)).1
            }
        };
        resampler.reset();

        let out_len = resampler.process_all_needed_output_len(input.len());
        let mut output = vec![0.0f32; out_len];
        let buf_in = InterleavedSlice::new(input, 1, input.len())
            .map_err(|e| ResampleError::Buffer(e.to_string()))?;
        let mut buf_out = InterleavedSlice::new_mut(&mut output, 1, out_len)
            .map_err(|e| ResampleError::Buffer(e.to_string()))?;
        // `process_all_into_buffer` corta o atraso inicial do filtro e processa a cauda,
        // por isso o resultado fica alinhado com a entrada.
        let (_, written) =
            resampler.process_all_into_buffer(&buf_in, &mut buf_out, input.len(), None)?;
        output.truncate(written);
        Ok(output)
    }
}

/// Passa-alto de 1.ª ordem com estado, para o áudio do stream (a versão de
/// `condition_speech` precisa da gravação inteira para normalizar o pico).
struct HighPass {
    alpha: f32,
    prev_x: f32,
    prev_y: f32,
    started: bool,
}

impl HighPass {
    fn new(rate: u32) -> Self {
        let rc = 1.0 / (std::f32::consts::TAU * HIGH_PASS_HZ);
        let dt = 1.0 / rate as f32;
        Self {
            alpha: rc / (rc + dt),
            prev_x: 0.0,
            prev_y: 0.0,
            started: false,
        }
    }

    fn apply(&mut self, x: f32) -> f32 {
        if !self.started {
            // Como em `condition_speech`: o primeiro valor não é um degrau.
            self.prev_x = x;
            self.started = true;
        }
        let y = self.alpha * (self.prev_y + x - self.prev_x);
        self.prev_x = x;
        self.prev_y = y;
        y
    }
}

/// O áudio do microfone, aos poucos (blocos de ~50 ms), para 16 kHz mono PCM16 — o
/// que a transcrição em streaming recebe enquanto se fala. Ao contrário de `To16k`,
/// guarda o estado entre blocos: o resto que não enche um bloco do resampler fica à
/// espera do próximo, e o passa-alto continua de onde ficou.
pub struct StreamResampler {
    fft: Option<Fft<f32>>,
    pending: Vec<f32>,
    high_pass: HighPass,
}

impl StreamResampler {
    pub fn new(input_rate: u32) -> Result<Self, ResampleError> {
        let fft = if input_rate == TARGET_RATE {
            None
        } else {
            Some(
                Fft::<f32>::new(
                    input_rate as usize,
                    TARGET_RATE as usize,
                    1024,
                    1,
                    FixedSync::Both,
                )
                .map_err(|source| ResampleError::Construct {
                    rate: input_rate,
                    source,
                })?,
            )
        };
        Ok(Self {
            fft,
            pending: Vec::new(),
            high_pass: HighPass::new(TARGET_RATE),
        })
    }

    /// Mono à taxa do microfone → PCM16 a 16 kHz. Pode devolver menos do que o
    /// equivalente à entrada (o resto espera pelo próximo bloco).
    pub fn push(&mut self, input: &[f32]) -> Result<Vec<i16>, ResampleError> {
        self.pending.extend_from_slice(input);
        self.process(false)
    }

    /// No fim do áudio: processa o que sobrou (completado com silêncio).
    pub fn finish(&mut self) -> Result<Vec<i16>, ResampleError> {
        self.process(true)
    }

    fn process(&mut self, flush: bool) -> Result<Vec<i16>, ResampleError> {
        let mut mono16k: Vec<f32> = Vec::new();
        match self.fft.as_mut() {
            None => mono16k.append(&mut self.pending),
            Some(fft) => loop {
                let need = fft.input_frames_next();
                if self.pending.len() < need {
                    if !flush || self.pending.is_empty() {
                        break;
                    }
                    self.pending.resize(need, 0.0);
                }
                let mut block = vec![0.0f32; fft.output_frames_next()];
                let out_len = block.len();
                let buf_in = InterleavedSlice::new(&self.pending[..need], 1, need)
                    .map_err(|e| ResampleError::Buffer(e.to_string()))?;
                let mut buf_out = InterleavedSlice::new_mut(&mut block, 1, out_len)
                    .map_err(|e| ResampleError::Buffer(e.to_string()))?;
                let (read, written) = fft.process_into_buffer(&buf_in, &mut buf_out, None)?;
                mono16k.extend_from_slice(&block[..written]);
                self.pending.drain(..read);
            },
        }
        Ok(mono16k
            .into_iter()
            .map(|s| (self.high_pass.apply(s).clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn stream_in_blocks(rate: u32, input: &[f32], block: usize) -> Vec<i16> {
        let mut r = StreamResampler::new(rate).unwrap();
        let mut out = Vec::new();
        for chunk in input.chunks(block) {
            out.extend(r.push(chunk).unwrap());
        }
        out.extend(r.finish().unwrap());
        out
    }

    fn crossings_i16(s: &[i16]) -> usize {
        s.windows(2).filter(|w| w[0] <= 0 && w[1] > 0).count()
    }

    #[test]
    fn stream_resampler_keeps_duration_and_frequency_in_50ms_blocks() {
        // 48 kHz: blocos de 50 ms = 2400 amostras, como o tap do microfone.
        let out = stream_in_blocks(48_000, &sine(1000.0, 48_000, 1.0), 2400);
        assert!(
            (out.len() as i64 - 16_000).abs() <= 1100,
            "len = {} (o último bloco completa-se com silêncio)",
            out.len()
        );
        let zc = crossings_i16(&out);
        assert!((985..=1015).contains(&zc), "zero crossings = {zc}");
        // 44,1 kHz tem um rácio fracionário.
        let out = stream_in_blocks(44_100, &sine(440.0, 44_100, 2.0), 2205);
        assert!(
            (out.len() as i64 - 32_000).abs() <= 1100,
            "len = {}",
            out.len()
        );
        // Só o miolo (440 Hz × 1,75 s = 770): as pontas têm o arranque do filtro e o silêncio de fecho.
        let zc = crossings_i16(&out[2_000..30_000]);
        assert!((760..=780).contains(&zc), "zero crossings = {zc}");
    }

    #[test]
    fn stream_resampler_does_not_depend_on_the_block_size() {
        let input = sine(700.0, 48_000, 0.5);
        let small = stream_in_blocks(48_000, &input, 480);
        let big = stream_in_blocks(48_000, &input, 4800);
        assert_eq!(small.len(), big.len());
        let worst = small
            .iter()
            .zip(&big)
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
            .max()
            .unwrap();
        assert!(worst <= 1, "diferença máxima = {worst}");
    }

    #[test]
    fn stream_resampler_at_16k_passes_through_and_filters_dc() {
        let out = stream_in_blocks(16_000, &vec![0.5f32; 16_000], 800);
        assert_eq!(out.len(), 16_000);
        // O desvio DC (0,5 constante) é cortado pelo passa-alto.
        assert!(
            out[15_000..].iter().all(|s| s.abs() < 800),
            "{:?}",
            &out[15_000..15_005]
        );
    }

    fn sine(freq: f32, rate: u32, secs: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| (TAU * freq * i as f32 / rate as f32).sin() * 0.5)
            .collect()
    }

    /// Conta as passagens por zero de baixo para cima: para um seno puro, é a frequência × duração.
    fn rising_zero_crossings(s: &[f32]) -> usize {
        s.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    #[test]
    fn downmix_averages_interleaved_channels() {
        let mut out = Vec::new();
        downmix(
            &[1.0f32, 0.0, 0.5, 0.5, -1.0, 1.0],
            2,
            |s| s,
            |s| out.push(s),
        );
        assert_eq!(out, vec![0.5, 0.5, 0.0]);
    }

    #[test]
    fn downmix_converts_integer_samples() {
        let mut out = Vec::new();
        downmix(
            &[i16::MAX, i16::MAX, 0, 0],
            2,
            |s| s as f32 / 32768.0,
            |s| out.push(s),
        );
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.99997).abs() < 1e-4);
        assert_eq!(out[1], 0.0);
    }

    #[test]
    fn level_maps_dbfs_to_unit_range() {
        assert_eq!(level(&[]), 0.0);
        assert_eq!(level(&[0.0; 100]), 0.0, "silêncio");
        assert!(
            (level(&[1.0, -1.0, 1.0, -1.0]) - 1.0).abs() < 1e-6,
            "escala cheia = 0 dBFS"
        );
        // 0.1 de amplitude = -20 dBFS → 40/60.
        assert!((level(&[0.1, -0.1, 0.1, -0.1]) - 40.0 / 60.0).abs() < 1e-4);
        // -70 dBFS fica abaixo do mínimo.
        assert_eq!(level(&[0.0003; 10]), 0.0);
    }

    #[test]
    fn downmix_mono_passes_through() {
        let mut out = Vec::new();
        downmix(&[0.1f32, 0.2, 0.3], 1, |s| s, |s| out.push(s));
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn resample_48k_keeps_duration_and_frequency() {
        let mut r = To16k::default();
        let out = r.convert(&sine(1000.0, 48_000, 1.0), 48_000).unwrap();
        assert!(
            (out.len() as i64 - 16_000).abs() <= 2,
            "len = {}",
            out.len()
        );
        let zc = rising_zero_crossings(&out);
        assert!((995..=1005).contains(&zc), "zero crossings = {zc}");
    }

    #[test]
    fn resample_44_1k_keeps_duration_and_frequency() {
        let mut r = To16k::default();
        let out = r.convert(&sine(440.0, 44_100, 2.0), 44_100).unwrap();
        assert!(
            (out.len() as i64 - 32_000).abs() <= 2,
            "len = {}",
            out.len()
        );
        let zc = rising_zero_crossings(&out);
        assert!((875..=885).contains(&zc), "zero crossings = {zc}");
    }

    #[test]
    fn resampler_is_reused_and_reset_between_recordings() {
        let mut r = To16k::default();
        let a = r.convert(&sine(1000.0, 48_000, 0.5), 48_000).unwrap();
        let b = r.convert(&sine(1000.0, 48_000, 0.5), 48_000).unwrap();
        assert_eq!(
            a, b,
            "o estado de uma gravação não pode contaminar a seguinte"
        );
    }

    #[test]
    fn frequencies_above_8k_are_filtered_out() {
        // 12 kHz não existe a 16 kHz (Nyquist = 8 kHz): tem de ser atenuado, não "dobrado".
        let mut r = To16k::default();
        let out = r.convert(&sine(12_000.0, 48_000, 0.5), 48_000).unwrap();
        let rms = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        assert!(rms < 0.01, "rms = {rms}");
    }

    #[test]
    fn same_rate_and_empty_input_pass_through() {
        let mut r = To16k::default();
        assert_eq!(
            r.convert(&[0.25, -0.25], 16_000).unwrap(),
            vec![0.25, -0.25]
        );
        assert!(r.convert(&[], 48_000).unwrap().is_empty());
    }

    #[test]
    fn condition_speech_removes_dc_offset_and_normalizes_quiet_speech() {
        // Voz baixa (-26 dBFS, amplitude 0.05) a 300 Hz com um forte desvio DC (+0.2).
        let mut s: Vec<f32> = (0..TARGET_RATE as usize)
            .map(|i| 0.2 + (TAU * 300.0 * i as f32 / TARGET_RATE as f32).sin() * 0.05)
            .collect();
        condition_speech(&mut s, TARGET_RATE);
        // Na segunda metade o filtro passa-alto já estabilizou: média ~0 e pico ~0.5 (ganho 10×).
        let tail = &s[TARGET_RATE as usize / 2..];
        let mean = tail.iter().sum::<f32>() / tail.len() as f32;
        let peak = tail.iter().fold(0.0f32, |p, &x| p.max(x.abs()));
        assert!(mean.abs() < 0.01, "média DC residual = {mean}");
        assert!((0.40..=0.75).contains(&peak), "pico normalizado = {peak}");
    }

    #[test]
    fn condition_speech_does_not_amplify_digital_silence() {
        let mut s = vec![0.0001f32; 1600];
        condition_speech(&mut s, TARGET_RATE);
        let peak = s.iter().fold(0.0f32, |p, &x| p.max(x.abs()));
        assert!(peak < 0.001, "silêncio amplificado: {peak}");
    }

    /// Mede o custo pós-gravação de 5 s de fala. Corre com:
    /// `cargo test --release finalize_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn finalize_cost_for_5s_at_48k() {
        let input = sine(220.0, 48_000, 5.0);
        let mut r = To16k::default();
        r.convert(&input, 48_000).unwrap(); // aquece: cria o resampler (só acontece uma vez)
        let t = std::time::Instant::now();
        let out = r.convert(&input, 48_000).unwrap();
        let resample = t.elapsed();
        let wav = crate::audio::wav::encode_pcm16_mono(&out, TARGET_RATE);
        println!(
            "resample: {resample:?}, resample+wav: {:?}, wav: {} KB",
            t.elapsed(),
            wav.len() / 1024
        );
    }
}
