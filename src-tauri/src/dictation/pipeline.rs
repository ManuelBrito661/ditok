//! O caminho crítico de um ditado, depois de largar a tecla:
//! WAV pronto (sem voz, pára aqui) → pedido STT → comandos, expansões e dicionário →
//! (limpeza com LLM, se ligada) → colar, se a janela em foco ainda for a do início
//! (senão fica só no clipboard) →
//! (fora do caminho crítico) métricas, histórico, WAV de teste e, se a opção
//! "deixar no clipboard" estiver desligada, reposição do clipboard.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use tauri::Emitter;

use super::{failure::FailReason, selection::Selection};
use crate::{
    audio::{AudioError, Recording},
    cleanup, command,
    config::Config,
    focus,
    history::HistoryEntry,
    metrics::{self, DictationMetrics},
    paste,
    state::AppState,
    stt::{SttError, SttProvider, SttRequest, openrouter::OpenRouter},
    text::{Applied, Processed, TextRules},
};

/// Gravações mais curtas do que isto são quase de certeza toques acidentais.
const MIN_AUDIO: Duration = Duration::from_millis(250);

/// Evento emitido para a janela de definições (se estiver aberta) depois de
/// guardar as métricas de um ditado, para atualizar o ecrã sem polling.
pub const EVENT_DICTATION_RECORDED: &str = "dictation-recorded";

/// Como acabou um ditado; decide o que a barra e o tray mostram a seguir.
/// (Sem `Copy`/`PartialEq`: `Failed` pode levar a gravação consigo.)
#[derive(Debug)]
pub enum Outcome {
    /// Texto colado na app em foco.
    Pasted,
    /// Não se colou (a janela em foco mudou, ou o sistema não deixou simular o
    /// Ctrl+V), mas o texto ficou no clipboard.
    Copied,
    /// Nada para colar: toque acidental ou silêncio. Não é um erro.
    Nothing,
    /// A gravação, a transcrição ou a colagem falharam (há uma notificação).
    Failed {
        reason: FailReason,
        /// A gravação, quando a falha foi depois de a termos (ex.: sem rede):
        /// fica guardada para o "Tentar de novo".
        retry: Option<Recording>,
    },
}

impl Outcome {
    fn failed(reason: FailReason) -> Self {
        Self::Failed {
            reason,
            retry: None,
        }
    }
}

/// Partilhado entre o processamento e o orquestrador para decidir, sem corridas,
/// quem ganha: o cancelamento (Esc/✕) ou a colagem.
///
/// Porquê: abortar a tarefa assíncrona não pára uma thread de `spawn_blocking`
/// que já esteja a colar. Por isso, antes de colar, o processamento
/// "compromete-se"; a partir daí o cancelamento é recusado e o ditado acaba
/// normalmente (com histórico, métricas e reposição do clipboard).
///
/// Um único `AtomicU8` com `compare_exchange` garante que só uma das duas
/// transições (a correr → comprometido, ou a correr → cancelado) acontece, mesmo
/// que as duas threads tentem ao mesmo tempo. `Clone` só copia o `Arc`.
#[derive(Debug, Clone, Default)]
pub struct Commit(Arc<AtomicU8>);

const RUNNING: u8 = 0;
const COMMITTED: u8 = 1;
const CANCELLED: u8 = 2;

impl Commit {
    /// Chamado pelo processamento antes de colar (e ao terminar). `false` se já
    /// foi cancelado: nesse caso não se cola nada.
    pub fn try_commit(&self) -> bool {
        match self
            .0
            .compare_exchange(RUNNING, COMMITTED, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => true,
            Err(current) => current == COMMITTED,
        }
    }

