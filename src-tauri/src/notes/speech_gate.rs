//! Filtro de fala (BETA) das Notas: antes de um enunciado ir ao STT, pergunta ao Silero
//! VAD (via sherpa-onnx) se lá há fala de facto.
//!
//! Porquê: a deteção por energia (`segmenter.rs`) só mede o volume, por isso teclado,
//! tosse e música com volume chegam ao STT como enunciados. Com o Silero, um enunciado
//! sem fala é descartado antes de ser transcrito (e pago).
//!
//! Limites: música com voz (canto) conta como fala, e o filtro não olha para o resto
//! do enunciado. Se o Silero falhar a carregar, o enunciado segue como antes.
//!
//! Só existe com o motor local, que é quem traz o sherpa-onnx. Sem a feature `local`,
//! `AVAILABLE` é falso e o filtro não faz nada.

use std::path::Path;

/// Se o filtro existe neste build.
pub const AVAILABLE: bool = cfg!(feature = "local");

#[cfg(feature = "local")]
const MODEL_NAME: &str = "silero_vad.onnx";
/// O modelo do Silero VAD (MIT), embebido no binário: são 640 KB e não há nada para
/// descarregar.
#[cfg(feature = "local")]
const MODEL: &[u8] = include_bytes!("../../assets/silero_vad.onnx");

/// `true` se `samples` (16 kHz mono) tem pelo menos um trecho de fala. Bloqueante:
/// `spawn_blocking`. Em caso de erro devolve `true`, para não perder fala por causa do
/// filtro.
#[cfg(feature = "local")]
pub fn has_speech(models_dir: &Path, samples: &[f32]) -> bool {
    use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};

    let Ok(model) = ensure_model(models_dir) else {
        tracing::warn!("filtro de fala: não foi possível escrever o modelo; enunciado mantido");
        return true;
    };
    let config = VadModelConfig {
        silero_vad: SileroVadModelConfig {
            model: Some(model.display().to_string()),
            // Os valores por omissão do sherpa: 0,5 para a probabilidade de fala, e
            // 0,25 s para um trecho contar (um clique de tecla ou uma tosse curta fica
            // abaixo disso).
            threshold: 0.5,
            min_silence_duration: 0.25,
            min_speech_duration: 0.25,
            // O Silero trabalha em janelas de 512 amostras a 16 kHz.
            window_size: 512,
            max_speech_duration: 60.0,
        },
        sample_rate: 16_000,
        num_threads: 1,
        ..Default::default()
    };
    let Some(vad) = VoiceActivityDetector::create(&config, 60.0) else {
        tracing::warn!("filtro de fala: o Silero não arrancou; enunciado mantido");
        return true;
    };
    vad.accept_waveform(samples);
    vad.flush();
    !vad.is_empty()
}

#[cfg(not(feature = "local"))]
pub fn has_speech(_models_dir: &Path, _samples: &[f32]) -> bool {
    true
}

/// Escreve o modelo em `models_dir/vad/` se não estiver lá com o tamanho certo. Passa
/// por `.part` para que um ficheiro a meio nunca fique com o nome final.
#[cfg(feature = "local")]
fn ensure_model(models_dir: &Path) -> std::io::Result<std::path::PathBuf> {
    let dir = models_dir.join("vad");
    let path = dir.join(MODEL_NAME);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() == MODEL.len() as u64) {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir)?;
    let part = dir.join(format!("{MODEL_NAME}.part"));
    std::fs::write(&part, MODEL)?;
    std::fs::rename(&part, &path)?;
    Ok(path)
}

#[cfg(all(test, feature = "local"))]
mod tests {
    use super::*;

    /// Ruído determinístico (xorshift) a -40 dBFS: não é fala.
    fn noise(seconds: f32, seed: u32) -> Vec<f32> {
        let a = 10f32.powf(-40.0 / 20.0) * 3f32.sqrt();
        let mut x = seed.max(1);
        (0..(seconds * 16_000.0) as usize)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                ((x as f32 / u32::MAX as f32) * 2.0 - 1.0) * a
            })
            .collect()
    }

    #[test]
    fn writes_the_model_once_and_checks_its_size() {
        let dir = std::env::temp_dir().join(format!("ditok-vad-{}", std::process::id()));
        let path = ensure_model(&dir).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), MODEL.len() as u64);
        // Segunda chamada: o ficheiro já está certo, não se reescreve.
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        ensure_model(&dir).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn silence_and_noise_are_not_speech() {
        let dir = std::env::temp_dir().join(format!("ditok-vad-n-{}", std::process::id()));
        assert!(!has_speech(&dir, &vec![0.0; 16_000 * 3]));
        assert!(!has_speech(&dir, &noise(3.0, 5)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
