//! O motor de transcrição local (Parakeet ou Whisper, via sherpa-onnx): o áudio não
//! sai do computador e não custa nada. Implementa o mesmo `SttProvider` do OpenRouter,
//! por isso o resto da app não sabe qual dos dois está a usar.
//!
//! - **Carrega a pedido** (no `prewarm`, ao premir o atalho, ou no primeiro ditado) e
//!   **larga por inatividade** (`idle::IdleSlot`): um modelo carregado ocupa 0,7 a
//!   2,4 GB, e a app em repouso não deve. Medido: ao largar, a memória volta a ~20 MB.
//! - **Um ditado de cada vez no modelo** (`decode_lock`): o ONNX Runtime aguenta
//!   pedidos em paralelo, mas o CPU é o limite de qualquer maneira.
//! - **Pedaços curtos** (`ModelSpec::max_chunk`): a RAM do pedido cresce com o
//!   comprimento do áudio, por isso um ditado comprido é partido nas pausas.
//! - Sem a feature `local` o tipo existe na mesma (a app compila igual), mas
//!   `transcribe` diz que o motor não está incluído.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU32, Ordering},
    },
};

use async_trait::async_trait;
use bytes::Bytes;

#[cfg(feature = "local")]
use super::Usage;
#[cfg(feature = "local")]
use super::idle::IdleSlot;
use super::{SttError, SttProvider, SttRequest, Transcription};
#[cfg(feature = "local")]
use crate::local_models;
use crate::local_models::LocalModelId;

/// Esta compilação inclui o motor?
pub const AVAILABLE: bool = cfg!(feature = "local");

/// O "nome" com que um modelo local aparece nas métricas, no histórico e nos pedidos
/// (`SttRequest::model`): `local/parakeet-v3`.
pub fn slug(id: LocalModelId) -> String {
    let name = serde_json::to_value(id)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    format!("local/{name}")
}

#[cfg_attr(not(feature = "local"), allow(dead_code))] // só o motor local o usa
pub fn id_from_slug(slug: &str) -> Option<LocalModelId> {
    let name = slug.strip_prefix("local/")?;
    serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
}

/// O que as definições escolhem (para o `prewarm`, que não recebe parâmetros).
#[derive(Debug, Clone, Default)]
#[cfg_attr(not(feature = "local"), allow(dead_code))] // só o motor local o lê
struct Selection {
    id: LocalModelId,
    language: Option<String>,
}

struct Inner {
    #[cfg_attr(not(feature = "local"), allow(dead_code))] // só o motor local o lê
    models_dir: PathBuf,
    idle_minutes: AtomicU32,
    selection: Mutex<Selection>,
    #[cfg(feature = "local")]
    slot: IdleSlot<engine::Loaded>,
    #[cfg(feature = "local")]
    decode_lock: Mutex<()>,
}

/// O motor local. `Clone` é barato (um `Arc`): o estado e os ditados partilham o
/// mesmo modelo carregado.
#[derive(Clone)]
pub struct LocalStt {
    inner: Arc<Inner>,
}

