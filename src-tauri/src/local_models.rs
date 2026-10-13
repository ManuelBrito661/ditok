//! Os modelos do motor local: o catálogo (o que existe, de onde se descarrega, que
//! ficheiros e que SHA-256 têm) e a descarga.
//!
//! Os modelos não vão no instalador (são centenas de MB): descarregam-se do Hugging
//! Face para `app_data_dir/models/<id>/` quando o utilizador o pede.
//! - Os endereços apontam a um *commit* do repositório, não a `main`: o que se
//!   descarrega hoje é o que se descarregou nos testes, e um repositório que mude
//!   não estraga a app.
//! - Cada ficheiro é escrito em `<nome>.part` e só ganha o nome final depois de o
//!   tamanho e o SHA-256 baterem certo. Por isso "o ficheiro existe" quer dizer
//!   "está completo e verificado", e uma descarga interrompida retoma de onde ia.
//!
//! Este módulo não sabe nada do motor (`stt/local.rs`): compila-se sempre, com ou
//! sem a feature `local`.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// O modelo escolhido. No JSON: "parakeet-v3", "whisper-small", "whisper-turbo",
/// "whisper-medium", "whisper-large-v3".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocalModelId {
    #[default]
    ParakeetV3,
    WhisperSmall,
    WhisperTurbo,
    WhisperMedium,
    WhisperLargeV3,
}

/// Que tipo de modelo é (decide como o motor o carrega).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// NeMo Parakeet (transducer): `encoder`, `decoder`, `joiner`, `tokens`.
    Parakeet,
    /// Whisper: `<prefixo>-encoder`, `<prefixo>-decoder`, `<prefixo>-tokens`.
    Whisper { prefix: &'static str },
}

