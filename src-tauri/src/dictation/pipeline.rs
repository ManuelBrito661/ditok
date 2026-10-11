//! O caminho crítico de um ditado, depois de largar a tecla:
//! WAV pronto → pedido STT → colar → (fora do caminho crítico) métricas, WAV de
//! teste e reposição do clipboard.

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

use crate::{
    audio::{AudioError, Recording},
    config::{Config, Provider},
    metrics::{self, DictationMetrics, MetricsStore},
    paste,
    secrets::{self, ApiKey},
    stt::{SttProvider, SttRequest, openrouter::OpenRouter},
};

/// Gravações mais curtas do que isto são quase de certeza toques acidentais.
const MIN_AUDIO: Duration = Duration::from_millis(250);

/// Dados partilhados (só leitura) entre o orquestrador e as tarefas de
/// processamento. Vive num `Arc`: cada tarefa leva uma referência, não uma cópia.
pub struct Pipeline {
    pub app: AppHandle,
    pub config: Config,
    pub http: reqwest::Client,
    pub metrics: MetricsStore,
    pub debug_dir: PathBuf,
}

impl Pipeline {
    /// Cria o provider configurado se a chave existir no cofre. `Ok(None)` = falta a chave.
    pub fn load_provider(&self) -> Result<Option<Arc<dyn SttProvider>>, secrets::SecretError> {
        Ok(match self.config.provider {
            Provider::OpenRouter => secrets::get(ApiKey::OpenRouter)?.map(|key| {
                // `as Arc<dyn …>`: a partir daqui só sabemos que é "um SttProvider".
                Arc::new(OpenRouter::new(
                    self.http.clone(),
                    key,
                    self.config.openrouter_transport,
                )) as Arc<dyn SttProvider>
            }),
        })
    }

    pub fn missing_key_message(&self) -> String {
        let key = match self.config.provider {
            Provider::OpenRouter => ApiKey::OpenRouter,
        };
        format!(
            "Falta a chave do {key}. Num terminal, na pasta src-tauri: cargo run --bin set-key -- {}",
            key.to_string().to_lowercase()
        )
    }

    pub fn notify(&self, body: &str) {
        if let Err(e) = self
            .app
            .notification()
            .builder()
            .title("Ditok")
            .body(body)
            .show()
        {
            tracing::warn!(error = %e, "não foi possível mostrar a notificação");
        }
    }

    fn model(&self) -> &str {
        match self.config.provider {
            Provider::OpenRouter => &self.config.openrouter_model,
        }
    }

    /// Processa um ditado do princípio ao fim. Devolve `true` se colou texto.
    ///
    /// `self: Arc<Self>` permite chamar o método num `Arc<Pipeline>` e mover esse
    /// `Arc` para dentro das tarefas de fundo lançadas no fim.
    pub async fn process(
        self: Arc<Self>,
        stt: Arc<dyn SttProvider>,
        recording: impl Future<Output = Result<Recording, AudioError>>,
        released_at: Instant,
    ) -> bool {
        let rec = match recording.await {
            Ok(rec) => rec,
            Err(e) => {
                tracing::error!(error = %e, "gravação falhou");
                self.notify(&e.to_string());
                return false;
            }
        };
        let release_to_wav = released_at.elapsed();
        log_recording(&rec, release_to_wav);
        if rec.audio < MIN_AUDIO {
            tracing::info!(
                audio_ms = ms(rec.audio),
                "gravação demasiado curta; ignorada"
            );
            return false;
        }

        let model = self.model().to_owned();
        let req = SttRequest {
            model: &model,
            language: self.config.language.as_deref(),
        };
        let mut m = DictationMetrics {
            at: metrics::now(),
            provider: stt.name(),
            model: model.clone(),
            transport: None,
            audio_ms: ms(rec.audio),
            wav_bytes: rec.wav.len(),
            mic_start_ms: rec.mic_start.map(ms),
            release_to_wav_ms: ms(release_to_wav),
            stt_ms: None,
            paste_ms: None,
            total_ms: None,
            cost_usd: None,
            chars: 0,
            generation_id: None,
            error: None,
        };

        let pasted = match stt.transcribe(rec.wav.clone(), &req).await {
            Err(e) => {
                tracing::error!(error = %e, provider = stt.name(), model, "transcrição falhou");
                self.notify(&format!("Transcrição falhou: {e}"));
                m.error = Some(e.to_string());
                false
            }
            Ok(t) => {
                m.stt_ms = Some(ms(t.elapsed));
                m.transport = Some(t.transport);
                m.cost_usd = t.usage.cost;
                m.generation_id = t.generation_id;
                let text = t.text.trim().to_owned();
                m.chars = text.chars().count();
                if text.is_empty() {
                    tracing::info!("nenhuma fala detetada; nada para colar");
                    false
                } else {
                    self.paste(text, released_at, &mut m).await
                }
            }
        };

        tracing::info!(
            provider = m.provider,
            model = %m.model,
            transport = m.transport,
            audio_ms = m.audio_ms,
            release_to_wav_ms = m.release_to_wav_ms,
            stt_ms = m.stt_ms,
            paste_ms = m.paste_ms,
            total_ms = m.total_ms,
            cost_usd = m.cost_usd,
            chars = m.chars,
            "ditado"
        );

        // Já colámos: o que falta não atrasa o utilizador.
        let this = Arc::clone(&self);
        tauri::async_runtime::spawn_blocking(move || {
            if let Err(e) = this.metrics.record(&m) {
                tracing::warn!(error = %e, "não foi possível guardar as métricas");
            }
            if this.config.save_last_recording {
                save_debug_wav(&this.debug_dir, &rec);
            }
        });
        pasted
    }

    async fn paste(&self, text: String, released_at: Instant, m: &mut DictationMetrics) -> bool {
        // `move` passa a posse do `text` para a thread de bloqueio.
        match tauri::async_runtime::spawn_blocking(move || paste::paste(&text)).await {
            Ok(Ok((took, restore))) => {
                m.paste_ms = Some(ms(took));
                m.total_ms = Some(ms(released_at.elapsed()));
                tauri::async_runtime::spawn_blocking(move || {
                    std::thread::sleep(paste::RESTORE_AFTER);
                    restore.run();
                });
                true
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, "colagem falhou");
                self.notify(&e.to_string());
                m.error = Some(e.to_string());
                false
            }
            Err(e) => {
                tracing::error!(error = %e, "tarefa de colagem falhou");
                m.error = Some(e.to_string());
                false
            }
        }
    }
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

fn log_recording(rec: &Recording, release_to_wav: Duration) {
    tracing::debug!(
        audio_ms = ms(rec.audio),
        device = %rec.device,
        device_rate = rec.device_rate,
        device_channels = rec.device_channels,
        mic_start_ms = rec.mic_start.map(ms),
        finalize_ms = ms(rec.finalize),
        release_to_wav_ms = ms(release_to_wav),
        wav_kb = rec.wav.len() / 1024,
        dropped = rec.dropped_samples,
        "gravação pronta"
    );
    if rec.dropped_samples > 0 {
        tracing::warn!(
            dropped = rec.dropped_samples,
            "perderam-se amostras de áudio"
        );
    }
}

fn save_debug_wav(dir: &std::path::Path, rec: &Recording) {
    let path = dir.join("last.wav");
    let result = std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, &rec.wav));
    match result {
        Ok(()) => tracing::debug!(path = %path.display(), "WAV de teste guardado"),
        Err(e) => tracing::warn!(error = %e, "não foi possível guardar o WAV de teste"),
    }
}
