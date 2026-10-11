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

/// Converte gravações completas para 16 kHz. Criar um resampler FFT é caro (planeia
/// as FFT), por isso guardamos o último e só o recriamos se a taxa de entrada mudar
/// (por exemplo, se trocares de microfone).
#[derive(Default)]
pub struct To16k {
    cached: Option<(u32, Fft<f32>)>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

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