pub struct ModelFile {
    pub name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

pub struct ModelSpec {
    pub id: LocalModelId,
    pub label: &'static str,
    pub description: &'static str,
    /// Um aviso a mostrar junto ao modelo (pesado, sem pontuação…).
    pub warning: Option<&'static str>,
    #[cfg_attr(not(feature = "local"), allow(dead_code))] // só o motor o lê
    pub family: Family,
    /// Repositório no Hugging Face (`csukuangfj/…`) e o commit a que se fixa.
    pub repo: &'static str,
    pub commit: &'static str,
    /// O maior pedaço de áudio que o modelo descodifica de uma vez: a RAM cresce com
    /// o comprimento (medido: Parakeet 30 s ≈ 1,2 GB; Whisper small 17 s ≈ 1,5 GB).
    #[cfg_attr(not(feature = "local"), allow(dead_code))] // só o motor o lê
    pub max_chunk: Duration,
    /// Memória que o modelo carregado ocupa, a aproximar, para a UI avisar.
    pub ram_mb: u32,
    pub files: &'static [ModelFile],
}

const PARAKEET_FILES: &[ModelFile] = &[
    ModelFile {
        name: "encoder.int8.onnx",
        size: 652_184_281,
        sha256: "acfc2b4456377e15d04f0243af540b7fe7c992f8d898d751cf134c3a55fd2247",
    },
    ModelFile {
        name: "decoder.int8.onnx",
        size: 11_845_275,
        sha256: "179e50c43d1a9de79c8a24149a2f9bac6eb5981823f2a2ed88d655b24248db4e",
    },
    ModelFile {
        name: "joiner.int8.onnx",
        size: 6_355_277,
        sha256: "3164c13fc2821009440d20fcb5fdc78bff28b4db2f8d0f0b329101719c0948b3",
    },
    ModelFile {
        name: "tokens.txt",
        size: 93_939,
        sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
    },
];

const WHISPER_SMALL_FILES: &[ModelFile] = &[
    ModelFile {
        name: "small-encoder.int8.onnx",
        size: 112_442_483,
        sha256: "4cbe7b22fa9026b843b60a68640c747de05bafb1a11b57edc0e66c232d9f33a9",
    },
    ModelFile {
        name: "small-decoder.int8.onnx",
        size: 262_226_114,
        sha256: "acad50b5c782696e91b55914cc5ab4f756f1532f76e22aa6fc615f39fb69a8ee",
    },
    ModelFile {
        name: "small-tokens.txt",
        size: 816_730,
        sha256: "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
    },
];

const WHISPER_TURBO_FILES: &[ModelFile] = &[
    ModelFile {
        name: "turbo-encoder.int8.onnx",
        size: 674_716_297,
        sha256: "b02dcdf54f348741e93fe732b67d933c8dcb6735655f710640143081db38878b",
    },
    ModelFile {
        name: "turbo-decoder.int8.onnx",
        size: 361_080_764,
        sha256: "20accd02388482eb3a46bd615631adfdc85e1eb2c7db9ea3f02a40ffe6b81547",
    },
    ModelFile {
        name: "turbo-tokens.txt",
        size: 816_730,
        sha256: "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
    },
];

const WHISPER_MEDIUM_FILES: &[ModelFile] = &[
    ModelFile {
        name: "medium-encoder.int8.onnx",
        size: 374_196_283,
        sha256: "1c54582b4d829de0089f6cb63bbbdb3bf7555398bacaf855fbecf1a84dfd193e",
    },
    ModelFile {
        name: "medium-decoder.int8.onnx",
        size: 571_059_257,
        sha256: "595d00a338a365a7bfa0ca7f296cabc639583bef770ab6130df90f49a6412747",
    },
    ModelFile {
        name: "medium-tokens.txt",
        size: 816_730,
        sha256: "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
    },
];

const WHISPER_LARGE_V3_FILES: &[ModelFile] = &[
    ModelFile {
        name: "large-v3-encoder.int8.onnx",
        size: 766_671_985,
        sha256: "d531cf17248acc43e8c09b472a0877055e770877857a5332fc1304b36534ec85",
    },
    ModelFile {
        name: "large-v3-decoder.int8.onnx",
        size: 1_008_265_203,
        sha256: "ebc6bfd88e162a46cb3edee8a7e727e1dcbc65cabecb19e2573695e4d495e1af",
    },
    ModelFile {
        name: "large-v3-tokens.txt",
        size: 816_730,
        sha256: "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
    },
];

pub const CATALOG: &[ModelSpec] = &[
    ModelSpec {
        id: LocalModelId::ParakeetV3,
        label: "Parakeet v3",
        description: "O mais rápido e o que melhor escreve (pontuação e maiúsculas). Português e mais 24 línguas europeias, com deteção automática.",
        warning: None,
        family: Family::Parakeet,
        repo: "csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
        commit: "2bda32ec70b097a55adaa07d9a7173915b43cc78",
        max_chunk: Duration::from_secs(30),
        ram_mb: 1200,
        files: PARAKEET_FILES,
    },
    ModelSpec {
        id: LocalModelId::WhisperSmall,
        label: "Whisper small",
        description: "Mais pequeno de descarregar e mais lento (cerca de 4 vezes o tempo real). Usa o idioma das definições.",
        warning: Some("Usa mais memória em pedaços compridos."),
        family: Family::Whisper { prefix: "small" },
        repo: "csukuangfj/sherpa-onnx-whisper-small",
        commit: "8f3c18b358db4d1f2fc1eae49d75cd20989e4309",
        max_chunk: Duration::from_secs(25),
        ram_mb: 1500,
        files: WHISPER_SMALL_FILES,
    },
    ModelSpec {
        id: LocalModelId::WhisperTurbo,
        label: "Whisper turbo",
        description: "O Whisper maior. Nos testes não compensou: mais pesado e mais lento do que os outros.",
        warning: Some(
            "Pesado (até 2,4 GB de memória) e escreve sem pontuação nem maiúsculas nesta versão.",
        ),
        family: Family::Whisper { prefix: "turbo" },
        repo: "csukuangfj/sherpa-onnx-whisper-turbo",
        commit: "2ca6ff69fc878651b770880507669577ac41c2ff",
        max_chunk: Duration::from_secs(25),
        ram_mb: 2400,
        files: WHISPER_TURBO_FILES,
    },
    ModelSpec {
        id: LocalModelId::WhisperMedium,
        label: "Whisper medium",
        description: "Multilíngue e mais preciso do que o small, mas mais lento. Usa o idioma das definições.",
        warning: Some("Pesado: cerca de 950 MB de ficheiros. Mais lento do que o Parakeet."),
        family: Family::Whisper { prefix: "medium" },
        repo: "csukuangfj/sherpa-onnx-whisper-medium",
        commit: "8c31d28503847560985df21f90e14f0c736e075e",
        max_chunk: Duration::from_secs(20),
        ram_mb: 2000,
        files: WHISPER_MEDIUM_FILES,
    },
    ModelSpec {
        id: LocalModelId::WhisperLargeV3,
        label: "Whisper large v3",
        description: "O Whisper maior e mais preciso, multilíngue. Usa o idioma das definições.",
        warning: Some(
            "Muito pesado (cerca de 1,8 GB de ficheiros e mais de 3 GB de memória). Lento.",
        ),
        family: Family::Whisper { prefix: "large-v3" },
        repo: "csukuangfj/sherpa-onnx-whisper-large-v3",
        commit: "2a6507094dd6020d939d78e3f1834a1d06267fca",
        max_chunk: Duration::from_secs(15),
        ram_mb: 3000,
        files: WHISPER_LARGE_V3_FILES,
    },
];

pub fn spec(id: LocalModelId) -> &'static ModelSpec {
    CATALOG
        .iter()
        .find(|m| m.id == id)
        .expect("todos os modelos estão no catálogo")
}

