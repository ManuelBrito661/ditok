//! Uma sessão do notetaker, do "Começar" ao "Parar": abre a captura, manda cada
//! enunciado a transcrever (poucos de cada vez), guarda as linhas à medida que ficam
//! prontas e avisa a janela. Quando se pára, espera pelo que está em curso, fecha a
//! sessão e regista o custo.
//!
//! Quem manda é o `Notes` (a máquina de estados: Idle → Recording → Finishing → Idle,
//! numa só `Mutex`, para dois cliques seguidos não abrirem duas capturas). O trabalho
//! pesado corre numa tarefa (`pump`) que só fala com o `Notes` por `update`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use tauri::Emitter;
use tokio::{
    sync::{
        Semaphore,
        mpsc::{UnboundedReceiver, unbounded_channel},
    },
    task::JoinSet,
};

use super::{
    capture::{Capture, CaptureConfig, CaptureError, CaptureEvent, Track, Utterance},
    store::{Line, Meta, Mode, NotesStore, StoreError},
};
use crate::{
    audio::{resample::TARGET_RATE, wav},
    config::SttEngine,
    metrics::{self, DictationMetrics},
    settings_window,
    state::AppState,
    stt::{SttProvider, job},
};

pub const EVENT_STATE: &str = "notes:state";
pub const EVENT_LINE: &str = "notes:line";
pub const EVENT_LEVEL: &str = "notes:level";

/// Pedidos de transcrição ao mesmo tempo. O motor local atende um de cada vez de
/// qualquer maneira; na nuvem, mais do que isto só atrasa o resto da fila.
const PARALLEL: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    #[default]
    Idle,
    Recording,
    /// Já não grava; falta transcrever o que ficou na fila.
    Finishing,
}

#[derive(Debug, thiserror::Error)]
pub enum NotesError {
    #[error("já há uma gravação em curso")]
    Busy,
    #[error("não há nenhuma gravação em curso")]
    NotRecording,
    #[error("{0}")]
    Unsupported(String),
    #[error("aceita primeiro o aviso sobre gravar conversas")]
    ConsentRequired,
    #[error("falta a chave do OpenRouter (ou escolhe o motor local) para transcrever")]
    NoKey,
    #[error(
        "com o motor local, as Notas precisam da separação de oradores: ativa-a nas definições e descarrega os modelos de oradores"
    )]
    NeedsSpeakers,
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("não foi possível ler a chave: {0}")]
    Secret(#[from] crate::secrets::SecretError),
}

/// O que uma sessão em curso sabe de si própria.
struct Active {
    meta: Meta,
    started_ms: i64,
    capture: Option<Capture>,
    cost_usd: f64,
    audio_ms: u64,
    lines: u32,
    system_mute: bool,
    mic_device: Option<String>,
    system_device: Option<String>,
    lost: Vec<String>,
    /// O último erro que impede de transcrever (chave recusada, sem créditos…).
    error: Option<String>,
    /// O que se está a fazer no fim ("A identificar quem fala…").
    step: Option<&'static str>,
}

#[derive(Default)]
struct Inner {
    phase: Phase,
    active: Option<Active>,
}

#[derive(Default)]
pub struct Notes {
    inner: Mutex<Inner>,
}

/// O que a janela vê.
#[derive(Debug, Clone, Serialize)]
pub struct StateView {
    pub phase: Phase,
    pub id: Option<String>,
    pub title: Option<String>,
    pub mode: Option<Mode>,
    /// Milissegundos desde 1970: a janela conta o tempo a partir daqui.
    pub started_ms: Option<i64>,
    pub cost_usd: f64,
    pub lines: u32,
    pub system_mute: bool,
    pub mic_device: Option<String>,
    pub system_device: Option<String>,
    pub lost: Vec<String>,
    pub error: Option<String>,
    pub step: Option<&'static str>,
    /// Onde se resolve o "só silêncio" do áudio do sistema neste sistema operativo.
    pub system_help: &'static str,
}

impl Notes {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn phase(&self) -> Phase {
        self.lock().phase
    }

    pub fn recording(&self) -> bool {
        self.phase() == Phase::Recording
    }

