//! Transcrever um ficheiro (a gravação de uma aula, um vídeo) arrastado para a
//! janela. Um trabalho de cada vez, que continua mesmo que a janela feche.
//!
//! Há dois caminhos (`Route`):
//!
//! - **O ficheiro inteiro** (`transcribe_whole`): um só pedido à xAI, que aceita mp3, wav,
//!   ogg, opus, flac, aac, mp4, m4a e mkv até 500 MB. Não se descodifica nada aqui. É o
//!   caminho de um ficheiro que pesa menos do que o áudio que o outro enviaria (um mp3,
//!   um m4a) e o único de um formato que o descodificador local não lê (opus). Se falhar,
//!   ou se a xAI não processar o ficheiro todo, cai-se no outro.
//! - **Por pedaços** (`transcribe_chunks`), para o resto (vídeos, WAV de estúdio). Três partes
//!   trabalham ao mesmo tempo:
//!   1. uma thread de bloqueio lê o ficheiro e corta-o em pedaços (`audio::decode`);
//!   2. um canal com espaço para 2 pedaços liga-a à tarefa principal: se os pedidos
//!      atrasarem, a leitura espera ("backpressure") e a memória não cresce;
//!   3. até 3 pedidos à xAI em paralelo, cada um numa tarefa de um `JoinSet`.
//!
//! Medido com 68 minutos de fala (0,10 US$/h nos dois): o mesmo tempo (30–40 s), o mesmo
//! custo e o mesmo WER (0,5 %) nos dois caminhos. O ficheiro inteiro poupa bytes (49 MB de
//! mp3 em vez de 131 MB de áudio descodificado) e CPU; o outro dá progresso a sério e só
//! perde o pedaço que falhar.
//!
//! A janela acompanha pelo evento `file:job` e pelo comando `file_status`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use serde::Serialize;
use tauri::Emitter;
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};

use crate::{
    audio::decode::{self, Chunk, DecodeError},
    dictation::pipeline::EVENT_DICTATION_RECORDED,
    metrics::{self, DictationMetrics},
    settings_window,
    state::AppState,
    xai::{
        ApiError, Xai,
        retry::is_fatal,
        stt::{self, BYTES_PER_SEC, MAX_FILE_BYTES, SttRequest, Transcription, batch_cost},
    },
};

pub const EVENT: &str = "file:job";
/// Pedidos ao mesmo tempo. Mais do que isto pouco acelera e arrisca o limite de
/// pedidos da xAI.
const PARALLEL: usize = 3;

/// Extensões propostas no diálogo de escolher ficheiro: as que o descodificador local
/// ou a xAI aceitam. O formato é detetado pelo conteúdo, por isso um ficheiro com outra
/// extensão também pode funcionar (por pedaços).
pub const EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "aac", "wav", "flac", "ogg", "oga", "opus", "mp4", "m4v", "mov", "mkv", "mka",
];

/// O ficheiro inteiro só se envia se pesar quase tanto ou menos do que o áudio que o
/// outro caminho enviaria (16 kHz, 16 bits: `BYTES_PER_SEC`). Os 10 % são os cabeçalhos
/// de um WAV que já está nesse formato.
const WHOLE_MAX_RATIO: f64 = 1.1;
/// Um parágrafo tem pelo menos tantas palavras (e acaba numa frase): ~35 s de fala.
const PARAGRAPH_WORDS: usize = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// Em que ponto vai o trabalho; decide o que a janela mostra.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    /// O ficheiro inteiro a subir (a barra mostra os bytes).
    Upload,
    /// Enviado: a xAI está a transcrever. Não diz quanto falta.
    Server,
    /// Por pedaços (a barra mostra o áudio já transcrito).
    #[default]
    Chunks,
}

/// O que a janela mostra. O mesmo struct vai no evento e no `file_status`.
#[derive(Debug, Clone, Serialize)]
pub struct JobView {
    pub name: String,
    pub status: Status,
    pub model: String,
    /// Duração do ficheiro, se o cabeçalho a disser.
    pub total_s: Option<f64>,
    /// Áudio já transcrito (ou saltado por ser silêncio).
    pub done_s: f64,
    pub cost_usd: f64,
    pub elapsed_s: f64,
    /// O texto no fim; se falhou ou foi parado, o que já estava transcrito.
    pub text: Option<String>,
    pub message: Option<String>,
    /// Cópia do texto na pasta de dados do Ditok.
    pub saved_to: Option<String>,
    /// Pedaços que falharam mesmo depois de repetir (ficam assinalados no texto).
    pub failed_chunks: usize,
    pub stage: Stage,
    /// Bytes já enviados e tamanho do ficheiro (só contam em `Stage::Upload`).
    pub sent_bytes: u64,
    pub total_bytes: u64,
}