impl ModelSpec {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    pub fn url(&self, file: &ModelFile) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo, self.commit, file.name
        )
    }

    pub fn dir(&self, models_dir: &Path) -> PathBuf {
        models_dir.join(dir_name(self.id))
    }

    /// Todos os ficheiros existem com o tamanho certo. (Só ganham o nome final depois
    /// de verificados, ver `download_files`.)
    pub fn is_installed(&self, models_dir: &Path) -> bool {
        let dir = self.dir(models_dir);
        self.files
            .iter()
            .all(|f| file_len(&dir.join(f.name)) == Some(f.size))
    }

    /// Bytes já descarregados (ficheiros completos e o que há nos `.part`).
    pub fn downloaded_bytes(&self, models_dir: &Path) -> u64 {
        let dir = self.dir(models_dir);
        self.files
            .iter()
            .map(|f| {
                file_len(&dir.join(f.name))
                    .or_else(|| file_len(&part_path(&dir.join(f.name))))
                    .unwrap_or(0)
                    .min(f.size)
            })
            .sum()
    }
}

pub fn dir_name(id: LocalModelId) -> &'static str {
    match id {
        LocalModelId::ParakeetV3 => "parakeet-v3",
        LocalModelId::WhisperSmall => "whisper-small",
        LocalModelId::WhisperTurbo => "whisper-turbo",
        LocalModelId::WhisperMedium => "whisper-medium",
        LocalModelId::WhisperLargeV3 => "whisper-large-v3",
    }
}

fn file_len(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

fn part_path(final_path: &Path) -> PathBuf {
    let mut name = final_path.file_name().unwrap_or_default().to_owned();
    name.push(".part");
    final_path.with_file_name(name)
}

/// Apaga um modelo (os ficheiros e o que ficou a meio). Não falha se não existir.
pub fn delete(spec: &ModelSpec, models_dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(spec.dir(models_dir)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------- descarga

/// O progresso de uma descarga, para a janela.
#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    /// O modelo ("parakeet-v3", "speakers"…): o mesmo texto que a UI usa como identificador.
    pub id: &'static str,
    pub file: String,
    /// Bytes do modelo todo já descarregados.
    pub done: u64,
    pub total: u64,
    /// "downloading", "verifying", "done", "cancelled" ou "error".
    pub state: &'static str,
    pub message: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("descarga cancelada")]
    Cancelled,
    #[error("não há espaço no disco: faltam {needed_mb} MB")]
    NoSpace { needed_mb: u64 },
    #[error("o ficheiro {0} chegou diferente do esperado (SHA-256 não bate); tenta outra vez")]
    BadHash(String),
    #[error("o ficheiro {0} chegou incompleto; tenta outra vez")]
    BadSize(String),
    #[error("falha na rede: {0}")]
    Http(String),
    #[error("erro no disco: {0}")]
    Io(#[from] std::io::Error),
}

impl From<reqwest::Error> for DownloadError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e.to_string())
    }
}

