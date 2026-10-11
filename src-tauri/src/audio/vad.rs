//! Deteção de voz (VAD, "voice activity detection") por energia: decide se uma
//! gravação tem fala e onde é que ela começa e acaba.
//!
//! Porquê: quando recebe só silêncio, o Whisper inventa frases ("Obrigado por
//! assistir.", "Legendas pela comunidade Amara.org") e o Ditok colava-as. E os
//! modelos são cobrados ao segundo, por isso o silêncio nas pontas é tempo de
//! upload e dinheiro deitados fora.
//!
//! Como: o áudio é partido em blocos de 20 ms e mede-se a energia de cada um em
//! dBFS. O ruído de fundo é estimado com os blocos mais silenciosos da própria
//! gravação (assim funciona com microfones e salas diferentes), e conta como voz o
//! que fica claramente acima dele durante pelo menos 80 ms seguidos. Um clique de
//! tecla é forte mas curto demais para passar.

use std::ops::Range;

const FRAME_MS: usize = 20;
/// Percentil dos blocos usado como ruído de fundo: mesmo quando falas quase o
/// tempo todo, há pausas entre as palavras.
const NOISE_PERCENTILE: usize = 10;
/// Quanto acima do ruído de fundo tem de estar um bloco para contar como voz.
const MARGIN_DB: f32 = 12.0;
/// Limites do limiar. O de baixo impede que, num microfone com supressão de ruído
/// (silêncio a ~-85 dBFS), uma respiração ou um clique conte como voz; o de cima
/// garante que som forte e sustentado conta sempre, mesmo numa sala barulhenta.
const MIN_THRESHOLD_DB: f32 = -55.0;
const MAX_THRESHOLD_DB: f32 = -35.0;
/// Buracos até este tamanho não partem um segmento (consoantes surdas, pausas
/// curtas entre sílabas).
const BRIDGE_FRAMES: usize = 2;
/// Duração mínima de um segmento para contar como fala: 4 × 20 ms = 80 ms.
const MIN_SEGMENT_FRAMES: usize = 4;
/// Margem mantida à volta da fala ao cortar: inícios e finais suaves ("s", "f")
/// ficam abaixo do limiar, e os modelos transcrevem melhor com um pouco de
/// silêncio à volta.
const PAD_BEFORE_MS: usize = 300;
const PAD_AFTER_MS: usize = 400;

/// Resultado da análise. Os níveis em dBFS servem para os logs: são eles que
/// permitem afinar os limiares se um microfone se portar de forma diferente.
#[derive(Debug, Clone, PartialEq)]
pub struct Voice {
    /// Ruído de fundo estimado.
    pub noise_db: f32,
    /// Limiar usado para separar voz de ruído.
    pub threshold_db: f32,
    /// Nível do bloco mais forte.
    pub peak_db: f32,
    /// Soma da duração dos segmentos com fala.
    pub voiced_ms: usize,
    /// Amostras a enviar: da primeira à última fala, com margem. `None` se não
    /// houver fala nenhuma.
    pub speech: Option<Range<usize>>,
}

impl Voice {
    pub fn has_speech(&self) -> bool {
        self.speech.is_some()
    }
}

/// Energia (RMS) em dBFS: 0 dB é a escala cheia, o silêncio digital fica a -100 dB.
pub fn dbfs(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return -100.0;
    }
    let mean_square = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
    10.0 * mean_square.max(1e-10).log10() // 10·log10(x²) = 20·log10(x)
}