    /// Chamado pelo orquestrador no Esc/✕. `false` se já é tarde (está a colar
    /// ou já acabou): o cancelamento é ignorado.
    pub fn try_cancel(&self) -> bool {
        self.0
            .compare_exchange(RUNNING, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

/// Se o ditado for cancelado depois de a transcrição já ter sido paga (ex.: a
/// meio da limpeza com IA), a tarefa é abortada e o código depois do `.await`
/// nunca corre. Este guarda corre no `Drop` (que acontece mesmo num abort) e
/// regista as métricas do STT, para o custo não se perder. No fim normal é
/// "desarmado" e as métricas completas são registadas uma única vez.
struct SttMetricsGuard {
    state: Arc<AppState>,
    budget: Option<f64>,
    metrics: Option<DictationMetrics>,
}

impl SttMetricsGuard {
    fn disarm(&mut self) {
        self.metrics = None;
    }
}

impl Drop for SttMetricsGuard {
    fn drop(&mut self) {
        let Some(mut m) = self.metrics.take() else {
            return;
        };
        m.error = Some("cancelado depois da transcrição".into());
        let state = Arc::clone(&self.state);
        let budget = self.budget;
        tracing::info!("ditado cancelado depois da transcrição; custo registado");
        tauri::async_runtime::spawn_blocking(move || {
            record_metrics(&state, &m, budget);
            let _ = state.app.emit(EVENT_DICTATION_RECORDED, ());
        });
    }
}

/// Bloqueante. Guarda as métricas e avisa se o orçamento foi ultrapassado.
fn record_metrics(state: &AppState, m: &DictationMetrics, budget: Option<f64>) {
    match state.metrics.record(m, budget) {
        Ok(Some(alert)) => {
            tracing::info!(?alert, "aviso de orçamento");
            state.notify(&alert.message());
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "não foi possível guardar as métricas"),
    }
}

/// Processa um ditado do princípio ao fim.
///
/// Recebe `Arc`s por valor: a função é dona de uma referência a cada um e pode
/// passá-la às tarefas de fundo que continuam depois de ela terminar.
///
/// `target`: a janela em primeiro plano quando o ditado começou (só se cola se
/// ainda for ela, ver `focus.rs`) e a app dona dela (decide o estilo).
pub async fn process(
    state: Arc<AppState>,
    openrouter: Arc<OpenRouter>,
    recording: impl Future<Output = Result<Recording, AudioError>>,
    released_at: Instant,
    target: focus::Target,
    commit: Commit,
) -> Outcome {
    let (rec, release_to_wav) = match ready_recording(&state, recording, released_at).await {
        Ok(ready) => ready,
        Err(outcome) => return outcome,
    };
    transcribe_and_paste(
        state,
        openrouter,
        rec,
        released_at,
        release_to_wav,
        target,
        commit,
    )
    .await
}

/// Espera pela gravação e decide se vale a pena enviá-la. `Err` traz o fim do
/// ditado (sem microfone, toque acidental, só silêncio), igual nos dois modos.
async fn ready_recording(
    state: &Arc<AppState>,
    recording: impl Future<Output = Result<Recording, AudioError>>,
    released_at: Instant,
) -> Result<(Recording, Duration), Outcome> {
    let rec = match recording.await {
        Ok(rec) => rec,
        // O microfone nem abriu: o motivo já foi notificado quando falhou
        // (`DictationEvent::AudioFailed`). Repetir a notificação só confundia.
        Err(AudioError::NotRecording) => {
            tracing::warn!("não havia gravação (o microfone não abriu)");
            return Err(Outcome::failed(FailReason::NoMicrophone));
        }
        Err(e) => {
            tracing::error!(error = %e, "gravação falhou");
            state.notify(&e.to_string());
            return Err(Outcome::failed(FailReason::from_audio(&e)));
        }
    };
    let release_to_wav = released_at.elapsed();
    log_recording(&rec, release_to_wav);
    if rec.audio < MIN_AUDIO {
        tracing::info!(
            audio_ms = ms(rec.audio),
            "gravação demasiado curta; ignorada"
        );
        return Err(Outcome::Nothing);
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
        if state.config().save_last_recording {
            let state = Arc::clone(state);
            tauri::async_runtime::spawn_blocking(move || save_debug_wav(&state.debug_dir(), &rec));
        }
        return Err(Outcome::Nothing);
    }
    Ok((rec, release_to_wav))
}

/// "Tentar de novo": volta a enviar uma gravação cuja transcrição falhou. As
/// métricas contam a partir de agora (o tempo de gravação já passou há muito).
pub async fn retry(
    state: Arc<AppState>,
    openrouter: Arc<OpenRouter>,
    rec: Recording,
    target: focus::Target,
    commit: Commit,
) -> Outcome {
    tracing::info!(
        audio_ms = ms(rec.audio),
        "a tentar de novo a última gravação"
    );
    transcribe_and_paste(
        state,
        openrouter,
        rec,
        Instant::now(),
        Duration::ZERO,
        target,
        commit,
    )
    .await
}

/// Do pedido STT à colagem, para uma gravação que já sabemos ter voz.
async fn transcribe_and_paste(
    state: Arc<AppState>,
    openrouter: Arc<OpenRouter>,
    rec: Recording,
    released_at: Instant,
    release_to_wav: Duration,
    target: focus::Target,
    commit: Commit,
) -> Outcome {
    // Uma "fotografia" da configuração: o ditado usa-a do princípio ao fim.
    let config = state.config();
    let rules = state.text_rules();
    // O estilo vem da app onde o ditado começou; com o nível, decide se (e
    // como) o texto passa pelo LLM.
    let style = config.style_for(target.app.as_deref());
    let plan = cleanup::plan(config.cleanup_level, style);

    // O pipeline fala com o STT através do trait; o tipo concreto só é preciso
    // para a limpeza, que é uma funcionalidade específica do OpenRouter.
    let stt: &dyn SttProvider = &*openrouter;
    let req = SttRequest {
        model: &config.stt_model,
        language: config.language.as_deref(),
    };
    let mut m = new_metrics(stt, &config, &rec, release_to_wav, false);
    let mut history_entry = None;
    let mut applied = Applied::default();
    let mut stt_guard: Option<SttMetricsGuard> = None;

    let outcome = match stt.transcribe(rec.wav.clone(), &req).await {
        Err(e) => {
            tracing::error!(error = %e, model = %config.stt_model, "transcrição falhou");
            state.notify(&format!("Transcrição falhou: {e}"));
            m.error = Some(e.to_string());
            Outcome::Failed {
                reason: FailReason::from_stt(&e),
                // Só para erros que podem correr bem à segunda. `clone` barato (o
                // WAV é `Bytes`): o original ainda vai para o WAV de diagnóstico.
                retry: FailReason::retryable(&e).then(|| rec.clone()),
            }
        }
        Ok(t) => {
            m.stt_ms = Some(ms(t.elapsed));
            m.transport = Some(t.transport);
            m.cost_usd = t.usage.cost;
            m.generation_id = t.generation_id;
            // A partir daqui a transcrição já foi paga: se o ditado for cancelado,
            // o guarda regista pelo menos isto.
            stt_guard = Some(SttMetricsGuard {
                state: Arc::clone(&state),
                budget: config.monthly_budget_usd,
                metrics: Some(m.clone()),
            });
            let raw = t.text.trim().to_owned();

            if raw.is_empty() {
                tracing::info!("nenhuma fala detetada; nada para colar");
                Outcome::Nothing
            } else {
                // Comandos, expansões e dicionário: neste computador, em microssegundos.
                let local = rules.apply(&raw);
                applied = local.applied;
                let cleaned = if let Some(plan) = plan
                    && let Some(input) = local.cleanup_input()
                {
                    clean_with_llm(
                        &openrouter,
                        &config.cleanup_model,
                        plan,
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
                // O custo da limpeza já está em `m`: o guarda tem de o levar, para
                // que um cancelamento a seguir o registe.
                if let Some(guard) = &mut stt_guard {
                    guard.metrics = Some(m.clone());
                }
                // Ponto sem retorno: daqui em diante o Esc já não cancela (a thread
                // de colagem não se pode parar a meio). Se já foi cancelado, pára aqui.
                if !commit.try_commit() {
                    tracing::info!("cancelado antes de colar; nada foi colado");
                    return Outcome::Nothing;
                }
                // Para o "Colar o último ditado" (só falha se a app estiver a sair).
                let _ = state
                    .events
                    .send(super::DictationEvent::LastText(text.clone()));
                let restore_clipboard = !config.keep_in_clipboard;
                let outcome = paste_text(
                    &state,
                    text.clone(),
                    released_at,
                    restore_clipboard,
                    target.window,
                    paste::Before::ReadNow,
                    &mut m,
                )
                .await;
                // Também quando a colagem falha: é aí que o histórico mais faz falta.
                if config.history_enabled {
                    history_entry = Some(HistoryEntry {
                        id: String::new(), // dado pelo `HistoryStore::push`
                        at: m.at,
                        raw_text: (text != raw).then_some(raw),
                        text,
                        model: config.stt_model.clone(),
                        audio_ms: m.audio_ms,
                        total_ms: m.total_ms,
                        pasted: matches!(outcome, Outcome::Pasted),
                        app: target.app.clone(),
                        style: cleanup::shown_style(config.cleanup_level, style),
                        instruction: None,
                        style_used: Some(style),
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
        level = ?config.cleanup_level,
        ?style,
        app = target.app.as_deref(),
        commands = applied.commands,
        replaced = applied.replaced,
        expanded = applied.expanded,
        "ditado"
    );

    // As métricas completas vão já a seguir: o guarda já não faz falta.
    if let Some(guard) = &mut stt_guard {
        guard.disarm();
    }
    record_in_background(state, config, m, history_entry, rec);
    outcome
}

/// Já colámos: o que falta (métricas, histórico, WAV de teste) não atrasa o
/// utilizador, por isso vai para uma thread de bloqueio.
fn record_in_background(
    state: Arc<AppState>,
    config: Arc<Config>,
    m: DictationMetrics,
    history_entry: Option<HistoryEntry>,
    rec: Recording,
) {
    tauri::async_runtime::spawn_blocking(move || {
        record_metrics(&state, &m, config.monthly_budget_usd);
        if let Some(entry) = history_entry {
            // Os comandos não se reprocessam (precisavam da mesma seleção): o
            // áudio deles não serve para nada.
            let is_command = entry.instruction.is_some();
            match state.history.push(entry, config.history_limit) {
                // A opção é lida outra vez dentro do `save_audio_if`, com o lock
                // trancado: se foi desligada a meio do ditado, não fica WAV.
                Ok(id) if !is_command => {
                    let keep = || state.config().keep_audio;
                    if let Err(e) = state.history.save_audio_if(&id, &rec.wav, keep) {
                        tracing::warn!(error = %e, "não foi possível guardar o áudio do ditado");
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "não foi possível guardar o histórico"),
            }
        }
        if config.save_last_recording {
            save_debug_wav(&state.debug_dir(), &rec);
        }
        // Sem janela aberta não há ninguém a ouvir, e o `emit` não custa nada.
        let _ = state.app.emit(EVENT_DICTATION_RECORDED, ());
    });
}

/// As métricas de uma gravação antes de a enviar (o resto vai sendo preenchido).
fn new_metrics(
    stt: &dyn SttProvider,
    config: &Config,
    rec: &Recording,
    release_to_wav: Duration,
    command: bool,
) -> DictationMetrics {
    DictationMetrics {
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
        file: false,
        command,
        error: None,
    }
}

/// Modo comando, do fim da gravação à colagem: a instrução falada passa pelo
/// STT, o LLM aplica-a à seleção (ou escreve texto novo, sem seleção) e o
/// resultado substitui a seleção.
///
/// Diferenças do ditado: sem regras locais nem limpeza (a instrução não é texto
/// a colar), sem "Tentar de novo" (repetir precisava da mesma seleção) e com o
/// clipboard já mexido pela cópia da seleção (ver `selection.rs`).
pub async fn command(
    state: Arc<AppState>,
    openrouter: Arc<OpenRouter>,
    recording: impl Future<Output = Result<Recording, AudioError>>,
    released_at: Instant,
    target: focus::Target,
    selection: Selection,
    commit: Commit,
) -> Outcome {
    // Se sairmos daqui sem colar (falha, silêncio, cancelamento), o `Drop` da
    // `selection` repõe o clipboard. Não é preciso fazer nada nesses caminhos.
    let (rec, release_to_wav) = match ready_recording(&state, recording, released_at).await {
        Ok(ready) => ready,
        Err(outcome) => return outcome,
    };
    let config = state.config();
    let model = config.cleanup_model.trim().to_owned();
    if model.is_empty() {
        tracing::error!("modo comando sem modelo de texto");
        state.notify("O modo comando precisa de um modelo de texto: escolhe um em Definições → Limpeza com IA.");
        return Outcome::failed(FailReason::NoModel);
    }

    let stt: &dyn SttProvider = &*openrouter;
    let req = SttRequest {
        model: &config.stt_model,
        language: config.language.as_deref(),
    };
    let mut m = new_metrics(stt, &config, &rec, release_to_wav, true);
    let mut history_entry = None;
    let mut stt_guard: Option<SttMetricsGuard> = None;

    // A cópia da seleção começou antes deste processamento (ver `Selection`),
    // por isso corre ao mesmo tempo que a transcrição: `join!` espera pelas duas.
    let (transcribed, selected) =
        tokio::join!(stt.transcribe(rec.wav.clone(), &req), selection.text());

    let outcome = match transcribed {
        Err(e) => {
            tracing::error!(error = %e, model = %config.stt_model, "transcrição do comando falhou");
            state.notify(&format!("Transcrição falhou: {e}"));
            m.error = Some(e.to_string());
            Outcome::failed(FailReason::from_stt(&e))
        }
        Ok(t) => {
            m.stt_ms = Some(ms(t.elapsed));
            m.transport = Some(t.transport);
            m.cost_usd = t.usage.cost;
            m.generation_id = t.generation_id;
            stt_guard = Some(SttMetricsGuard {
                state: Arc::clone(&state),
                budget: config.monthly_budget_usd,
                metrics: Some(m.clone()),
            });
            let instruction = t.text.trim().to_owned();
            let chars = selected.as_ref().map_or(0, |s| s.chars().count());
            if instruction.is_empty() {
                tracing::info!("comando sem instrução; nada para fazer");
                Outcome::Nothing
            } else if chars > command::MAX_SELECTION {
                tracing::info!(
                    chars,
                    max = command::MAX_SELECTION,
                    "seleção grande demais para um comando"
                );
                state.notify(&format!(
                    "A seleção tem {chars} caracteres; o modo comando aceita até {}. Seleciona menos texto.",
                    command::MAX_SELECTION
                ));
                m.error = Some("seleção grande demais".into());
                Outcome::failed(FailReason::SelectionTooLarge)
            } else {
                tracing::info!(
                    instruction_chars = instruction.chars().count(),
                    selection_chars = chars,
                    "a pedir o comando ao LLM"
                );
                match command::run(&openrouter, &model, &instruction, selected.as_deref()).await {
                    Err(e) => {
                        tracing::error!(error = %e, model, "comando falhou");
                        state.notify(&format!("O comando falhou: {e}"));
                        m.error = Some(e.to_string());
                        let reason = match e {
                            SttError::Decode(_) => FailReason::BadAnswer,
                            ref e => FailReason::from_stt(e),
                        };
                        Outcome::failed(reason)
                    }
                    Ok(answer) => {
                        m.cleanup_ms = Some(ms(answer.elapsed));
                        m.cleanup_cost_usd = answer.cost;
                        let text = answer.text;
                        m.chars = text.chars().count();
                        // Como no ditado: o guarda leva o custo do LLM, se o comando
                        // for cancelado a seguir.
                        if let Some(guard) = &mut stt_guard {
                            guard.metrics = Some(m.clone());
                        }
                        // Ponto sem retorno, como no ditado.
                        if !commit.try_commit() {
                            tracing::info!("comando cancelado antes de colar");
                            return Outcome::Nothing;
                        }
                        // A colagem fica com o clipboard de antes da cópia: é
                        // esse que se repõe (ou não, com "deixar no clipboard").
                        // Sem cópia, lê-se o clipboard atual.
                        let before = match selection.take() {
                            Some(captured) => captured.into_before(),
                            None => paste::Before::ReadNow,
                        };
                        let _ = state
                            .events
                            .send(super::DictationEvent::LastText(text.clone()));
                        let outcome = paste_text(
                            &state,
                            text.clone(),
                            released_at,
                            !config.keep_in_clipboard,
                            target.window,
                            before,
                            &mut m,
                        )
                        .await;
                        if config.history_enabled {
                            history_entry = Some(HistoryEntry {
                                id: String::new(),
                                at: m.at,
                                // O texto que foi substituído (sem seleção, nada).
                                raw_text: selected,
                                text,
                                model: model.clone(),
                                audio_ms: m.audio_ms,
                                total_ms: m.total_ms,
                                pasted: matches!(outcome, Outcome::Pasted),
                                app: target.app.clone(),
                                style: None,
                                instruction: Some(instruction),
                                style_used: None,
                            });
                        }
                        outcome
                    }
                }
            }
        }
    };

    tracing::info!(
        stt_model = %m.model,
        model,
        audio_ms = m.audio_ms,
        stt_ms = m.stt_ms,
        llm_ms = m.cleanup_ms,
        paste_ms = m.paste_ms,
        total_ms = m.total_ms,
        cost_usd = m.cost_usd,
        llm_cost_usd = m.cleanup_cost_usd,
        chars = m.chars,
        app = target.app.as_deref(),
        "comando"
    );
    if let Some(guard) = &mut stt_guard {
        guard.disarm();
    }
    record_in_background(state, config, m, history_entry, rec);
    outcome
}

/// O resultado de "Reprocessar": o texto novo e o que o STT devolveu.
#[derive(Debug)]
pub struct Reprocessed {
    pub text: String,
    /// O texto do STT, só se a limpeza o mudou (como no `HistoryEntry`).
    pub raw_text: Option<String>,
    pub model: String,
    pub total_ms: u64,
}

/// "Reprocessar" no Início: volta a transcrever um áudio guardado com o modelo
/// de STT atual e a limpá-lo com o nível atual e o estilo do ditado original.
/// Não cola nada (a janela do Ditok é que tem o foco). As métricas contam como
/// um ditado: o custo é real.
///
/// `Err` traz uma frase pronta a mostrar.
pub async fn reprocess(
    state: Arc<AppState>,
    openrouter: Arc<OpenRouter>,
    wav: bytes::Bytes,
    original: &HistoryEntry,
) -> Result<Reprocessed, String> {
    let started = Instant::now();
    let config = state.config();
    let rules = state.text_rules();
    // O estilo do ditado original, como foi usado. Entradas antigas, sem ele,
    // ficam com o da app onde foi ditado (o que se sabe hoje).
    let style = original
        .style_used
        .or(original.style)
        .unwrap_or_else(|| config.style_for(original.app.as_deref()));
    let plan = cleanup::plan(config.cleanup_level, style);

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
        audio_ms: original.audio_ms,
        wav_bytes: wav.len(),
        mic_start_ms: None,
        release_to_wav_ms: 0,
        stt_ms: None,
        cleanup_ms: None,
        paste_ms: None,
        total_ms: None,
        cost_usd: None,
        cleanup_cost_usd: None,
        chars: 0,
        generation_id: None,
        file: false,
        command: false,
        error: None,
    };

    let result = match stt.transcribe(wav, &req).await {
        Err(e) => {
            tracing::error!(error = %e, model = %config.stt_model, "reprocessamento: transcrição falhou");
            m.error = Some(e.to_string());
            Err(format!("Transcrição falhou: {e}"))
        }
        Ok(t) => {
            m.stt_ms = Some(ms(t.elapsed));
            m.transport = Some(t.transport);
            m.cost_usd = t.usage.cost;
            m.generation_id = t.generation_id;
            let raw = t.text.trim().to_owned();
            if raw.is_empty() {
                Err("Não ouvi nada nesta gravação.".to_owned())
            } else {
                let local = rules.apply(&raw);
                let cleaned = if let Some(plan) = plan
                    && let Some(input) = local.cleanup_input()
                {
                    clean_with_llm(
                        &openrouter,
                        &config.cleanup_model,
                        plan,
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
                m.total_ms = Some(ms(started.elapsed()));
                Ok(Reprocessed {
                    raw_text: (text != raw).then_some(raw),
                    text,
                    model: config.stt_model.clone(),
                    total_ms: ms(started.elapsed()),
                })
            }
        }
    };
    tracing::info!(
        model = %m.model,
        audio_ms = m.audio_ms,
        stt_ms = m.stt_ms,
        cleanup_ms = m.cleanup_ms,
        cost_usd = m.cost_usd,
        level = ?config.cleanup_level,
        ?style,
        ok = result.is_ok(),
        "ditado reprocessado"
    );
    let budget = config.monthly_budget_usd;
    // Espera-se pela escrita para a página de métricas, ao atualizar, já a ver.
    // Não conta como ditado novo: só o custo e a linha.
    let _ = tauri::async_runtime::spawn_blocking(move || {
        match state.metrics.record_reprocess(&m, budget) {
            Ok(Some(alert)) => state.notify(&alert.message()),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "não foi possível guardar as métricas"),
        }
    })
    .await;
    result
}

/// A limpeza com IA é um extra: se falhar, demorar mais de 6 s ou estragar o
/// texto (mexer nas linhas ou nas expansões), devolve `None` e cola-se o texto
/// sem limpeza.
async fn clean_with_llm(
    openrouter: &OpenRouter,
    model: &str,
    plan: cleanup::Plan,
    rules: &TextRules,
    local: &Processed,
    input: &str,
    m: &mut DictationMetrics,
) -> Option<String> {
    match cleanup::clean(openrouter, model, plan, input, rules.vocabulary()).await {
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

/// O que aconteceu na thread de colagem.
enum Pasting {
    Pasted(Duration),
    /// A janela mudou: ficou só no clipboard.
    Copied,
}

async fn paste_text(
    state: &AppState,
    text: String,
    released_at: Instant,
    restore_clipboard: bool,
    target: Option<focus::Window>,
    before: paste::Before,
    m: &mut DictationMetrics,
) -> Outcome {
    // `move` passa a posse do `text` para a thread de bloqueio. A janela é
    // verificada lá dentro, o mais perto possível do Ctrl+V.
    let job = move || paste_or_copy(&text, restore_clipboard, target, before);
    match tauri::async_runtime::spawn_blocking(job).await {
        Ok(Ok(Pasting::Copied)) => {
            m.total_ms = Some(ms(released_at.elapsed()));
            // Não se repõe o clipboard: o texto tem de lá ficar para o colares.
            Outcome::Copied
        }
        Ok(Ok(Pasting::Pasted(took))) => {
            m.paste_ms = Some(ms(took));
            m.total_ms = Some(ms(released_at.elapsed()));
            Outcome::Pasted
        }
        // O texto já estava no clipboard quando o Ctrl+V falhou: para o
        // utilizador é igual a "copiado", e a notificação explica porquê.
        Ok(Err(e @ paste::PasteError::Input(_))) => {
            tracing::error!(error = %e, "colagem falhou; o texto ficou no clipboard");
            state.notify(&e.to_string());
            m.error = Some(e.to_string());
            Outcome::Copied
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "colagem falhou");
            state.notify(&e.to_string());
            m.error = Some(e.to_string());
            Outcome::failed(FailReason::Generic)
        }
        Err(e) => {
            tracing::error!(error = %e, "tarefa de colagem falhou");
            m.error = Some(e.to_string());
            Outcome::failed(FailReason::Generic)
        }
    }
}

/// Bloqueante. Cola se a janela em primeiro plano for `target`; senão deixa o
/// texto só no clipboard.
fn paste_or_copy(
    text: &str,
    restore_clipboard: bool,
    target: Option<focus::Window>,
    before: paste::Before,
) -> Result<Pasting, paste::PasteError> {
    let now = focus::foreground();
    if focus::should_paste(target, now) {
        let (took, restore) = paste::paste(text, before)?;
        // A reposição do clipboard é agendada aqui dentro, numa thread própria:
        // uma vez colado, nada (nem um cancelamento) a pode impedir de correr.
        if restore_clipboard {
            let spawned = std::thread::Builder::new()
                .name("ditok-clipboard".into())
                .spawn(move || {
                    std::thread::sleep(paste::RESTORE_AFTER);
                    restore.run();
                });
            if let Err(e) = spawned {
                tracing::warn!(error = %e, "não foi possível agendar a reposição do clipboard");
            }
        }
        // Senão, o `restore` é largado aqui sem correr e o ditado fica no clipboard.
        Ok(Pasting::Pasted(took))
    } else {
        tracing::info!(
            ?target,
            ?now,
            "a janela em foco não é a de destino; texto só copiado"
        );
        paste::copy(text).map(|()| Pasting::Copied)
    }
}

/// "Colar o último ditado" (menu do tray ou da barra). `target`: a janela onde
/// estavas antes de abrir o menu; volto a pô-la à frente antes de colar.
pub async fn paste_again(
    state: Arc<AppState>,
    text: String,
    target: Option<focus::Window>,
) -> Outcome {
    let restore_clipboard = !state.config().keep_in_clipboard;
    let job = move || {
        if let Some(w) = target
            && !focus::activate(w)
        {
            tracing::info!("não consegui voltar à janela de destino");
        }
        paste_or_copy(&text, restore_clipboard, target, paste::Before::ReadNow)
    };
    match tauri::async_runtime::spawn_blocking(job).await {
        Ok(Ok(Pasting::Pasted(_))) => Outcome::Pasted,
        Ok(Ok(Pasting::Copied)) => Outcome::Copied,
        Ok(Err(e @ paste::PasteError::Input(_))) => {
            tracing::error!(error = %e, "colagem falhou; o texto ficou no clipboard");
            state.notify(&e.to_string());
            Outcome::Copied
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "colagem falhou");
            state.notify(&e.to_string());
            Outcome::failed(FailReason::Generic)
        }
        Err(e) => {
            tracing::error!(error = %e, "tarefa de colagem falhou");
            Outcome::failed(FailReason::Generic)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_and_cancel_exclude_each_other() {
        // Colar primeiro: o cancelamento chega tarde e é recusado.
        let c = Commit::default();
        assert!(c.try_commit());
        assert!(!c.try_cancel());
        assert!(c.try_commit(), "comprometer duas vezes não faz mal");

        // Cancelar primeiro: a colagem já não acontece.
        let c = Commit::default();
        assert!(c.try_cancel());
        assert!(!c.try_commit());
        assert!(!c.try_cancel(), "só se cancela uma vez");
    }

    #[test]
    fn clones_share_the_same_decision() {
        let c = Commit::default();
        let other = c.clone();
        assert!(other.try_cancel());
        assert!(!c.try_commit());
    }
}