impl JobView {
    fn apply(&mut self, p: Progress) {
        self.stage = p.stage;
        self.done_s = p.done.as_secs_f64();
        self.cost_usd = p.cost;
        self.failed_chunks = p.failed;
        self.sent_bytes = p.sent_bytes;
        if p.note.is_some() {
            self.message = p.note;
        }
    }
}

struct Job {
    view: JobView,
    path: PathBuf,
    started: Instant,
    cancel: watch::Sender<bool>,
}

/// O trabalho atual (ou o último), dentro do `AppState`.
#[derive(Default)]
pub struct FileJobs {
    current: Mutex<Option<Job>>,
}

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("já está a transcrever um ficheiro; espera que acabe ou pára-o")]
    Busy,
    #[error("não há nenhuma transcrição com texto")]
    NoText,
    #[error("não foi possível guardar: {0}")]
    Io(#[from] std::io::Error),
}

impl FileJobs {
    fn lock(&self) -> MutexGuard<'_, Option<Job>> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn view(&self) -> Option<JobView> {
        self.lock().as_ref().map(|j| j.view.clone())
    }

    pub fn cancel(&self) {
        if let Some(job) = self.lock().as_ref() {
            let _ = job.cancel.send(true);
        }
    }

    /// Esquece um trabalho terminado (botão "Transcrever outro").
    pub fn clear(&self) {
        let mut current = self.lock();
        if current
            .as_ref()
            .is_some_and(|j| j.view.status != Status::Running)
        {
            *current = None;
        }
    }

    /// Grava o texto ao lado do ficheiro original, sem substituir nenhum ficheiro
    /// que lá esteja. Bloqueante.
    pub fn save_beside(&self) -> Result<PathBuf, JobError> {
        let (text, source) = {
            let current = self.lock();
            let job = current.as_ref().ok_or(JobError::NoText)?;
            (
                job.view.text.clone().ok_or(JobError::NoText)?,
                job.path.clone(),
            )
        };
        let dir = source.parent().unwrap_or(Path::new("."));
        let target = unique_path(dir, &stem(&source));
        std::fs::write(&target, text)?;
        Ok(target)
    }

    /// Grava um resumo (Markdown) ao lado do ficheiro original, como
    /// `<nome> resumo.md`, sem substituir nenhum ficheiro que lá esteja. Bloqueante.
    pub fn save_summary_beside(&self, summary: &str) -> Result<PathBuf, JobError> {
        let source = {
            let current = self.lock();
            current.as_ref().ok_or(JobError::NoText)?.path.clone()
        };
        let dir = source.parent().unwrap_or(Path::new("."));
        let target = unique_path_ext(dir, &format!("{} resumo", stem(&source)), "md");
        std::fs::write(&target, summary)?;
        Ok(target)
    }

    /// Muda a vista e avisa a janela (se estiver aberta).
    fn update(&self, app: &tauri::AppHandle, f: impl FnOnce(&mut JobView)) {
        let view = {
            let mut current = self.lock();
            let Some(job) = current.as_mut() else { return };
            f(&mut job.view);
            job.view.elapsed_s = job.started.elapsed().as_secs_f64();
            job.view.clone()
        };
        // Fora do lock: emitir pode demorar e não precisa dele.
        let _ = app.emit_to(settings_window::LABEL, EVENT, &view);
    }
}

/// O que se sabe de um ficheiro antes de o transcrever.
#[derive(Debug, Clone, Copy)]
pub struct Inspection {
    pub size: u64,
    /// `None` quando o cabeçalho não a diz (alguns mp3) ou o ficheiro não se lê aqui.
    pub duration: Option<Duration>,
    pub route: Route,
}

/// Como se vai transcrever um ficheiro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// O ficheiro inteiro, num só pedido. Com `can_fall_back`, se falhar tenta-se por pedaços.
    Whole { can_fall_back: bool },
    /// Descodificado aqui e enviado aos pedaços.
    Chunks,
}

/// Lê o cabeçalho e decide o caminho. Bloqueante (abre o ficheiro).
pub fn inspect(path: &Path) -> Result<Inspection, DecodeError> {
    let size = std::fs::metadata(path)?.len();
    match decode::probe(path) {
        Ok(info) => Ok(Inspection {
            size,
            duration: info.duration,
            route: choose_route(size, info.duration),
        }),
        // O descodificador local não o lê (opus, AAC-HE, …), mas a xAI talvez leia: tenta-se
        // se a extensão for de áudio ou vídeo e couber num pedido. Sem reserva. (Um fim de
        // ficheiro a meio do cabeçalho conta como ilegível; um ficheiro que não abre, não.)
        Err(e)
            if is_unreadable_here(&e)
                && has_known_extension(path)
                && (1..=MAX_FILE_BYTES).contains(&size) =>
        {
            Ok(Inspection {
                size,
                duration: None,
                route: Route::Whole {
                    can_fall_back: false,
                },
            })
        }
        Err(e) => Err(e),
    }
}