pub fn detect(samples: &[f32], rate: u32) -> Voice {
    let rate = rate as usize;
    let frame = (rate * FRAME_MS / 1000).max(1);
    // `chunks` (e não `chunks_exact`) inclui o último bloco, mesmo incompleto.
    let db: Vec<f32> = samples.chunks(frame).map(dbfs).collect();

    let mut sorted = db.clone();
    // `total_cmp` porque `f32` não é `Ord` (por causa do NaN); aqui não há NaN,
    // mas assim o compilador não nos obriga a um `unwrap`.
    sorted.sort_by(f32::total_cmp);
    let noise_db = sorted
        .get(sorted.len() * NOISE_PERCENTILE / 100)
        .copied()
        .unwrap_or(-100.0);
    let peak_db = sorted.last().copied().unwrap_or(-100.0);
    let threshold_db = (noise_db + MARGIN_DB).clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB);

    // Segmentos de blocos com voz, como intervalos [início, fim) em blocos.
    let mut segments: Vec<Range<usize>> = Vec::new();
    for (i, &d) in db.iter().enumerate() {
        if d < threshold_db {
            continue;
        }
        match segments.last_mut() {
            // Desde o fim do último segmento só houve um buraco pequeno: continua.
            Some(last) if i - last.end <= BRIDGE_FRAMES => last.end = i + 1,
            _ => segments.push(i..i + 1),
        }
    }
    segments.retain(|s| s.len() >= MIN_SEGMENT_FRAMES);

    let voiced_ms = segments.iter().map(|s| s.len() * FRAME_MS).sum();
    let speech = match (segments.first(), segments.last()) {
        (Some(first), Some(last)) => {
            let start = (first.start * frame).saturating_sub(rate * PAD_BEFORE_MS / 1000);
            let end = (last.end * frame + rate * PAD_AFTER_MS / 1000).min(samples.len());
            Some(start..end)
        }
        _ => None,
    };

    Voice {
        noise_db,
        threshold_db,
        peak_db,
        voiced_ms,
        speech,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const RATE: u32 = 16_000;
    const MS: usize = 16; // amostras por milissegundo a 16 kHz

    /// Ruído branco determinístico (xorshift) com o nível RMS pedido, em dBFS.
    fn noise(ms: usize, db: f32, seed: u32) -> Vec<f32> {
        // Ruído uniforme em [-a, a] tem RMS a/√3.
        let a = 10f32.powf(db / 20.0) * 3f32.sqrt();
        let mut x = seed.max(1);
        (0..ms * MS)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32 * 2.0 - 1.0) * a
            })
            .collect()
    }

    /// "Fala": um seno de 200 Hz com amplitude 0.1 (≈ -23 dBFS) por cima do ruído.
    fn tone(ms: usize, seed: u32) -> Vec<f32> {
        noise(ms, -70.0, seed)
            .into_iter()
            .enumerate()
            .map(|(i, n)| n + (TAU * 200.0 * i as f32 / RATE as f32).sin() * 0.1)
            .collect()
    }

    fn concat(parts: &[Vec<f32>]) -> Vec<f32> {
        parts.concat()
    }

    #[test]
    fn digital_silence_and_steady_noise_have_no_speech() {
        assert!(!detect(&[0.0; 32_000], RATE).has_speech());
        assert!(!detect(&noise(2000, -60.0, 1), RATE).has_speech());
        // Microfone barulhento: o limiar sobe com o ruído.
        assert!(!detect(&noise(2000, -40.0, 2), RATE).has_speech());
        assert!(!detect(&[], RATE).has_speech());
    }

    #[test]
    fn speech_is_found_and_padded() {
        // 1 s de silêncio, 0,5 s de fala, 1 s de silêncio. Os limites caem
        // exatamente em blocos de 20 ms, por isso as contas são exatas.
        let audio = concat(&[noise(1000, -70.0, 3), tone(500, 4), noise(1000, -70.0, 5)]);
        let v = detect(&audio, RATE);
        assert_eq!(v.speech, Some((1000 - 300) * MS..(1500 + 400) * MS));
        assert_eq!(v.voiced_ms, 500);
        assert!(v.noise_db < -65.0 && v.peak_db > -30.0, "{v:?}");
    }

    #[test]
    fn a_key_click_is_not_speech() {
        let click: Vec<f32> = noise(20, -6.0, 6);
        let audio = concat(&[noise(800, -70.0, 7), click, noise(800, -70.0, 8)]);
        assert!(!detect(&audio, RATE).has_speech());
    }

    #[test]
    fn short_pauses_between_syllables_do_not_split_speech() {
        // Cada sílaba (60 ms) sozinha é curta demais; com o buraco de 40 ms
        // ligado, o segmento tem 160 ms.
        let audio = concat(&[
            noise(500, -70.0, 9),
            tone(60, 10),
            noise(40, -70.0, 11),
            tone(60, 12),
            noise(500, -70.0, 13),
        ]);
        let v = detect(&audio, RATE);
        assert!(v.has_speech());
        assert_eq!(v.voiced_ms, 160);
    }

    #[test]
    fn speech_that_fills_the_recording_is_kept_whole() {
        let audio = tone(1000, 14);
        assert_eq!(detect(&audio, RATE).speech, Some(0..audio.len()));
    }

    /// Analisa um WAV gravado pelo Ditok (`debug/last.wav`), para afinar os limiares
    /// com gravações reais. Corre com:
    /// `DITOK_WAV=caminho/last.wav cargo test vad_on_file -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn vad_on_file() {
        let path = std::env::var("DITOK_WAV").expect("define DITOK_WAV");
        let bytes = std::fs::read(path).unwrap();
        // O nosso WAV: cabeçalho de 44 bytes + PCM16 mono a 16 kHz.
        let samples: Vec<f32> = bytes[44..]
            .chunks_exact(2)
            .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0)
            .collect();
        let v = detect(&samples, RATE);
        println!("{} ms gravados → {v:?}", samples.len() / MS);
    }

    #[test]
    fn dbfs_of_known_levels() {
        assert_eq!(dbfs(&[]), -100.0);
        assert_eq!(dbfs(&[0.0; 10]), -100.0);
        assert!(dbfs(&[1.0, -1.0]).abs() < 1e-6);
        assert!((dbfs(&[0.1, -0.1]) + 20.0).abs() < 1e-4);
    }
}