    /// Idle → Recording. Falha se já houver sessão.
    fn begin(&self, active: Active) -> Result<(), NotesError> {
        let mut inner = self.lock();
        if inner.phase != Phase::Idle {
            return Err(NotesError::Busy);
        }
        inner.phase = Phase::Recording;
        inner.active = Some(active);
        Ok(())
    }

    /// Recording → Finishing, entregando a captura para a fechar fora do lock.
    fn request_stop(&self) -> Result<Option<Capture>, NotesError> {
        let mut inner = self.lock();
        if inner.phase != Phase::Recording {
            return Err(NotesError::NotRecording);
        }
        inner.phase = Phase::Finishing;
        Ok(inner.active.as_mut().and_then(|a| a.capture.take()))
    }

    /// Qualquer fase → Idle, devolvendo o que havia.
    fn end(&self) -> Option<Active> {
        let mut inner = self.lock();
        inner.phase = Phase::Idle;
        inner.active.take()
    }

    fn update(&self, f: impl FnOnce(&mut Active)) {
        if let Some(active) = self.lock().active.as_mut() {
            f(active);
        }
    }

    pub fn view(&self) -> StateView {
        let inner = self.lock();
        match &inner.active {
            None => StateView {
                phase: inner.phase,
                id: None,
                title: None,
                mode: None,
                started_ms: None,
                cost_usd: 0.0,
                lines: 0,
                system_mute: false,
                mic_device: None,
                system_device: None,
                lost: Vec::new(),
                error: None,
                step: None,
                system_help: super::platform::SYSTEM_AUDIO_HELP,
            },
            Some(a) => StateView {
                phase: inner.phase,
                id: Some(a.meta.id.clone()),
                title: Some(a.meta.title.clone()),
                mode: Some(a.meta.mode),
                started_ms: Some(a.started_ms),
                cost_usd: a.cost_usd,
                lines: a.lines,
                system_mute: a.system_mute,
                mic_device: a.mic_device.clone(),
                system_device: a.system_device.clone(),
                lost: a.lost.clone(),
                error: a.error.clone(),
                step: a.step,
                system_help: super::platform::SYSTEM_AUDIO_HELP,
            },
        }
    }
}

fn emit<S: Serialize + Clone>(state: &AppState, event: &str, payload: S) {
    let _ = state.app.emit_to(settings_window::LABEL, event, payload);
}

/// Conta à janela (e ao tray e à pílula) o estado atual.
fn publish(state: &AppState) {
    emit(state, EVENT_STATE, state.notes.view());
    state.publish();
}

fn default_title(mode: Mode, now: chrono::DateTime<chrono::Local>) -> String {
    let what = match mode {
        Mode::Meeting => "Reunião",
        Mode::SystemOnly => "Áudio do sistema",
        Mode::InPerson => "Conversa",
    };
    format!("{what} {}", now.format("%d/%m %H:%M"))
}

fn capture_config(
    mode: Mode,
    mic_device: Option<String>,
    output_device: Option<String>,
) -> CaptureConfig {
    CaptureConfig {
        mic: mode != Mode::SystemOnly,
        system: mode != Mode::InPerson,
        mic_device,
        output_device,
    }
}