impl LocalStt {
    pub fn new(models_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                models_dir,
                idle_minutes: AtomicU32::new(5),
                selection: Mutex::new(Selection::default()),
                #[cfg(feature = "local")]
                slot: IdleSlot::default(),
                #[cfg(feature = "local")]
                decode_lock: Mutex::new(()),
            }),
        }
    }

    /// Chamado quando as definições mudam: o modelo e o idioma que o `prewarm` usa e
    /// quanto tempo o modelo fica carregado sem uso.
    pub fn configure(&self, id: LocalModelId, language: Option<&str>, idle_minutes: u32) {
        self.inner
            .idle_minutes
            .store(idle_minutes.max(1), Ordering::Relaxed);
        *self
            .inner
            .selection
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Selection {
            id,
            language: language.map(str::to_owned),
        };
    }

    /// Larga já o modelo (ao mudar para a nuvem ou ao apagar o modelo).
    pub fn unload(&self) {
        #[cfg(feature = "local")]
        self.inner.slot.unload();
    }

    pub fn is_loaded(&self) -> bool {
        #[cfg(feature = "local")]
        return self.inner.slot.is_loaded();
        #[cfg(not(feature = "local"))]
        false
    }

    #[cfg_attr(not(feature = "local"), allow(dead_code))] // só o `prewarm` do motor local o usa
    fn selection(&self) -> Selection {
        self.inner
            .selection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// O áudio partido em pedaços de, no máximo, `max` (nas pausas mais silenciosas). Um
/// áudio curto fica inteiro. Não se perde nada: os pedaços juntos são o áudio todo.
#[cfg(any(feature = "local", test))]
fn split_for_model(samples: &[f32], rate: u32, max: std::time::Duration) -> Vec<Vec<f32>> {
    use std::time::Duration;

    use crate::audio::decode::Chunker;

    /// Janela em que se procura a pausa onde cortar, antes do limite.
    const SEARCH: Duration = Duration::from_secs(4);
    let max_samples = (max.as_secs_f64() * f64::from(rate)) as usize;
    if samples.len() <= max_samples {
        return vec![samples.to_vec()];
    }
    // Um pedaço sai com `target ± search`: com `target = max - search` nunca passa de `max`.
    let mut chunker = Chunker::new(
        rate,
        max.saturating_sub(SEARCH),
        SEARCH,
        Duration::from_millis(300),
    );
    let mut pieces = Vec::new();
    chunker.push(samples, &mut pieces);
    pieces
        .into_iter()
        .chain(chunker.flush())
        .map(|p| p.samples)
        .collect()
}

#[cfg(feature = "local")]
mod engine {
    use std::path::Path;

    use sherpa_onnx::{
        OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
        OfflineWhisperModelConfig,
    };

    use crate::{
        local_models::{self, Family, LocalModelId},
        stt::SttError,
    };

    /// Um modelo em memória. O idioma só conta no Whisper (o Parakeet deteta sozinho).
    pub struct Loaded {
        pub id: LocalModelId,
        pub language: Option<String>,
        pub recognizer: OfflineRecognizer,
    }

    /// O idioma que conta para este modelo: o do Parakeet é sempre `None`, para mudar
    /// de idioma não o obrigar a recarregar.
    pub fn effective_language(id: LocalModelId, language: Option<&str>) -> Option<String> {
        match local_models::spec(id).family {
            Family::Parakeet => None,
            Family::Whisper { .. } => language.filter(|l| !l.is_empty()).map(str::to_owned),
        }
    }

    pub fn load(
        id: LocalModelId,
        language: Option<String>,
        models_dir: &Path,
    ) -> Result<Loaded, SttError> {
        let spec = local_models::spec(id);
        if !spec.is_installed(models_dir) {
            return Err(SttError::ModelMissing(spec.label.to_owned()));
        }
        let dir = spec.dir(models_dir);
        let path = |name: &str| Some(dir.join(name).to_string_lossy().into_owned());

        let mut config = OfflineRecognizerConfig::default();
        // Até 8 threads: mais do que isso quase não acelera e rouba o CPU ao resto.
        config.model_config.num_threads = std::thread::available_parallelism()
            .map_or(4, |n| n.get())
            .clamp(2, 8) as i32;
        config.model_config.provider = Some("cpu".into());
        match spec.family {
            Family::Parakeet => {
                config.model_config.transducer = OfflineTransducerModelConfig {
                    encoder: path("encoder.int8.onnx"),
                    decoder: path("decoder.int8.onnx"),
                    joiner: path("joiner.int8.onnx"),
                };
                config.model_config.tokens = path("tokens.txt");
                config.model_config.model_type = Some("nemo_transducer".into());
            }
            Family::Whisper { prefix } => {
                config.model_config.whisper = OfflineWhisperModelConfig {
                    encoder: path(&format!("{prefix}-encoder.int8.onnx")),
                    decoder: path(&format!("{prefix}-decoder.int8.onnx")),
                    // Vazio = o Whisper deteta o idioma.
                    language: Some(language.clone().unwrap_or_default()),
                    task: Some("transcribe".into()),
                    tail_paddings: -1,
                    ..Default::default()
                };
                config.model_config.tokens = path(&format!("{prefix}-tokens.txt"));
            }
        }
        let started = std::time::Instant::now();
        let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
            SttError::Inference(format!(
                "não consegui carregar o modelo {} (os ficheiros podem estar estragados: apaga-o e descarrega-o outra vez)",
                spec.label
            ))
        })?;
        tracing::info!(
            model = spec.label,
            ms = started.elapsed().as_millis() as u64,
            "modelo local carregado"
        );
        Ok(Loaded {
            id,
            language,
            recognizer,
        })
    }
}