/// O descodificador local não conseguiu ler o ficheiro, mas o ficheiro abriu: o formato ou o
/// codec não são os que ele sabe, ou o cabeçalho está cortado. Um erro de disco não conta.
fn is_unreadable_here(e: &DecodeError) -> bool {
    match e {
        DecodeError::Unsupported | DecodeError::NoAudio | DecodeError::Corrupt(_) => true,
        DecodeError::Io(io) => io.kind() == std::io::ErrorKind::UnexpectedEof,
        DecodeError::Resample(_) => false,
    }
}

fn has_known_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Inteiro quando o ficheiro cabe num pedido e pesa menos do que o áudio que se enviaria
/// por pedaços; senão por pedaços. Sem duração não se sabe: o caminho de sempre.
fn choose_route(size: u64, duration: Option<Duration>) -> Route {
    let lighter =
        duration.is_some_and(|d| size as f64 <= d.as_secs_f64() * BYTES_PER_SEC * WHOLE_MAX_RATIO);
    if lighter && size <= MAX_FILE_BYTES {
        Route::Whole {
            can_fall_back: true,
        }
    } else {
        Route::Chunks
    }
}

/// Começa a transcrever `path`, se não houver outro ficheiro a meio.
pub fn start(
    state: &Arc<AppState>,
    xai: Xai,
    path: PathBuf,
    found: Inspection,
) -> Result<JobView, JobError> {
    let (cancel, cancel_rx) = watch::channel(false);
    let view = JobView {
        name: file_name(&path),
        status: Status::Running,
        model: stt::MODEL.to_owned(),
        total_s: found.duration.map(|d| d.as_secs_f64()),
        done_s: 0.0,
        cost_usd: 0.0,
        elapsed_s: 0.0,
        text: None,
        message: None,
        saved_to: None,
        failed_chunks: 0,
        stage: match found.route {
            Route::Whole { .. } => Stage::Upload,
            Route::Chunks => Stage::Chunks,
        },
        sent_bytes: 0,
        total_bytes: found.size,
    };
    {
        let mut current = state.files.lock();
        if current
            .as_ref()
            .is_some_and(|j| j.view.status == Status::Running)
        {
            return Err(JobError::Busy);
        }
        *current = Some(Job {
            view: view.clone(),
            path: path.clone(),
            started: Instant::now(),
            cancel,
        });
    }
    tracing::info!(
        file = %view.name,
        total_s = view.total_s,
        bytes = found.size,
        route = ?found.route,
        model = %view.model,
        "a transcrever um ficheiro"
    );
    tauri::async_runtime::spawn(run(Arc::clone(state), xai, path, found, cancel_rx));
    Ok(view)
}

/// O trabalho do princípio ao fim: transcreve, monta o texto, guarda uma cópia,
/// regista o custo e avisa.
async fn run(
    state: Arc<AppState>,
    xai: Xai,
    path: PathBuf,
    found: Inspection,
    cancel: watch::Receiver<bool>,
) {
    let config = state.config();
    let progress_state = Arc::clone(&state);
    let outcome = transcribe_file(
        xai,
        Input {
            path: path.clone(),
            found,
            language: config.language.clone(),
            keyterms: stt::keyterms(&config.dictionary).into(),
        },
        cancel,
        move |p| {
            progress_state
                .files
                .update(&progress_state.app, |v| v.apply(p));
        },
    )
    .await;

    let text = state
        .text_rules()
        .apply_dictionary(&assemble(&outcome.parts));
    let text = (!text.is_empty()).then_some(text);
    let (status, message) = match &outcome.end {
        End::Cancelled => (Status::Cancelled, None),
        End::Failed(e) => (Status::Failed, Some(e.clone())),
        End::Finished if text.is_none() => (
            Status::Done,
            Some("Não encontrei fala nenhuma no ficheiro.".to_owned()),
        ),
        End::Finished => (
            Status::Done,
            (outcome.failed > 0).then(|| {
                format!(
                    "{} trecho(s) não foram transcritos e estão assinalados no texto.",
                    outcome.failed
                )
            }),
        ),
    };

    // Uma cópia na pasta de dados: uma aula transcrita custou dinheiro e não se
    // pode perder por fechares a app.
    let saved_to = match text.clone() {
        Some(t) => {
            let (dir, source) = (state.data_dir.join("transcricoes"), path.clone());
            tauri::async_runtime::spawn_blocking(move || save_copy(&dir, &source, &t))
                .await
                .ok()
                .and_then(|r| {
                    r.inspect_err(
                        |e| tracing::warn!(error = %e, "não foi possível guardar a cópia"),
                    )
                    .ok()
                })
                .map(|p| p.display().to_string())
        }
        None => None,
    };

    let name = file_name(&path);
    let minutes = outcome.sent.as_secs_f64() / 60.0;
    tracing::info!(
        file = %name,
        ?status,
        audio_s = outcome.sent.as_secs(),
        cost_usd = outcome.cost,
        failed = outcome.failed,
        chars = text.as_ref().map_or(0, |t| t.chars().count()),
        "ficheiro transcrito"
    );
    let chars = text.as_ref().map_or(0, |t| t.chars().count());
    state.files.update(&state.app, |v| {
        v.status = status;
        v.text = text;
        v.message = message.clone();
        v.saved_to = saved_to;
        if status == Status::Done {
            // A duração real, que pode diferir um pouco da do cabeçalho.
            v.total_s = Some(v.done_s);
        }
    });
    match status {
        Status::Done => state.notify(&format!(
            "Transcrição pronta: {name} ({minutes:.0} min, ${:.2}).",
            outcome.cost
        )),
        Status::Failed => state.notify(&format!(
            "A transcrição de {name} falhou: {}",
            message.as_deref().unwrap_or("erro desconhecido")
        )),
        Status::Running | Status::Cancelled => {}
    }

    // O custo conta para o mês (e para o orçamento), como o de um ditado.
    let m = DictationMetrics {
        at: metrics::now(),
        model: stt::MODEL.to_owned(),
        transport: Some("multipart"),
        audio_ms: outcome.sent.as_millis() as u64,
        wav_bytes: 0,
        mic_start_ms: None,
        release_to_wav_ms: 0,
        stt_ms: None,
        cleanup_ms: None,
        paste_ms: None,
        total_ms: None,
        cost_usd: (outcome.cost > 0.0).then_some(outcome.cost),
        cleanup_cost_usd: None,
        chars,
        words: 0,
        app: None,
        error: (status == Status::Failed)
            .then(|| message.clone())
            .flatten(),
        file: true,
        command: false,
        summary: false,
    };
    tauri::async_runtime::spawn_blocking(move || {
        match state.metrics.record(&m, config.monthly_budget_usd) {
            Ok(Some(alert)) => state.notify(&alert.message()),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "não foi possível guardar as métricas"),
        }
        let _ = state.app.emit(EVENT_DICTATION_RECORDED, ());
    });
}

