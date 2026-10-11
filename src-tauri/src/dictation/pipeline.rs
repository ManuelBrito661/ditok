//! O caminho crítico de um ditado, depois de largar a tecla:
//! WAV pronto (sem voz, pára aqui) → pedido STT → comandos, expansões e dicionário →
//! (limpeza com LLM, se ligada) → colar →
//! (fora do caminho crítico) métricas, histórico, WAV de teste e, se a opção
//! "deixar no clipboard" estiver desligada, reposição do clipboard.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tauri::Emitter;

use crate::{
    audio::{AudioError, Recording},
    cleanup,
    history::HistoryEntry,
    metrics::{self, DictationMetrics},
    paste,
    state::AppState,
    stt::{SttProvider, SttRequest, openrouter::OpenRouter},
    text::{Applied, Processed, TextRules},
};

/// Gravações mais curtas do que isto são quase de certeza toques acidentais.
const MIN_AUDIO: Duration = Duration::from_millis(250);

/// Evento emitido para a janela de definições (se estiver aberta) depois de
/// guardar as métricas de um ditado, para atualizar o ecrã sem polling.
pub const EVENT_DICTATION_RECORDED: &str = "dictation-recorded";

/// Como acabou um ditado; decide o que a barra e o tray mostram a seguir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Texto colado na app em foco.
    Pasted,
    /// Nada para colar: toque acidental ou silêncio. Não é um erro.
    Nothing,
    /// A gravação, a transcrição ou a colagem falharam (há uma notificação).
    Failed,
}

/// Processa um ditado do princípio ao fim.
///
/// Recebe `Arc`s por valor: a função é dona de uma referência a cada um e pode
/// passá-la às tarefas de fundo que continuam depois de ela terminar.
pub async fn process(
    state: Arc<AppState>,
    openrouter: Arc<OpenRouter>,
    recording: impl Future<Output = Result<Recording, AudioError>>,
    released_at: Instant,
) -> Outcome {
    // Uma "fotografia" da configuração: o ditado usa-a do princípio ao fim.
    let config = state.config();
    let rules = state.text_rules();

    let rec = match recording.await {
        Ok(rec) => rec,
        Err(e) => {
            tracing::error!(error = %e, "gravação falhou");
            state.notify(&e.to_string());
            return Outcome::Failed;
        }
    };
    let release_to_wav = released_at.elapsed();
    log_recording(&rec, release_to_wav);
    if rec.audio < MIN_AUDIO {
        tracing::info!(
            audio_ms = ms(rec.audio),
            "gravação demasiado curta; ignorada"
        );
        return Outcome::Nothing;
    }
    if !rec.voice.has_speech() {
        // Só silêncio ou ruído: o Whisper inventaria uma frase, por isso nem se envia.
        tracing::info!(
            audio_ms = ms(rec.audio),
            noise_db = rec.voice.noise_db,
            peak_db = rec.voice.peak_db,
            threshold_db = rec.voice.threshold_db,
            "sem voz na gravação; não enviada"
        );
        // O WAV de diagnóstico é o que permite perceber um falso negativo.
        if config.save_last_recording {
            tauri::async_runtime::spawn_blocking(move || save_debug_wav(&state.debug_dir(), &rec));
        }
        return Outcome::Nothing;
    }

    // O pipeline fala com o STT através do trait; o tipo concreto só é preciso
    // para a limpeza, que é uma funcionalidade específica do OpenRouter.
    let stt: &dyn SttProvider = &*openrouter;
    let req = SttRequest {
        model: &config.stt_model,
        language: config.language.as_deref(),
    };
    let mut m = DictationMetrics {
        at: metrics::now(),
        provider: stt.name(),
        model: config.stt_model.clone(),
        transport: None,
        audio_ms: ms(rec.audio),
        wav_bytes: rec.wav.len(),
        mic_start_ms: rec.mic_start.map(ms),
        release_to_wav_ms: ms(release_to_wav),
        stt_ms: None,
        cleanup_ms: None,
        paste_ms: None,
        total_ms: None,
        cost_usd: None,
        cleanup_cost_usd: None,
        chars: 0,
        generation_id: None,
        error: None,
    };
    let mut history_entry = None;
    let mut applied = Applied::default();

    let outcome = match stt.transcribe(rec.wav.clone(), &req).await {
        Err(e) => {
            tracing::error!(error = %e, model = %config.stt_model, "transcrição falhou");
            state.notify(&format!("Transcrição falhou: {e}"));
            m.error = Some(e.to_string());
            Outcome::Failed
        }
        Ok(t) => {
            m.stt_ms = Some(ms(t.elapsed));
            m.transport = Some(t.transport);
            m.cost_usd = t.usage.cost;
            m.generation_id = t.generation_id;
            let raw = t.text.trim().to_owned();

            if raw.is_empty() {
                tracing::info!("nenhuma fala detetada; nada para colar");
                Outcome::Nothing
            } else {
                // Comandos, expansões e dicionário: neste computador, em microssegundos.
                let local = rules.apply(&raw);
                applied = local.applied;
                let cleaned = if config.cleanup_enabled
                    && let Some(input) = local.cleanup_input()
                {
                    clean_with_llm(
                        &openrouter,
                        &config.cleanup_model,
                        &rules,
                        &local,
                        input,
                        &mut m,
                    )
                    .await
                } else {
                    None
                };
                let text = cleaned.unwrap_or(local.text);
                m.chars = text.chars().count();
                let restore_clipboard = !config.keep_in_clipboard;
                let outcome =
                    paste_text(&state, text.clone(), released_at, restore_clipboard, &mut m).await;
                // Também quando a colagem falha: é aí que o histórico mais faz falta.
                if config.history_enabled {
                    history_entry = Some(HistoryEntry {
                        at: m.at,
                        raw_text: (text != raw).then_some(raw),
                        text,
                        model: config.stt_model.clone(),
                        audio_ms: m.audio_ms,
                        total_ms: m.total_ms,
                    });
                }
                outcome
            }
        }
    };

    tracing::info!(
        model = %m.model,
        transport = m.transport,
        audio_ms = m.audio_ms,
        trimmed_ms = ms(rec.trimmed),
        release_to_wav_ms = m.release_to_wav_ms,
        stt_ms = m.stt_ms,
        cleanup_ms = m.cleanup_ms,
        paste_ms = m.paste_ms,
        total_ms = m.total_ms,
        cost_usd = m.cost_usd,
        cleanup_cost_usd = m.cleanup_cost_usd,
        chars = m.chars,
        commands = applied.commands,
        replaced = applied.replaced,
        expanded = applied.expanded,
        "ditado"
    );

    // Já colámos: o que falta não atrasa o utilizador.
    tauri::async_runtime::spawn_blocking(move || {
        match state.metrics.record(&m, config.monthly_budget_usd) {
            Ok(Some(alert)) => {
                tracing::info!(?alert, "aviso de orçamento");
                state.notify(&alert.message());
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "não foi possível guardar as métricas"),
        }
        if let Some(entry) = history_entry
            && let Err(e) = state.history.push(entry, config.history_limit)
        {
            tracing::warn!(error = %e, "não foi possível guardar o histórico");
        }
        if config.save_last_recording {
            save_debug_wav(&state.debug_dir(), &rec);
        }
        // Sem janela aberta não há ninguém a ouvir, e o `emit` não custa nada.
        let _ = state.app.emit(EVENT_DICTATION_RECORDED, ());
    });
    outcome
}