#[cfg(feature = "local")]
impl LocalStt {
    /// Corre numa thread de bloqueio: carrega o modelo se for preciso e descodifica.
    fn run(
        &self,
        id: LocalModelId,
        language: Option<&str>,
        wav: &[u8],
    ) -> Result<String, SttError> {
        let spec = local_models::spec(id);
        if !spec.is_installed(&self.inner.models_dir) {
            return Err(SttError::ModelMissing(spec.label.to_owned()));
        }
        let (samples, rate) = crate::audio::wav::decode_pcm16_mono(wav)
            .map_err(|e| SttError::Inference(format!("áudio inválido: {e}")))?;
        let language = engine::effective_language(id, language);
        let model = self.inner.slot.acquire(
            |loaded| loaded.id == id && loaded.language == language,
            || engine::load(id, language.clone(), &self.inner.models_dir),
        )?;
        // Um ditado de cada vez no modelo.
        let _turn = self
            .inner
            .decode_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut texts = Vec::new();
        for piece in split_for_model(&samples, rate, spec.max_chunk) {
            let stream = model.recognizer.create_stream();
            stream.accept_waveform(rate as i32, &piece);
            model.recognizer.decode(&stream);
            if let Some(result) = stream.get_result() {
                let text = result.text.trim();
                if !text.is_empty() {
                    texts.push(text.to_owned());
                }
            }
        }
        Ok(texts.join(" "))
    }

    /// Depois de um uso, agenda a verificação que larga o modelo se continuar parado.
    fn schedule_unload(&self) {
        let generation = self.inner.slot.finish_use();
        let wait = std::time::Duration::from_secs(
            60 * u64::from(self.inner.idle_minutes.load(Ordering::Relaxed)),
        );
        let inner = Arc::clone(&self.inner);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(wait).await;
            if inner.slot.unload_if_idle(generation) {
                tracing::info!("modelo local largado por inatividade");
            }
        });
    }
}

#[cfg(feature = "local")]
#[async_trait]
impl SttProvider for LocalStt {
    fn name(&self) -> &'static str {
        "local"
    }

    /// Ao premir o atalho: começa a carregar o modelo enquanto falas (leva ~2 s).
    async fn prewarm(&self) {
        let me = self.clone();
        let selection = self.selection();
        let _ = tauri::async_runtime::spawn_blocking(move || {
            let id = selection.id;
            let language = engine::effective_language(id, selection.language.as_deref());
            let loaded = me.inner.slot.acquire(
                |l| l.id == id && l.language == language,
                || engine::load(id, language.clone(), &me.inner.models_dir),
            );
            match loaded {
                Ok(_) => me.schedule_unload(),
                Err(e) => tracing::debug!(error = %e, "o modelo local não carregou no prewarm"),
            }
        })
        .await;
    }

    async fn transcribe(
        &self,
        audio: Bytes,
        req: &SttRequest<'_>,
    ) -> Result<Transcription, SttError> {
        let id =
            id_from_slug(req.model).ok_or_else(|| SttError::ModelMissing(req.model.to_owned()))?;
        let language = req.language.map(str::to_owned);
        let me = self.clone();
        let started = std::time::Instant::now();
        let text =
            tauri::async_runtime::spawn_blocking(move || me.run(id, language.as_deref(), &audio))
                .await
                .map_err(|e| SttError::Inference(e.to_string()))??;
        self.schedule_unload();
        Ok(Transcription {
            text,
            usage: Usage::default(),
            generation_id: None,
            elapsed: started.elapsed(),
            transport: "local",
        })
    }
}