// ---------------------------------------------------------------- transcrever

/// Um pedaço do texto final, pela ordem do ficheiro.
#[derive(Debug, Clone, PartialEq)]
enum Part {
    Text(String),
    /// Só silêncio: não foi enviado.
    Silent,
    /// Falhou mesmo depois de repetir.
    Missing {
        start: Duration,
        end: Duration,
    },
}

#[derive(Debug, Clone, PartialEq)]
enum End {
    Finished,
    Cancelled,
    Failed(String),
}

/// Como correu um ficheiro, antes de o texto ser montado.
struct Outcome {
    parts: BTreeMap<usize, Part>,
    cost: f64,
    /// Áudio enviado (sem os pedaços de silêncio).
    sent: Duration,
    failed: usize,
    end: End,
}

impl Outcome {
    fn new(end: End) -> Self {
        Self {
            parts: BTreeMap::new(),
            cost: 0.0,
            sent: Duration::ZERO,
            failed: 0,
            end,
        }
    }
}

/// O que a janela fica a saber a cada passo.
#[derive(Debug, Clone, Default)]
struct Progress {
    stage: Stage,
    /// Áudio já transcrito (ou saltado por ser silêncio).
    done: Duration,
    cost: f64,
    failed: usize,
    sent_bytes: u64,
    /// Um aviso para mostrar enquanto trabalha (a mudança de caminho).
    note: Option<String>,
}

/// O que `transcribe_file` precisa de um ficheiro.
struct Input {
    path: PathBuf,
    found: Inspection,
    language: Option<String>,
    keyterms: Arc<[String]>,
}

fn chunk_progress(done: Duration, out: &Outcome) -> Progress {
    Progress {
        stage: Stage::Chunks,
        done,
        cost: out.cost,
        failed: out.failed,
        ..Progress::default()
    }
}

/// O que um pedido devolve à tarefa principal.
struct ChunkResult {
    index: usize,
    start: Duration,
    length: Duration,
    result: Result<Transcription, ApiError>,
}

/// Transcreve o ficheiro pelo caminho que `found.route` diz e, se o ficheiro inteiro não
/// resultar e houver reserva, pelo outro. Não sabe nada da janela nem do `AppState`: recebe
/// o que precisa e conta o progresso a `progress`.
async fn transcribe_file(
    xai: Xai,
    input: Input,
    mut cancel: watch::Receiver<bool>,
    mut progress: impl FnMut(Progress),
) -> Outcome {
    let mut wasted = 0.0;
    if let Route::Whole { can_fall_back } = input.found.route {
        match transcribe_whole(&xai, &input, can_fall_back, &mut cancel, &mut progress).await {
            Whole::Done(outcome) => return outcome,
            Whole::Retry { wasted: cost, why } => {
                tracing::warn!(%why, "o ficheiro inteiro não resultou; a tentar por pedaços");
                wasted = cost;
                progress(Progress {
                    cost,
                    note: Some(format!(
                        "O envio do ficheiro inteiro não resultou ({why}); a transcrever por pedaços."
                    )),
                    ..Progress::default()
                });
            }
        }
    }
    transcribe_chunks(xai, input, cancel, progress, wasted).await
}