/// Um cliente próprio para as descargas: o dos pedidos de transcrição corta aos 75 s
/// no total, e 650 MB demoram mais do que isso. Aqui só se corta se a ligação ficar
/// parada (30 s sem receber nada).
pub fn download_client() -> reqwest::Result<reqwest::Client> {
    // Já instalado por `stt::http_client` no arranque; `Err` só quer dizer isso.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .user_agent(concat!("Ditok/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()
}

/// O que é preciso descarregar de um ficheiro (o catálogo dá os URLs; os testes dão
/// os de um servidor local).
pub struct Job<'a> {
    pub url: String,
    pub name: &'a str,
    pub size: u64,
    pub sha256: &'a str,
}

/// Descarrega um modelo do catálogo para `models_dir`.
pub async fn download(
    client: &reqwest::Client,
    spec: &ModelSpec,
    models_dir: &Path,
    cancel: &AtomicBool,
    on_progress: impl FnMut(Progress),
) -> Result<(), DownloadError> {
    let jobs: Vec<Job> = spec
        .files
        .iter()
        .map(|f| Job {
            url: spec.url(f),
            name: f.name,
            size: f.size,
            sha256: f.sha256,
        })
        .collect();
    download_files(
        client,
        dir_name(spec.id),
        &spec.dir(models_dir),
        &jobs,
        cancel,
        on_progress,
    )
    .await
}

/// Descarrega os ficheiros pela ordem, retomando o que estiver a meio.
pub async fn download_files(
    client: &reqwest::Client,
    id: &'static str,
    dir: &Path,
    jobs: &[Job<'_>],
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(Progress),
) -> Result<(), DownloadError> {
    std::fs::create_dir_all(dir)?;
    let total: u64 = jobs.iter().map(|j| j.size).sum();

    // Espaço: o que falta descarregar (o que já está não conta).
    let already: u64 = jobs
        .iter()
        .map(|j| {
            let path = dir.join(j.name);
            file_len(&path)
                .or_else(|| file_len(&part_path(&path)))
                .unwrap_or(0)
                .min(j.size)
        })
        .sum();
    let needed = total - already;
    if let Some(free) = free_space(dir)
        && free < needed + SPACE_MARGIN
    {
        return Err(DownloadError::NoSpace {
            needed_mb: (needed + SPACE_MARGIN - free).div_ceil(1_048_576),
        });
    }

    let mut done_before: u64 = 0;
    let mut last_emit = Instant::now() - EMIT_EVERY;
    let mut emit = |file: &str, done: u64, state: &'static str, force: bool| {
        if force || last_emit.elapsed() >= EMIT_EVERY {
            last_emit = Instant::now();
            on_progress(Progress {
                id,
                file: file.to_owned(),
                done,
                total,
                state,
                message: None,
            });
        }
    };

    for job in jobs {
        let final_path = dir.join(job.name);
        if file_len(&final_path) == Some(job.size) {
            done_before += job.size;
            continue;
        }
        let part = part_path(&final_path);
        download_one(client, job, &part, cancel, |got| {
            emit(job.name, done_before + got, "downloading", false);
        })
        .await?;
        // Verificar: o tamanho e o SHA-256 (lê o ficheiro todo, num thread de bloqueio).
        emit(job.name, done_before + job.size, "verifying", true);
        if file_len(&part) != Some(job.size) {
            let _ = std::fs::remove_file(&part);
            return Err(DownloadError::BadSize(job.name.to_owned()));
        }
        let (check, expected) = (part.clone(), job.sha256.to_owned());
        let matches = tauri::async_runtime::spawn_blocking(move || {
            sha256_of(&check).map(|h| h.eq_ignore_ascii_case(&expected))
        })
        .await
        .map_err(|e| DownloadError::Io(std::io::Error::other(e)))??;
        if !matches {
            let _ = std::fs::remove_file(&part);
            return Err(DownloadError::BadHash(job.name.to_owned()));
        }
        std::fs::rename(&part, &final_path)?;
        done_before += job.size;
    }
    emit("", total, "done", true);
    Ok(())
}