/// Começa a gravar. `stt` já vem escolhido (nuvem ou local) pelo chamador.
pub async fn start(state: &Arc<AppState>, mode: Mode) -> Result<StateView, NotesError> {
    let config = state.config();
    if !config.notes_consent {
        return Err(NotesError::ConsentRequired);
    }
    // Com o motor local, a identificação de oradores é obrigatória: sem ela a reunião
    // ficaria só com "Eu" e "Outros", e o motor local não tem outra forma de as separar.
    if config.stt_engine == SttEngine::Local
        && !(config.notes_speakers
            && super::speakers::AVAILABLE
            && super::speakers::is_installed(&state.models_dir(), config.notes_speaker_model))
    {
        return Err(NotesError::NeedsSpeakers);
    }
    if state.notes.phase() != Phase::Idle {
        return Err(NotesError::Busy);
    }
    if mode != Mode::InPerson {
        super::platform::check_system_audio().map_err(NotesError::Unsupported)?;
    }
    let state_ = Arc::clone(state);
    let providers = tauri::async_runtime::spawn_blocking(move || state_.load_providers())
        .await
        .map_err(|_| NotesError::NoKey)??
        .ok_or(NotesError::NoKey)?;

    let now = chrono::Local::now();
    let store_dir = state.data_dir.clone();
    let meta = {
        let store = NotesStore::new(&store_dir);
        Meta {
            id: store.new_id(now),
            title: default_title(mode, now),
            started_at: now.to_rfc3339(),
            ended_at: None,
            mode,
            model: config.active_stt_model(),
            cost_usd: 0.0,
            audio_ms: 0,
            interrupted: false,
            finished: false,
            mic_name: "Eu".into(),
            system_name: "Outros".into(),
            speaker_names: Default::default(),
        }
    };
    state.notes.begin(Active {
        meta: meta.clone(),
        started_ms: now.timestamp_millis(),
        capture: None,
        cost_usd: 0.0,
        audio_ms: 0,
        lines: 0,
        system_mute: false,
        mic_device: None,
        system_device: None,
        lost: Vec::new(),
        error: None,
        step: None,
    })?;

    let (tx, rx) = unbounded_channel();
    let cfg = capture_config(
        mode,
        config.input_device.clone(),
        config.notes_output_device.clone(),
    );
    let created = tauri::async_runtime::spawn_blocking({
        let meta = meta.clone();
        move || -> Result<Capture, NotesError> {
            NotesStore::new(&store_dir).create(&meta)?;
            // `tx` entra aqui e fica só nas threads da captura: quando elas acabam,
            // o canal fecha e o `pump` sabe que não vem mais nada.
            Ok(Capture::start(&cfg, tx)?)
        }
    })
    .await;
    let capture = match created {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            state.notes.end();
            let _ = NotesStore::new(&state.data_dir).delete(&meta.id);
            return Err(e);
        }
        Err(_) => {
            state.notes.end();
            return Err(NotesError::NoKey);
        }
    };
    state.notes.update(|a| a.capture = Some(capture));

    tracing::info!(id = %meta.id, ?mode, model = %meta.model, "gravação de notas começada");
    let diar = diarized_track(state, mode);
    tauri::async_runtime::spawn(pump(
        Arc::clone(state),
        providers.stt,
        meta.clone(),
        diar,
        rx,
    ));
    publish(state);
    Ok(state.notes.view())
}

/// Pára de gravar. Devolve logo; a transcrição do que falta acaba em segundo plano
/// e a janela é avisada pelo evento de estado.
pub async fn stop(state: &Arc<AppState>) -> Result<StateView, NotesError> {
    let capture = state.notes.request_stop()?;
    publish(state);
    if let Some(capture) = capture {
        // Fechar as streams e esperar pelas threads bloqueia um pouco.
        let _ = tauri::async_runtime::spawn_blocking(move || capture.stop()).await;
    }
    Ok(state.notes.view())
}