/// O que o ficheiro inteiro deu.
enum Whole {
    Done(Outcome),
    /// Não resultou e há o outro caminho. `wasted` é o que a xAI já cobrou.
    Retry {
        wasted: f64,
        why: String,
    },
}

/// A xAI diz a duração do áudio que processou. Um ficheiro cujo cabeçalho diz mais
/// do que isso (com folga para arredondamentos) ficou a meio.
fn is_truncated(processed: f64, expected: Duration) -> bool {
    let expected = expected.as_secs_f64();
    processed < expected - (expected * 0.015).max(3.0)
}

/// Divide o texto em parágrafos: acaba-se um quando já tem `PARAGRAPH_WORDS` palavras e a
/// frase termina. (A xAI devolve o texto de seguida; sem isto, uma aula seria uma parede.)
fn paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for word in text.split_whitespace() {
        current.push(word);
        if current.len() >= PARAGRAPH_WORDS && word.ends_with(['.', '!', '?', '…']) {
            out.push(current.join(" "));
            current.clear();
        }
    }
    if !current.is_empty() {
        out.push(current.join(" "));
    }
    out
}

/// O ficheiro inteiro num só pedido. Conta os bytes enviados enquanto sobe e deixa-se
/// parar a qualquer momento (largar o pedido fecha a ligação).
async fn transcribe_whole(
    xai: &Xai,
    input: &Input,
    can_fall_back: bool,
    cancel: &mut watch::Receiver<bool>,
    progress: &mut impl FnMut(Progress),
) -> Whole {
    let req = SttRequest {
        language: input.language.as_deref(),
        keyterms: &input.keyterms,
    };
    let uploaded = Arc::new(AtomicU64::new(0));
    let request = xai.transcribe_path_retrying(&input.path, input.found.duration, &req, &uploaded);
    tokio::pin!(request);
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let size = input.found.size;
    let result = loop {
        tokio::select! {
            biased;
            _ = cancel.changed() => return Whole::Done(Outcome::new(End::Cancelled)),
            result = &mut request => break result,
            _ = tick.tick() => {
                let sent_bytes = uploaded.load(Ordering::Relaxed).min(size);
                progress(Progress {
                    stage: if sent_bytes >= size { Stage::Server } else { Stage::Upload },
                    sent_bytes,
                    ..Progress::default()
                });
            }
        }
    };

    match result {
        Ok(t) => {
            let expected = input.found.duration;
            if can_fall_back
                && let (Some(got), Some(want)) = (t.seconds, expected)
                && is_truncated(got, want)
            {
                return Whole::Retry {
                    wasted: batch_cost(got),
                    why: format!(
                        "a xAI só processou {} de {}",
                        clock(Duration::from_secs_f64(got)),
                        clock(want)
                    ),
                };
            }
            let audio = t
                .seconds
                .map(Duration::from_secs_f64)
                .or(expected)
                .unwrap_or_default();
            let cost = batch_cost(audio.as_secs_f64());
            let mut out = Outcome::new(End::Finished);
            out.cost = cost;
            out.sent = audio;
            out.parts = paragraphs(&t.text)
                .into_iter()
                .enumerate()
                .map(|(i, p)| (i, Part::Text(p)))
                .collect();
            progress(Progress {
                stage: Stage::Server,
                done: audio,
                cost,
                sent_bytes: size,
                ..Progress::default()
            });
            Whole::Done(out)
        }
        Err(e) if is_fatal(&e) || !can_fall_back => {
            Whole::Done(Outcome::new(End::Failed(e.to_string())))
        }
        Err(e) => Whole::Retry {
            wasted: 0.0,
            why: e.to_string(),
        },
    }
}