/// Margem de espaço livre a deixar além do modelo.
const SPACE_MARGIN: u64 = 200 * 1_048_576;
/// De quanto em quanto tempo se avisa a janela.
const EMIT_EVERY: Duration = Duration::from_millis(250);

/// Descarrega um ficheiro para `part`, a retomar se já tiver bytes.
async fn download_one(
    client: &reqwest::Client,
    job: &Job<'_>,
    part: &Path,
    cancel: &AtomicBool,
    mut on_bytes: impl FnMut(u64),
) -> Result<(), DownloadError> {
    let mut have = file_len(part).unwrap_or(0);
    if have > job.size {
        // Um .part maior do que o ficheiro: lixo, recomeça.
        std::fs::remove_file(part)?;
        have = 0;
    }
    if have == job.size {
        return Ok(()); // já estava completo, só falta verificar
    }

    let mut request = client.get(&job.url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut response = request.send().await?;
    let status = response.status();
    let mut file = if status == reqwest::StatusCode::PARTIAL_CONTENT {
        std::fs::OpenOptions::new().append(true).open(part)?
    } else if status.is_success() {
        // O servidor ignorou o `Range` e mandou tudo: recomeça do zero.
        have = 0;
        std::fs::File::create(part)?
    } else {
        return Err(DownloadError::Http(format!(
            "{} ao descarregar {}",
            status, job.name
        )));
    };

    let mut got = have;
    while let Some(chunk) = response.chunk().await? {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        file.write_all(&chunk)?;
        got += chunk.len() as u64;
        on_bytes(got);
    }
    file.flush()?;
    Ok(())
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Espaço livre no disco onde está `path`, se o sistema o disser.
fn free_space(path: &Path) -> Option<u64> {
    // `absolute` (e não `canonicalize`): no Windows o `canonicalize` devolve caminhos
    // "verbatim" (com o prefixo `\\?\`), que nunca "começam por" o ponto de montagem
    // simples (`C:\`) que o sistema dá.
    let path = std::path::absolute(path).ok()?;
    let disks = sysinfo::Disks::new_with_refreshed_list();
    // O disco cujo ponto de montagem é o prefixo mais comprido do caminho.
    disks
        .list()
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(sysinfo::Disk::available_space)
}

/// Uma descarga em curso, para a poder cancelar.
pub struct ActiveDownload {
    /// `None`: a descarga é de um modelo de oradores (ver `speaker`), não de transcrição.
    pub id: Option<LocalModelId>,
    /// Qual dos modelos de oradores está a descarregar.
    pub speaker: Option<crate::notes::speakers::Model>,
    pub cancel: Arc<AtomicBool>,
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        sync::atomic::AtomicUsize,
    };

    use super::*;

    fn sha(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ditok-localmodels-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Um servidor HTTP mínimo: serve `body`, com ou sem suporte a `Range`, e conta
    /// os pedidos. Aceita ligações até `accept` vezes.
    fn serve(body: Vec<u8>, honor_range: bool, accept: usize) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/ficheiro", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        std::thread::spawn(move || {
            for _ in 0..accept {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                counter.fetch_add(1, Ordering::Relaxed);
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut range_from = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(rest) = line.to_lowercase().strip_prefix("range: bytes=") {
                        range_from = rest.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                }
                let (status, slice, extra) = match range_from {
                    Some(from) if honor_range => (
                        "206 Partial Content",
                        &body[from..],
                        format!(
                            "Content-Range: bytes {from}-{}/{}\r\n",
                            body.len() - 1,
                            body.len()
                        ),
                    ),
                    _ => ("200 OK", &body[..], String::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                    slice.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(slice);
            }
        });
        (url, hits)
    }

    fn client() -> reqwest::Client {
        // O reqwest está sem motor TLS por omissão (ver `stt::http_client`).
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::new()
    }

    fn body(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[test]
    fn catalog_hashes_are_64_hex() {
        for model in CATALOG {
            assert!(!model.files.is_empty(), "{}", model.label);
            for f in model.files {
                assert_eq!(f.sha256.len(), 64, "{}", f.name);
                assert!(
                    f.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                    "{}",
                    f.name
                );
                assert!(f.size > 0, "{}", f.name);
            }
            assert_eq!(model.commit.len(), 40, "o commit tem de estar fixo");
            assert!(model.url(&model.files[0]).contains(model.commit));
        }
        assert_eq!(
            spec(LocalModelId::ParakeetV3).total_bytes(),
            652_184_281 + 11_845_275 + 6_355_277 + 93_939
        );
    }

    #[test]
    fn model_ids_use_kebab_case_in_json() {
        assert_eq!(
            serde_json::to_string(&LocalModelId::WhisperSmall).unwrap(),
            "\"whisper-small\""
        );
        let id: LocalModelId = serde_json::from_str("\"parakeet-v3\"").unwrap();
        assert_eq!(id, LocalModelId::ParakeetV3);
        assert_eq!(LocalModelId::default(), LocalModelId::ParakeetV3);
        // Cada id tem a sua entrada no catálogo e uma pasta própria.
        for id in [
            LocalModelId::ParakeetV3,
            LocalModelId::WhisperSmall,
            LocalModelId::WhisperTurbo,
            LocalModelId::WhisperMedium,
            LocalModelId::WhisperLargeV3,
        ] {
            assert_eq!(spec(id).id, id);
        }
    }

    #[test]
    fn chunk_size_for_whisper_is_under_30s() {
        // O Whisper trabalha em janelas de 30 s; o Parakeet gasta RAM com o comprimento.
        assert!(spec(LocalModelId::WhisperSmall).max_chunk < Duration::from_secs(30));
        assert!(spec(LocalModelId::WhisperTurbo).max_chunk < Duration::from_secs(30));
        assert!(spec(LocalModelId::WhisperMedium).max_chunk < Duration::from_secs(30));
        assert!(spec(LocalModelId::WhisperLargeV3).max_chunk < Duration::from_secs(30));
        assert!(spec(LocalModelId::ParakeetV3).max_chunk <= Duration::from_secs(30));
    }

    #[tokio::test]
    async fn download_writes_verifies_and_renames() {
        let data = body(300_000);
        let (url, _) = serve(data.clone(), true, 1);
        let dir = temp_dir("ok");
        let hash = sha(&data);
        let job = Job {
            url,
            name: "modelo.onnx",
            size: data.len() as u64,
            sha256: &hash,
        };
        let cancel = AtomicBool::new(false);
        let mut states = Vec::new();
        download_files(&client(), "parakeet-v3", &dir, &[job], &cancel, |p| {
            states.push(p.state)
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read(dir.join("modelo.onnx")).unwrap(), data);
        assert!(!dir.join("modelo.onnx.part").exists());
        assert!(states.contains(&"verifying"));
        assert_eq!(states.last(), Some(&"done"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn download_resumes_from_part_file() {
        let data = body(300_000);
        let (url, hits) = serve(data.clone(), true, 2);
        let dir = temp_dir("resume");
        std::fs::create_dir_all(&dir).unwrap();
        // Uma descarga anterior parou a meio.
        std::fs::write(dir.join("modelo.onnx.part"), &data[..120_000]).unwrap();
        let hash = sha(&data);
        let job = Job {
            url,
            name: "modelo.onnx",
            size: data.len() as u64,
            sha256: &hash,
        };
        let cancel = AtomicBool::new(false);
        download_files(&client(), "parakeet-v3", &dir, &[job], &cancel, |_| {})
            .await
            .unwrap();
        assert_eq!(std::fs::read(dir.join("modelo.onnx")).unwrap(), data);
        assert_eq!(
            hits.load(Ordering::Relaxed),
            1,
            "um só pedido, só com o que faltava"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn download_restarts_when_the_server_ignores_range() {
        let data = body(100_000);
        let (url, _) = serve(data.clone(), false, 1);
        let dir = temp_dir("norange");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("modelo.onnx.part"), &data[..40_000]).unwrap();
        let hash = sha(&data);
        let job = Job {
            url,
            name: "modelo.onnx",
            size: data.len() as u64,
            sha256: &hash,
        };
        let cancel = AtomicBool::new(false);
        download_files(&client(), "parakeet-v3", &dir, &[job], &cancel, |_| {})
            .await
            .unwrap();
        // Se tivesse acrescentado ao .part, o ficheiro ficava com 140 000 bytes.
        assert_eq!(std::fs::read(dir.join("modelo.onnx")).unwrap(), data);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn download_rejects_bad_hash() {
        let data = body(50_000);
        let (url, _) = serve(data.clone(), true, 1);
        let dir = temp_dir("badhash");
        let wrong = sha(b"outra coisa");
        let job = Job {
            url,
            name: "modelo.onnx",
            size: data.len() as u64,
            sha256: &wrong,
        };
        let cancel = AtomicBool::new(false);
        let err = download_files(&client(), "parakeet-v3", &dir, &[job], &cancel, |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, DownloadError::BadHash(_)), "{err}");
        assert!(
            !dir.join("modelo.onnx").exists(),
            "nunca fica um ficheiro falso"
        );
        assert!(!dir.join("modelo.onnx.part").exists(), "e o lixo é apagado");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn download_rejects_a_wrong_size() {
        let data = body(50_000);
        let (url, _) = serve(data.clone(), true, 1);
        let dir = temp_dir("badsize");
        let hash = sha(&data);
        let job = Job {
            url,
            name: "modelo.onnx",
            size: data.len() as u64 + 10,
            sha256: &hash,
        };
        let cancel = AtomicBool::new(false);
        let err = download_files(&client(), "parakeet-v3", &dir, &[job], &cancel, |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, DownloadError::BadSize(_)), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn download_can_be_cancelled_and_keeps_the_part() {
        let data = body(2_000_000);
        let (url, _) = serve(data.clone(), true, 1);
        let dir = temp_dir("cancel");
        let hash = sha(&data);
        let job = Job {
            url,
            name: "modelo.onnx",
            size: data.len() as u64,
            sha256: &hash,
        };
        let cancel = AtomicBool::new(true); // já cancelada: pára no primeiro bloco
        let err = download_files(&client(), "parakeet-v3", &dir, &[job], &cancel, |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, DownloadError::Cancelled), "{err}");
        assert!(!dir.join("modelo.onnx").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn existing_complete_files_are_skipped() {
        let dir = temp_dir("skip");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("modelo.onnx"), b"completo").unwrap();
        // O URL não existe: se tentasse descarregar, falhava.
        let job = Job {
            url: "http://127.0.0.1:1/nao-existe".into(),
            name: "modelo.onnx",
            size: 8,
            sha256: &sha(b"completo"),
        };
        let cancel = AtomicBool::new(false);
        download_files(&client(), "parakeet-v3", &dir, &[job], &cancel, |_| {})
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn installed_needs_every_file_with_the_exact_size() {
        let models = temp_dir("installed");
        let spec = spec(LocalModelId::WhisperSmall);
        assert!(!spec.is_installed(&models));
        let dir = spec.dir(&models);
        std::fs::create_dir_all(&dir).unwrap();
        for f in spec.files {
            std::fs::write(dir.join(f.name), vec![0u8; f.size as usize % 1000]).unwrap();
        }
        assert!(!spec.is_installed(&models), "tamanhos errados não contam");
        assert!(spec.downloaded_bytes(&models) > 0);
        delete(spec, &models).unwrap();
        assert!(!dir.exists());
        // Apagar o que não existe não é um erro.
        delete(spec, &models).unwrap();
        let _ = std::fs::remove_dir_all(models);
    }

    #[test]
    fn free_space_is_reported_for_an_existing_folder() {
        let dir = temp_dir("space");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(free_space(&dir).is_some_and(|b| b > 0));
        let _ = std::fs::remove_dir_all(dir);
    }
}
