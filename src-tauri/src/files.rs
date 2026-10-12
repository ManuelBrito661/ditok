//! Transcrever um ficheiro (a gravação de uma aula, um vídeo) arrastado para a
//! janela. Um trabalho de cada vez, que continua mesmo que a janela feche.
//!
//! Três partes trabalham ao mesmo tempo:
//! 1. uma thread de bloqueio lê o ficheiro e corta-o em pedaços (`audio::decode`);
//! 2. um canal com espaço para 2 pedaços liga-a à tarefa principal: se os pedidos
//!    atrasarem, a leitura espera ("backpressure") e a memória não cresce;
//! 3. até 3 pedidos ao OpenRouter em paralelo, cada um numa tarefa de um `JoinSet`.
//!
//! A janela acompanha pelo evento `file:job` e pelo comando `file_status`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use serde::Serialize;
use tauri::Emitter;
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};

use crate::{
    audio::decode::{self, Chunk, FileInfo},
    dictation::pipeline::EVENT_DICTATION_RECORDED,
    metrics::{self, DictationMetrics},
    settings_window,
    state::AppState,
    stt::{SttError, SttProvider, Transcription, job},
};

pub const EVENT: &str = "file:job";
/// Pedidos ao mesmo tempo. Mais do que isto pouco acelera e arrisca o limite de
/// pedidos do OpenRouter.
const PARALLEL: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Running,
    Done,
    Failed,
    Cancelled,
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

/// Começa a transcrever `path`, se não houver outro ficheiro a meio.
pub fn start(
    state: &Arc<AppState>,
    stt: Arc<dyn SttProvider>,
    path: PathBuf,
    info: FileInfo,
) -> Result<JobView, JobError> {
    let config = state.config();
    let (cancel, cancel_rx) = watch::channel(false);
    let view = JobView {
        name: file_name(&path),
        status: Status::Running,
        model: config.active_stt_model(),
        total_s: info.duration.map(|d| d.as_secs_f64()),
        done_s: 0.0,
        cost_usd: 0.0,
        elapsed_s: 0.0,
        text: None,
        message: None,
        saved_to: None,
        failed_chunks: 0,
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
    tracing::info!(file = %view.name, total_s = view.total_s, model = %view.model, "a transcrever um ficheiro");
    tauri::async_runtime::spawn(run(Arc::clone(state), stt, path, cancel_rx));
    Ok(view)
}

/// O trabalho do princípio ao fim: transcreve, monta o texto, guarda uma cópia,
/// regista o custo e avisa.
async fn run(
    state: Arc<AppState>,
    stt: Arc<dyn SttProvider>,
    path: PathBuf,
    cancel: watch::Receiver<bool>,
) {
    let config = state.config();
    let progress_state = Arc::clone(&state);
    let outcome = transcribe_file(
        Arc::clone(&stt),
        path.clone(),
        config.active_stt_model(),
        config.language.clone(),
        cancel,
        move |p| {
            progress_state.files.update(&progress_state.app, |v| {
                v.done_s = p.done.as_secs_f64();
                v.cost_usd = p.cost;
                v.failed_chunks = p.failed;
            });
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
        provider: stt.name(),
        model: config.active_stt_model(),
        transport: None,
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
        generation_id: None,
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

#[derive(Debug, Clone, Copy)]
struct Progress {
    done: Duration,
    cost: f64,
    failed: usize,
}

/// O que um pedido devolve à tarefa principal.
struct ChunkResult {
    index: usize,
    start: Duration,
    length: Duration,
    result: Result<Transcription, SttError>,
}

/// Lê, corta e transcreve o ficheiro. Não sabe nada da janela nem do `AppState`:
/// recebe o que precisa e conta o progresso a `progress`.
async fn transcribe_file(
    stt: Arc<dyn SttProvider>,
    path: PathBuf,
    model: String,
    language: Option<String>,
    mut cancel: watch::Receiver<bool>,
    mut progress: impl FnMut(Progress),
) -> Outcome {
    let (tx, mut rx) = mpsc::channel::<Chunk>(2);
    // A leitura pára quando o ficheiro acaba ou quando o envio falha, o que
    // acontece quando o `rx` é largado (parado ou erro).
    let reading_task = tauri::async_runtime::spawn_blocking(move || {
        decode::decode(&path, |chunk| tx.blocking_send(chunk).is_ok())
    });

    let mut tasks = JoinSet::new();
    let mut out = Outcome {
        parts: BTreeMap::new(),
        cost: 0.0,
        sent: Duration::ZERO,
        failed: 0,
        end: End::Finished,
    };
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
                    Err(e) if job::is_fatal(&e) => {
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
                progress(Progress { done, cost: out.cost, failed: out.failed });
            }
            // Só lê mais um pedaço quando há lugar para outro pedido.
            next = rx.recv(), if reading && tasks.len() < PARALLEL => match next {
                None => reading = false,
                Some(chunk) => match chunk.wav {
                    None => {
                        out.parts.insert(chunk.index, Part::Silent);
                        done += chunk.length;
                        progress(Progress { done, cost: out.cost, failed: out.failed });
                    }
                    Some(wav) => {
                        out.sent += chunk.length;
                        let (stt, model, language) = (Arc::clone(&stt), model.clone(), language.clone());
                        let (index, start, length) = (chunk.index, chunk.start, chunk.length);
                        tasks.spawn(async move {
                            let result = job::transcribe(&*stt, wav, &model, language.as_deref()).await;
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
    use crate::stt::openrouter::OpenRouter;

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

    /// Transcreve um ficheiro de verdade com a chave do cofre (custa uns cêntimos
    /// de cêntimo com o whisper). Gera-se uma "aula" com a voz do Windows:
    /// `DITOK_FILE=aula.wav cargo test live_file -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_file() {
        let path = PathBuf::from(std::env::var("DITOK_FILE").expect("define DITOK_FILE"));
        let model = std::env::var("DITOK_MODEL").unwrap_or("openai/whisper-large-v3-turbo".into());
        let key = crate::secrets::get().unwrap().expect("sem chave no cofre");
        let or = OpenRouter::new(
            crate::stt::http_client().unwrap(),
            key,
            crate::stt::openrouter::Transport::Multipart,
        );
        let info = decode::probe(&path).unwrap();
        println!("{} → {:?}", path.display(), info.duration);
        let (_cancel, rx) = watch::channel(false);
        let started = Instant::now();
        let out = transcribe_file(Arc::new(or), path, model, Some("en".into()), rx, |p| {
            println!("  {:?} transcritos, ${:.5}", p.done, p.cost)
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
        assert_eq!(out.end, End::Finished);
    }
}