/// Recebe os eventos da captura, transcreve os enunciados e fecha a sessão.
async fn pump(
    state: Arc<AppState>,
    stt: Arc<dyn SttProvider>,
    meta: Meta,
    diar: Option<Track>,
    mut rx: UnboundedReceiver<CaptureEvent>,
) {
    let config = state.config();
    let rules = state.text_rules();
    let model = meta.model.clone();
    let language = config.language.clone();
    // BETA: com o filtro ligado, cada enunciado passa pelo Silero antes do STT.
    let speech_dir =
        (config.notes_speech_gate && super::speech_gate::AVAILABLE).then(|| state.models_dir());
    let gate = Arc::new(Semaphore::new(PARALLEL));
    let mut tasks: JoinSet<()> = JoinSet::new();

    while let Some(event) = rx.recv().await {
        match event {
            CaptureEvent::Utterance(u) => {
                // O áudio da faixa a separar guarda-se (temporário) para depois do fim.
                if diar == Some(u.track) {
                    let (state, id, samples, start) = (
                        Arc::clone(&state),
                        meta.id.clone(),
                        u.samples.clone(),
                        u.start_ms,
                    );
                    tasks.spawn(async move {
                        let _ = tauri::async_runtime::spawn_blocking(move || {
                            NotesStore::new(&state.data_dir).write_pcm_at(&id, start, &samples)
                        })
                        .await;
                    });
                }
                let counted = Arc::clone(&state);
                let speech_dir = speech_dir.clone();
                let job = UtteranceJob {
                    state: Arc::clone(&state),
                    stt: Arc::clone(&stt),
                    rules: Arc::clone(&rules),
                    id: meta.id.clone(),
                    model: model.clone(),
                    language: language.clone(),
                };
                let gate = Arc::clone(&gate);
                tasks.spawn(async move {
                    // O filtro (BETA) corre antes do pedido: um enunciado sem fala não se
                    // transcreve nem se paga.
                    let u = match speech_dir {
                        None => u,
                        Some(dir) => {
                            let Ok((u, speech)) = tauri::async_runtime::spawn_blocking(move || {
                                let speech = super::speech_gate::has_speech(&dir, &u.samples);
                                (u, speech)
                            })
                            .await
                            else {
                                return;
                            };
                            if !speech {
                                tracing::debug!(
                                    start_ms = u.start_ms,
                                    "enunciado sem fala, não transcrito"
                                );
                                let audio_ms = u.end_ms.saturating_sub(u.start_ms);
                                counted.notes.update(|a| a.audio_ms += audio_ms);
                                return;
                            }
                            u
                        }
                    };
                    let wav = wav::encode_pcm16_mono(&u.samples, TARGET_RATE);
                    // O pedido espera a sua vez aqui, sem travar a receção de eventos.
                    let Ok(_permit) = gate.acquire().await else {
                        return;
                    };
                    job.run(u, wav).await;
                });
            }
            CaptureEvent::Level(track, level) => emit(
                &state,
                EVENT_LEVEL,
                serde_json::json!({ "track": track, "level": level }),
            ),
            CaptureEvent::Device(track, name) => {
                state.notes.update(|a| {
                    match track {
                        Track::Mic => a.mic_device = Some(name),
                        Track::System => a.system_device = Some(name),
                    }
                    a.lost.retain(|t| t != track.label());
                });
                publish(&state);
            }
            CaptureEvent::Lost(track, why) => {
                tracing::warn!(track = track.label(), %why, "faixa perdida");
                state
                    .notes
                    .update(|a| a.lost.push(track.label().to_owned()));
                publish(&state);
            }
            CaptureEvent::SystemMute => {
                state.notes.update(|a| a.system_mute = true);
                publish(&state);
            }
        }
    }
    // A captura acabou: falta o que já estava na fila.
    while tasks.join_next().await.is_some() {}
    finish(&state, meta, diar).await;
}

struct UtteranceJob {
    state: Arc<AppState>,
    stt: Arc<dyn SttProvider>,
    rules: Arc<crate::text::TextRules>,
    id: String,
    model: String,
    language: Option<String>,
}

impl UtteranceJob {
    async fn run(self, u: Utterance, wav: bytes::Bytes) {
        let audio_ms = u.end_ms.saturating_sub(u.start_ms);
        let result = job::transcribe(&*self.stt, wav, &self.model, self.language.as_deref()).await;
        let (text, failed, cost) = match result {
            Ok(t) => (
                self.rules.apply_dictionary(t.text.trim()),
                false,
                t.usage.cost.unwrap_or(0.0),
            ),
            Err(e) => {
                tracing::warn!(error = %e, start_ms = u.start_ms, "enunciado não transcrito");
                if job::is_fatal(&e) {
                    let message = e.to_string();
                    self.state.notes.update(|a| a.error = Some(message));
                    publish(&self.state);
                }
                (String::new(), true, 0.0)
            }
        };
        // Sem fala reconhecida (ruído, um "hm"): não é uma linha.
        if text.is_empty() && !failed {
            self.state.notes.update(|a| a.audio_ms += audio_ms);
            return;
        }
        let line = Line {
            track: u.track,
            start_ms: u.start_ms,
            end_ms: u.end_ms,
            text,
            failed,
            speaker: None,
        };
        let (state, id, to_save) = (Arc::clone(&self.state), self.id.clone(), line.clone());
        let saved = tauri::async_runtime::spawn_blocking(move || {
            NotesStore::new(&state.data_dir).append(&id, &to_save)
        })
        .await;
        if !matches!(saved, Ok(Ok(()))) {
            tracing::error!(?saved, "não foi possível guardar uma linha da transcrição");
        }
        self.state.notes.update(|a| {
            a.cost_usd += cost;
            a.audio_ms += audio_ms;
            a.lines += 1;
            a.error = None;
        });
        emit(
            &self.state,
            EVENT_LINE,
            serde_json::json!({ "id": self.id, "line": line, "cost_usd": self.state.notes.view().cost_usd }),
        );
    }
}