#[cfg(not(feature = "local"))]
#[async_trait]
impl SttProvider for LocalStt {
    fn name(&self) -> &'static str {
        "local"
    }

    async fn prewarm(&self) {}

    async fn transcribe(
        &self,
        _audio: Bytes,
        _req: &SttRequest<'_>,
    ) -> Result<Transcription, SttError> {
        Err(SttError::Inference(
            "o motor local não está incluído nesta versão do Ditok".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn slugs_round_trip() {
        for id in [
            LocalModelId::ParakeetV3,
            LocalModelId::WhisperSmall,
            LocalModelId::WhisperTurbo,
            LocalModelId::WhisperMedium,
            LocalModelId::WhisperLargeV3,
        ] {
            assert_eq!(id_from_slug(&slug(id)), Some(id));
        }
        assert_eq!(slug(LocalModelId::ParakeetV3), "local/parakeet-v3");
        assert_eq!(id_from_slug("openai/whisper-large-v3-turbo"), None);
        assert_eq!(id_from_slug("local/inexistente"), None);
    }

    fn speech(seconds: f32, rate: u32) -> Vec<f32> {
        // Uma "fala" com pausas de meio segundo a cada 5 s, onde se pode cortar.
        (0..(seconds * rate as f32) as usize)
            .map(|i| {
                let t = i as f32 / rate as f32;
                if (t % 5.0) > 4.5 {
                    0.0
                } else {
                    0.3 * (t * 440.0).sin()
                }
            })
            .collect()
    }

    #[test]
    fn short_audio_is_not_split() {
        let samples = speech(12.0, 16_000);
        let pieces = split_for_model(&samples, 16_000, Duration::from_secs(30));
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].len(), samples.len());
    }

    #[test]
    fn long_audio_is_split_under_the_limit_without_losing_anything() {
        for max in [25, 30] {
            let samples = speech(100.0, 16_000);
            let limit = Duration::from_secs(max);
            let pieces = split_for_model(&samples, 16_000, limit);
            assert!(pieces.len() >= 4, "{max}: {}", pieces.len());
            let max_len = (limit.as_secs_f64() * 16_000.0) as usize;
            for p in &pieces {
                assert!(
                    p.len() <= max_len,
                    "um pedaço de {} s",
                    p.len() as f32 / 16_000.0
                );
            }
            let total: usize = pieces.iter().map(Vec::len).sum();
            assert_eq!(total, samples.len(), "os pedaços juntos são o áudio todo");
            // E pela mesma ordem.
            let joined: Vec<f32> = pieces.concat();
            assert_eq!(joined, samples);
        }
    }

    #[test]
    fn the_tail_of_a_long_dictation_is_kept_even_if_tiny() {
        // 60,05 s: o fim (50 ms) não pode ser deitado fora como no caso de um ficheiro.
        let samples = speech(60.05, 16_000);
        let pieces = split_for_model(&samples, 16_000, Duration::from_secs(30));
        let total: usize = pieces.iter().map(Vec::len).sum();
        assert_eq!(total, samples.len());
    }

    /// Com os modelos e um WAV reais (a spike deixou-os em Ditok-spike):
    /// `DITOK_MODELS=C:\Users\manue\Documents\Ditok-spike\models DITOK_WAV=...\fr.wav \
    ///  cargo test --features local live_local -- --ignored --nocapture`
    #[cfg(feature = "local")]
    #[tokio::test]
    #[ignore]
    async fn live_local() {
        let models = PathBuf::from(std::env::var("DITOK_MODELS").expect("define DITOK_MODELS"));
        let wav = std::fs::read(std::env::var("DITOK_WAV").expect("define DITOK_WAV")).unwrap();
        let local = LocalStt::new(models);
        local.configure(LocalModelId::ParakeetV3, Some("pt"), 1);
        let req = SttRequest {
            model: "local/parakeet-v3",
            language: Some("pt"),
        };
        assert!(!local.is_loaded());
        for run in 1..=2 {
            let t = local
                .transcribe(Bytes::from(wav.clone()), &req)
                .await
                .expect("a transcrição local falhou");
            println!("{run}: {:?} em {:?}: {}", t.transport, t.elapsed, t.text);
            assert!(!t.text.trim().is_empty());
            assert!(local.is_loaded(), "o modelo fica carregado entre ditados");
        }
        local.unload();
        assert!(!local.is_loaded());
    }

    #[tokio::test]
    async fn without_the_feature_or_model_the_error_says_so() {
        let local = LocalStt::new(std::env::temp_dir().join("ditok-local-sem-modelos"));
        let req = SttRequest {
            model: "local/parakeet-v3",
            language: Some("pt"),
        };
        let err = local
            .transcribe(Bytes::from_static(b"RIFF"), &req)
            .await
            .unwrap_err();
        // Com a feature: o modelo não está descarregado. Sem ela: não está incluído.
        if AVAILABLE {
            // O WAV é inválido, mas o modelo em falta é o primeiro erro que o utilizador
            // deve ver.
            assert!(
                matches!(err, SttError::ModelMissing(_) | SttError::Inference(_)),
                "{err}"
            );
        } else {
            assert!(matches!(err, SttError::Inference(m) if m.contains("não está incluído")));
        }
        assert!(!local.is_loaded());
    }
}