/// A limpeza com IA é um extra: se falhar, demorar mais de 6 s ou estragar o
/// texto (mexer nas linhas ou nas expansões), devolve `None` e cola-se o texto
/// sem limpeza.
async fn clean_with_llm(
    openrouter: &OpenRouter,
    model: &str,
    rules: &TextRules,
    local: &Processed,
    input: &str,
    m: &mut DictationMetrics,
) -> Option<String> {
    match cleanup::clean(openrouter, model, input, rules.vocabulary()).await {
        Ok(c) => {
            m.cleanup_ms = Some(ms(c.elapsed));
            m.cleanup_cost_usd = c.cost;
            local
                .merge_cleaned(&c.text)
                .inspect_err(|e| {
                    tracing::warn!(error = %e, model, "resposta da limpeza ignorada; a colar o texto sem limpeza");
                })
                .ok()
        }
        Err(e) => {
            tracing::warn!(error = %e, model, "limpeza falhou; a colar o texto sem limpeza");
            None
        }
    }
}

async fn paste_text(
    state: &AppState,
    text: String,
    released_at: Instant,
    restore_clipboard: bool,
    m: &mut DictationMetrics,
) -> Outcome {
    // `move` passa a posse do `text` para a thread de bloqueio.
    match tauri::async_runtime::spawn_blocking(move || paste::paste(&text)).await {
        Ok(Ok((took, restore))) => {
            m.paste_ms = Some(ms(took));
            m.total_ms = Some(ms(released_at.elapsed()));
            if restore_clipboard {
                tauri::async_runtime::spawn_blocking(move || {
                    std::thread::sleep(paste::RESTORE_AFTER);
                    restore.run();
                });
            }
            // Senão, o `restore` é largado aqui sem correr e o ditado fica no clipboard.
            Outcome::Pasted
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "colagem falhou");
            state.notify(&e.to_string());
            m.error = Some(e.to_string());
            Outcome::Failed
        }
        Err(e) => {
            tracing::error!(error = %e, "tarefa de colagem falhou");
            m.error = Some(e.to_string());
            Outcome::Failed
        }
    }
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

fn log_recording(rec: &Recording, release_to_wav: Duration) {
    tracing::debug!(
        audio_ms = ms(rec.audio),
        trimmed_ms = ms(rec.trimmed),
        voiced_ms = rec.voice.voiced_ms,
        noise_db = rec.voice.noise_db,
        peak_db = rec.voice.peak_db,
        threshold_db = rec.voice.threshold_db,
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