/// Fecha a sessão: meta final, métricas, aviso.
async fn finish(state: &Arc<AppState>, mut meta: Meta, diar: Option<Track>) {
    let speakers_error = match diar {
        Some(track) => identify_speakers(state, &meta.id, track).await.err(),
        None => None,
    };
    let Some(active) = state.notes.end() else {
        return;
    };
    meta.cost_usd = active.cost_usd;
    meta.audio_ms = active.audio_ms;
    meta.finished = true;
    meta.ended_at = Some(chrono::Local::now().to_rfc3339());
    let config = state.config();
    let m = DictationMetrics {
        at: metrics::now(),
        provider: if meta.model.starts_with("local/") {
            "local"
        } else {
            "openrouter"
        },
        model: meta.model.clone(),
        transport: None,
        audio_ms: meta.audio_ms,
        wav_bytes: 0,
        mic_start_ms: None,
        release_to_wav_ms: 0,
        stt_ms: None,
        cleanup_ms: None,
        paste_ms: None,
        total_ms: None,
        cost_usd: (meta.cost_usd > 0.0).then_some(meta.cost_usd),
        cleanup_cost_usd: None,
        chars: 0,
        words: 0,
        app: None,
        generation_id: None,
        error: None,
        file: true,
        command: false,
        summary: false,
    };
    let (state_, saved_meta) = (Arc::clone(state), meta.clone());
    let _ = tauri::async_runtime::spawn_blocking(move || {
        if let Err(e) = NotesStore::new(&state_.data_dir).save_meta(&saved_meta) {
            tracing::error!(error = %e, "não foi possível fechar a sessão");
        }
        match state_.metrics.record(&m, config.monthly_budget_usd) {
            Ok(Some(alert)) => state_.notify(&alert.message()),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "não foi possível guardar as métricas"),
        }
    })
    .await;
    tracing::info!(id = %meta.id, lines = active.lines, cost_usd = meta.cost_usd, "gravação de notas terminada");
    publish(state);
    let _ = state
        .app
        .emit(crate::dictation::pipeline::EVENT_DICTATION_RECORDED, ());
    state.notify(&format!("Notas guardadas: {}.", meta.title));
    if let Some(why) = speakers_error {
        state.notify(&format!("Não consegui separar os oradores: {why}"));
    }
}

/// Que faixa se separa em oradores nesta sessão: a dos outros (ou a do microfone, numa
/// conversa presencial). `None` se estiver desligado ou faltar o motor ou os modelos.
fn diarized_track(state: &AppState, mode: Mode) -> Option<Track> {
    let config = state.config();
    if !config.notes_speakers
        || !super::speakers::AVAILABLE
        || !super::speakers::is_installed(&state.models_dir(), config.notes_speaker_model)
    {
        return None;
    }
    Some(if mode == Mode::InPerson {
        Track::Mic
    } else {
        Track::System
    })
}