/// Lê, corta e transcreve o ficheiro aos pedaços. `base_cost` é o que já se gastou antes
/// (um ficheiro inteiro que a xAI processou a meio).
async fn transcribe_chunks(
    xai: Xai,
    input: Input,
    mut cancel: watch::Receiver<bool>,
    mut progress: impl FnMut(Progress),
    base_cost: f64,
) -> Outcome {
    let Input {
        path,
        language,
        keyterms,
        ..
    } = input;
    let (tx, mut rx) = mpsc::channel::<Chunk>(2);
    // A leitura pára quando o ficheiro acaba ou quando o envio falha, o que
    // acontece quando o `rx` é largado (parado ou erro).
    let reading_task = tauri::async_runtime::spawn_blocking(move || {
        decode::decode(&path, |chunk| tx.blocking_send(chunk).is_ok())
    });

    let mut tasks = JoinSet::new();
    let mut out = Outcome::new(End::Finished);
    out.cost = base_cost;
    let mut done = Duration::ZERO;
    let mut reading = true;

    loop {
        if !reading && tasks.is_empty() {
            break;
        }
        tokio::select! {
            // `biased`: primeiro o cancelamento, depois os pedidos que acabaram,
            // e só depois ler mais pedaços.
            biased;
            _ = cancel.changed() => {
                out.end = End::Cancelled;
                break;
            }
            Some(joined) = tasks.join_next() => {
                let Ok(r): Result<ChunkResult, _> = joined else { continue };
                match r.result {
                    Ok(t) => {
                        out.cost += t.usage.cost.unwrap_or(0.0);
                        out.parts.insert(r.index, Part::Text(t.text));
                    }
                    Err(e) if is_fatal(&e) => {
                        out.end = End::Failed(e.to_string());
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, index = r.index, "pedaço do ficheiro falhou");
                        out.failed += 1;
                        out.parts.insert(r.index, Part::Missing { start: r.start, end: r.start + r.length });
                    }
                }
                done += r.length;
                progress(chunk_progress(done, &out));
            }
            // Só lê mais um pedaço quando há lugar para outro pedido.
            next = rx.recv(), if reading && tasks.len() < PARALLEL => match next {
                None => reading = false,
                Some(chunk) => match chunk.wav {
                    None => {
                        out.parts.insert(chunk.index, Part::Silent);
                        done += chunk.length;
                        progress(chunk_progress(done, &out));
                    }
                    Some(wav) => {
                        out.sent += chunk.length;
                        let (xai, language, keyterms) =
                            (xai.clone(), language.clone(), Arc::clone(&keyterms));
                        let (index, start, length) = (chunk.index, chunk.start, chunk.length);
                        tasks.spawn(async move {
                            let req = SttRequest { language: language.as_deref(), keyterms: &keyterms };
                            let result = xai.transcribe_retrying(wav, &req).await;
                            ChunkResult { index, start, length, result }
                        });
                    }
                },
            },
        }
    }

    tasks.abort_all();
    drop(rx);
    match reading_task.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) if out.end == End::Finished => out.end = End::Failed(e.to_string()),
        Ok(Err(_)) => {}
        Err(e) => out.end = End::Failed(e.to_string()),
    }
    out
}

// ---------------------------------------------------------------- texto e ficheiros

/// Junta os pedaços pela ordem do ficheiro. Os cortes foram feitos em pausas: se a
/// frase anterior acabou, começa um parágrafo novo; senão, continua na mesma linha.
fn assemble(parts: &BTreeMap<usize, Part>) -> String {
    let mut out = String::new();
    let mut expected = 0;
    for (&index, part) in parts {
        // Pedaços que nunca chegaram (parado a meio): fica assinalado.
        if index > expected {
            join(&mut out, "[…]");
        }
        expected = index + 1;
        match part {
            Part::Text(t) => join(&mut out, t.trim()),
            Part::Missing { start, end } => join(
                &mut out,
                &format!(
                    "[não foi possível transcrever {}–{}]",
                    clock(*start),
                    clock(*end)
                ),
            ),
            Part::Silent => {}
        }
    }
    out
}

fn join(out: &mut String, piece: &str) {
    // Os modelos às vezes deixam espaços duplos entre frases.
    let piece = piece.split_whitespace().collect::<Vec<_>>().join(" ");
    if piece.is_empty() {
        return;
    }
    let paragraph = out.is_empty() || out.ends_with(['.', '!', '?', '…', ']']);
    if !out.is_empty() {
        out.push_str(if paragraph { "\n\n" } else { " " });
    }
    let mut chars = piece.chars();
    match chars.next() {
        // Um parágrafo novo começa com maiúscula, mesmo que o pedaço não.
        Some(first) if paragraph => {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
        _ => out.push_str(&piece),
    }
}

/// "4:05" ou "1:02:30".
fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "transcricao".to_owned())
}

/// `dir/stem.txt`, ou `dir/stem (2).txt`, … se já existir.
fn unique_path(dir: &Path, stem: &str) -> PathBuf {
    unique_path_ext(dir, stem, "txt")
}

/// O mesmo com outra extensão (o resumo vai para `.md`).
fn unique_path_ext(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}).{ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

fn save_copy(dir: &Path, source: &Path, text: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let date = metrics::now()
        .format(time::macros::format_description!(
            "[year]-[month]-[day] [hour]h[minute]"
        ))
        .unwrap_or_default();
    let target = unique_path(dir, &format!("{} {date}", stem(source)));
    std::fs::write(&target, text)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn parts_are_joined_in_order_with_paragraphs_at_sentence_ends() {
        let parts = BTreeMap::from([
            (2, Part::Text("e depois o Pedro.".into())),
            (0, Part::Text(" Bom dia a todos. ".into())),
            (1, Part::Text("Hoje vamos falar do Rust".into())),
            (3, Part::Silent),
            (
                4,
                Part::Missing {
                    start: secs(600),
                    end: secs(750),
                },
            ),
            (5, Part::Text("Até para a semana.".into())),
        ]);
        assert_eq!(
            assemble(&parts),
            "Bom dia a todos.\n\nHoje vamos falar do Rust e depois o Pedro.\n\n\
             [não foi possível transcrever 10:00–12:30]\n\nAté para a semana."
        );
    }

    #[test]
    fn spacing_is_tidied_and_paragraphs_start_with_a_capital() {
        let parts = BTreeMap::from([
            (0, Part::Text("Good morning.  Today   we start.".into())),
            (1, Part::Text("lifetimes are next.".into())),
        ]);
        assert_eq!(
            assemble(&parts),
            "Good morning. Today we start.\n\nLifetimes are next."
        );
    }

    #[test]
    fn chunks_that_never_arrived_are_marked() {
        let parts = BTreeMap::from([(1, Part::Text("b".into())), (3, Part::Text("d".into()))]);
        assert_eq!(assemble(&parts), "[…]\n\nB […]\n\nD");
        assert_eq!(assemble(&BTreeMap::new()), "");
    }

    #[test]
    fn clock_formats_minutes_and_hours() {
        assert_eq!(clock(secs(65)), "1:05");
        assert_eq!(clock(secs(3750)), "1:02:30");
    }

    #[test]
    fn saving_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("ditok-files-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = unique_path(&dir, "aula");
        std::fs::write(&a, "1").unwrap();
        let b = unique_path(&dir, "aula");
        assert_eq!(b.file_name().unwrap(), "aula (2).txt");
        let _ = std::fs::remove_dir_all(dir);
    }

    fn minutes(m: u64) -> Option<Duration> {
        Some(Duration::from_secs(m * 60))
    }

    #[test]
    fn a_light_file_goes_whole_and_a_heavy_one_in_chunks() {
        let whole = Route::Whole {
            can_fall_back: true,
        };
        // Um mp3 de 1 h a 128 kbps (58 MB) pesa menos do que 1 h a 16 kHz (115 MB).
        assert_eq!(choose_route(58_000_000, minutes(60)), whole);
        // Um WAV de 16 kHz mono já está nesse formato: segue inteiro.
        assert_eq!(choose_route(115_200_046, minutes(60)), whole);
        // Um WAV de 44,1 kHz estéreo (635 MB/h) e um vídeo de 300 MB/h pesam mais.
        assert_eq!(choose_route(320_000_000, minutes(60)), Route::Chunks);
        // Cabe no pedido, mas o outro caminho envia menos: um vídeo curto e pesado.
        assert_eq!(choose_route(150_000_000, minutes(30)), Route::Chunks);
        // Mais de 500 MB nunca vai inteiro, mesmo que seja leve para a duração dele.
        assert_eq!(choose_route(600_000_000, minutes(600)), Route::Chunks);
        // Sem duração não se sabe quanto pesa o outro caminho: o de sempre.
        assert_eq!(choose_route(10_000_000, None), Route::Chunks);
    }

    #[test]
    fn a_cut_short_answer_is_told_from_rounding() {
        let hour = Duration::from_secs(3600);
        assert!(!is_truncated(3600.0, hour));
        // O cabeçalho de um mp3 pode errar uns segundos.
        assert!(!is_truncated(3590.0, hour));
        assert!(!is_truncated(3550.0, hour), "1,5 % de folga");
        assert!(is_truncated(3500.0, hour));
        assert!(is_truncated(2000.0, hour));
        // Num áudio curto a folga mínima são 3 s.
        let short = Duration::from_secs(60);
        assert!(!is_truncated(57.5, short));
        assert!(is_truncated(50.0, short));
    }

    #[test]
    fn text_is_split_into_paragraphs_at_sentence_ends() {
        let sentence = |n: usize| format!("{} fim.", vec!["palavra"; n - 1].join(" "));
        // 40 + 40 + 40 palavras: o parágrafo fecha à 3.ª frase (120 ≥ 90), não antes.
        let text = [sentence(40), sentence(40), sentence(40), sentence(10)].join(" ");
        let found = paragraphs(&text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].split_whitespace().count(), 120);
        assert_eq!(found[1].split_whitespace().count(), 10);

        // Atingir o número de palavras a meio de uma frase não corta a frase.
        let long_sentence = format!("{} fim", vec!["a"; 200].join(" "));
        assert_eq!(
            paragraphs(&long_sentence),
            std::slice::from_ref(&long_sentence)
        );

        assert!(paragraphs("").is_empty());
        assert_eq!(paragraphs("  Olá,   mundo.  "), ["Olá, mundo."]);
    }

    #[test]
    fn paragraphs_survive_assembly() {
        let sentence = |n: usize| format!("{} fim.", vec!["palavra"; n - 1].join(" "));
        let text = [sentence(95), sentence(95)].join(" ");
        let parts: BTreeMap<usize, Part> = paragraphs(&text)
            .into_iter()
            .enumerate()
            .map(|(i, p)| (i, Part::Text(p)))
            .collect();
        let assembled = assemble(&parts);
        assert_eq!(assembled.matches("\n\n").count(), 1);
        assert_eq!(assembled.split_whitespace().count(), 190);
    }

    #[test]
    fn inspecting_sends_what_only_the_xai_can_read_and_refuses_the_rest() {
        let dir = std::env::temp_dir().join(format!("ditok-inspect-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };

        // Uma extensão de áudio que o descodificador local não lê: inteiro, sem reserva.
        let opus = inspect(&write("nota de voz.OPUS", b"OggS fingido")).unwrap();
        assert_eq!(opus.size, 12);
        assert_eq!(opus.duration, None);
        assert_eq!(
            opus.route,
            Route::Whole {
                can_fall_back: false
            }
        );

        // Nem uma extensão de áudio nem legível: erro, como sempre.
        let text = inspect(&write("notas.txt", b"isto nao e audio"));
        assert!(matches!(text, Err(DecodeError::Unsupported)), "{text:?}");
        // Vazio não se manda.
        let empty = inspect(&write("vazio.mp3", b""));
        assert!(empty.is_err());
        // Um ficheiro que não existe é um erro de leitura, não um formato.
        assert!(matches!(
            inspect(&dir.join("não-existe.mp3")),
            Err(DecodeError::Io(_))
        ));

        // Um WAV legível (6 s de silêncio a 16 kHz): inteiro, com reserva, duração conhecida.
        let wav = crate::audio::wav::encode_pcm16_mono(&vec![0.0; 96_000], 16_000);
        let found = inspect(&write("aula.wav", &wav)).unwrap();
        assert_eq!(found.size, wav.len() as u64);
        assert!((found.duration.unwrap().as_secs_f64() - 6.0).abs() < 0.01);
        assert_eq!(
            found.route,
            Route::Whole {
                can_fall_back: true
            }
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Transcreve um ficheiro de verdade com a chave do cofre (custa uns cêntimos
    /// de cêntimo). Gera-se uma "aula" com a voz do Windows:
    /// `DITOK_FILE=aula.wav cargo test live_file -- --ignored --nocapture`
    /// `DITOK_ROUTE=chunks` obriga ao caminho por pedaços; `DITOK_LANG` muda o idioma
    /// (por omissão `en`); `DITOK_OUT=<ficheiro>` guarda o texto; `DITOK_CANCEL_MS=<n>`
    /// para o trabalho passados `n` ms (e espera-se `Cancelled`).
    #[tokio::test]
    #[ignore]
    async fn live_file() {
        let path = PathBuf::from(std::env::var("DITOK_FILE").expect("define DITOK_FILE"));
        let key = crate::secrets::get().unwrap().expect("sem chave no cofre");
        let xai = Xai::new(crate::xai::http_client().unwrap(), key);
        let mut found = inspect(&path).unwrap();
        if std::env::var("DITOK_ROUTE").is_ok_and(|r| r == "chunks") {
            found.route = Route::Chunks;
        }
        println!(
            "{} → {:?}, {} MB, {:?}",
            path.display(),
            found.duration,
            found.size / 1_000_000,
            found.route
        );
        let (cancel, rx) = watch::channel(false);
        let cancel = Arc::new(cancel);
        let cancel_after = std::env::var("DITOK_CANCEL_MS")
            .ok()
            .and_then(|ms| ms.parse::<u64>().ok());
        if let Some(ms) = cancel_after {
            let cancel = Arc::clone(&cancel);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                let _ = cancel.send(true);
            });
        }
        let started = Instant::now();
        let language = std::env::var("DITOK_LANG").unwrap_or_else(|_| "en".into());
        let input = Input {
            path,
            found,
            language: Some(language),
            keyterms: Arc::from([]),
        };
        let out = transcribe_file(xai, input, rx, |p| {
            println!(
                "  {:?}: {} MB enviados, {:?} transcritos, ${:.5}{}",
                p.stage,
                p.sent_bytes / 1_000_000,
                p.done,
                p.cost,
                p.note.map(|n| format!(" ({n})")).unwrap_or_default()
            )
        })
        .await;
        println!(
            "{:?} em {:?}, ${:.5}, {} falhados\n---\n{}",
            out.end,
            started.elapsed(),
            out.cost,
            out.failed,
            assemble(&out.parts)
        );
        if let Ok(out_path) = std::env::var("DITOK_OUT") {
            std::fs::write(out_path, assemble(&out.parts)).unwrap();
        }
        let expected = if cancel_after.is_some() {
            End::Cancelled
        } else {
            End::Finished
        };
        assert_eq!(out.end, expected);
        drop(cancel);
    }
}