/// Depois da reunião: separa quem falou na `track` e grava o orador em cada linha.
/// O áudio temporário apaga-se sempre, com ou sem sucesso.
async fn identify_speakers(state: &Arc<AppState>, id: &str, track: Track) -> Result<(), String> {
    state
        .notes
        .update(|a| a.step = Some("A identificar quem fala…"));
    publish(state);
    let (state_, id_) = (Arc::clone(state), id.to_owned());
    let model = state.config().notes_speaker_model;
    let result = tauri::async_runtime::spawn_blocking(move || -> Result<usize, String> {
        let store = NotesStore::new(&state_.data_dir);
        let outcome = (|| {
            let samples = store.read_pcm(&id_).map_err(|e| e.to_string())?;
            if samples.is_empty() {
                return Ok(0);
            }
            let started = std::time::Instant::now();
            let segments = super::speakers::diarize(&state_.models_dir(), model, &samples)
                .map_err(|e| e.to_string())?;
            let mut lines = store.lines(&id_).map_err(|e| e.to_string())?;
            super::speakers::assign(&mut lines, track, &segments);
            store.set_lines(&id_, &lines).map_err(|e| e.to_string())?;
            let speakers = segments.iter().map(|s| s.speaker).max().unwrap_or(0) as usize;
            tracing::info!(
                id = %id_,
                speakers,
                seconds = samples.len() / 16_000,
                took_s = started.elapsed().as_secs_f32(),
                "oradores separados"
            );
            Ok(speakers)
        })();
        store.delete_pcm(&id_);
        outcome
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Err(why) = &result {
        tracing::warn!(%why, "não foi possível separar os oradores");
    }
    result.map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active() -> Active {
        Active {
            meta: Meta {
                id: "20261012-100000".into(),
                title: "t".into(),
                started_at: String::new(),
                ended_at: None,
                mode: Mode::Meeting,
                model: "m".into(),
                cost_usd: 0.0,
                audio_ms: 0,
                interrupted: false,
                finished: false,
                mic_name: "Eu".into(),
                system_name: "Outros".into(),
                speaker_names: Default::default(),
            },
            started_ms: 0,
            capture: None,
            cost_usd: 0.0,
            audio_ms: 0,
            lines: 0,
            system_mute: false,
            mic_device: None,
            system_device: None,
            lost: Vec::new(),
            error: None,
            step: None,
        }
    }

    #[test]
    fn session_state_transitions() {
        let notes = Notes::default();
        assert_eq!(notes.phase(), Phase::Idle);
        assert!(matches!(
            notes.request_stop(),
            Err(NotesError::NotRecording)
        ));

        notes.begin(active()).unwrap();
        assert_eq!(notes.phase(), Phase::Recording);
        assert!(notes.recording());
        assert!(matches!(notes.begin(active()), Err(NotesError::Busy)));

        notes.request_stop().unwrap();
        assert_eq!(notes.phase(), Phase::Finishing);
        assert!(!notes.recording());
        // Já está a fechar: um segundo "Parar" e um novo "Começar" são recusados.
        assert!(matches!(
            notes.request_stop(),
            Err(NotesError::NotRecording)
        ));
        assert!(matches!(notes.begin(active()), Err(NotesError::Busy)));

        assert!(notes.end().is_some());
        assert_eq!(notes.phase(), Phase::Idle);
        notes.begin(active()).unwrap();
    }

    #[test]
    fn view_reflects_updates() {
        let notes = Notes::default();
        assert_eq!(notes.view().phase, Phase::Idle);
        notes.begin(active()).unwrap();
        notes.update(|a| {
            a.cost_usd = 0.25;
            a.lines = 3;
            a.system_mute = true;
        });
        let v = notes.view();
        assert_eq!(v.lines, 3);
        assert!(v.system_mute);
        assert!((v.cost_usd - 0.25).abs() < 1e-9);
    }

    #[test]
    fn capture_config_follows_mode() {
        let c = capture_config(Mode::Meeting, Some("mic".into()), None);
        assert!(c.mic && c.system);
        let c = capture_config(Mode::SystemOnly, None, None);
        assert!(!c.mic && c.system);
        let c = capture_config(Mode::InPerson, None, None);
        assert!(c.mic && !c.system);
    }

    #[test]
    fn capture_config_passes_output_device() {
        let c = capture_config(Mode::Meeting, None, Some("Altifalantes".into()));
        assert_eq!(c.output_device.as_deref(), Some("Altifalantes"));
    }
}
