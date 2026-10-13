//! xAI: transcrição de voz em lote (REST) e em streaming (WebSocket).
//!
//! - **Lote**: `POST /v1/stt` (multipart; os campos antes do `file`, que vai em último).
//!   Serve o "Tentar de novo", o reprocessar, os ficheiros, as notas e é a alternativa
//!   quando o stream falha.
//! - **Streaming**: `wss://api.x.ai/v1/stt`, com a chave no header `Authorization` e a
//!   configuração nos parâmetros do URL. Fluxo: esperar por `transcript.created`, enviar
//!   o áudio (frames binários PCM s16le 16 kHz), no fim `finalize` + `audio.done`, e ler
//!   até `transcript.done`. Durante a fala chegam `transcript.partial`.
//!
//! Preços (docs.x.ai/developers/models): 0,10 USD/hora em lote, 0,20 USD/hora em streaming.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use reqwest::{
    Body, Client,
    multipart::{Form, Part},
};
use serde::Deserialize;
use tokio::sync::watch;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{self, Message, client::IntoClientRequest, http::HeaderValue},
};

use super::{
    AudioRx, LiveSession, StreamingStt, SttError, SttProvider, SttRequest, Transcription, Usage,
    error_from_response,
};

const MODEL: &str = "grok-voice-transcribe-2.0";
/// O "modelo" que fica nas métricas e no histórico (como `local/<modelo>`).
pub const MODEL_SLUG: &str = "xai/grok-voice-transcribe-2.0";
const API: &str = "https://api.x.ai/v1";
const WS_URL: &str = "wss://api.x.ai/v1/stt";

const BATCH_USD_PER_SEC: f64 = 0.10 / 3600.0;
const STREAM_USD_PER_SEC: f64 = 0.20 / 3600.0;
/// PCM s16le mono a 16 kHz.
const BYTES_PER_SEC: f64 = 32_000.0;
/// Frames de ~200 ms: um bloco acumulado (ex.: durante a ligação) parte-se em frames
/// destes. A doc recomenda 100 ms; o servidor aceita mais.
const MAX_FRAME: usize = 6_400;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Do fim da ligação até `transcript.created`.
const CREATED_TIMEOUT: Duration = Duration::from_secs(5);
/// Do fim do áudio até `transcript.done`.
pub const DONE_TIMEOUT: Duration = Duration::from_secs(4);

pub struct Xai {
    client: Client,
    api_key: String,
    ws_url: String,
}

// Como o `OpenRouter`: a chave nunca aparece num log.
impl std::fmt::Debug for Xai {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Xai").finish_non_exhaustive()
    }
}

impl Xai {
    pub fn new(client: Client, api_key: String) -> Self {
        Self {
            client,
            api_key,
            ws_url: WS_URL.to_owned(),
        }
    }

    /// O URL do WebSocket com a configuração do ditado.
    fn stream_url(&self, language: Option<&str>) -> String {
        let mut url = format!(
            "{}?model={MODEL}&sample_rate=16000&encoding=pcm&interim_results=true",
            self.ws_url
        );
        if let Some(lang) = language {
            // `format` (números, moedas, unidades por extenso) exige o idioma.
            url.push_str(&format!("&language={}&format=true", api_language(lang)));
        }
        url
    }
}

/// O Ditok guarda ISO-639-1 ("pt"); na xAI, "pt" é o português do Brasil e o europeu
/// é "pt-PT". Os outros códigos seguem como estão.
pub fn api_language(lang: &str) -> &str {
    match lang {
        "pt" => "pt-PT",
        other => other,
    }
}

#[derive(Deserialize)]
struct BatchBody {
    text: String,
    #[serde(default)]
    duration: Option<f64>,
}

#[async_trait]
impl SttProvider for Xai {
    fn name(&self) -> &'static str {
        "xai"
    }

    async fn prewarm(&self) {
        // Abre TCP + TLS + HTTP/2 ao host da API (o "Tentar de novo" e o lote usam-no).
        // O WebSocket liga-se à parte, em fundo, enquanto se fala.
        let started = Instant::now();
        match self.client.head(format!("{API}/models")).send().await {
            Ok(r) => tracing::debug!(
                status = r.status().as_u16(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "pre-warm xai"
            ),
            Err(e) => tracing::debug!(error = %SttError::from(e), "pre-warm xai falhou"),
        }
    }

    async fn transcribe(
        &self,
        audio: Bytes,
        req: &SttRequest<'_>,
    ) -> Result<Transcription, SttError> {
        let started = Instant::now();
        let len = audio.len() as u64;
        // `Body::from(Bytes)` aponta para o mesmo buffer: o áudio não é copiado.
        let file = Part::stream_with_length(Body::from(audio), len)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(SttError::from)?;
        let mut form = Form::new().text("model", MODEL);
        if let Some(lang) = req.language {
            form = form
                .text("language", api_language(lang).to_owned())
                .text("format", "true");
        }
        // O ficheiro vai em último: a xAI lê os campos pela ordem.
        let form = form.part("file", file);
        let response = self
            .client
            .post(format!("{API}/stt"))
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await?;
        let status = response.status();
        let body = response.bytes().await?;
        let elapsed = started.elapsed();
        if !status.is_success() {
            return Err(error_from_response(status.as_u16(), &body, MODEL));
        }
        let parsed: BatchBody =
            serde_json::from_slice(&body).map_err(|e| SttError::Decode(e.to_string()))?;
        Ok(Transcription {
            text: parsed.text,
            usage: Usage {
                seconds: parsed.duration,
                cost: parsed.duration.map(|s| s * BATCH_USD_PER_SEC),
                ..Usage::default()
            },
            generation_id: None,
            elapsed,
            transport: "multipart",
        })
    }
}

/// Confirma que a chave é aceite pela xAI, sem gastar nada (`GET /v1/models`).
pub async fn check_key(client: &Client, api_key: &str) -> Result<(), SttError> {
    let response = client
        .get(format!("{API}/models"))
        .bearer_auth(api_key)
        .send()
        .await?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.bytes().await?;
    Err(error_from_response(status.as_u16(), &body, ""))
}

impl StreamingStt for Xai {
    fn open(&self, language: Option<String>, audio: AudioRx) -> LiveSession {
        let (tx, rx) = watch::channel(String::new());
        let url = self.stream_url(language.as_deref());
        let key = self.api_key.clone();
        let handle = tauri::async_runtime::spawn(async move { run(url, key, audio, tx).await });
        LiveSession::new(rx, handle)
    }
}

// ---------------------------------------------------------------------------
// Eventos e montagem do texto
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Event {
    Created,
    Partial {
        text: String,
        is_final: bool,
        speech_final: bool,
    },
    Done {
        text: String,
        duration: Option<f64>,
    },
    Error(String),
    Other,
}

fn parse_event(raw: &str) -> Event {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(rename = "type", default)]
        kind: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        is_final: bool,
        #[serde(default)]
        speech_final: bool,
        #[serde(default)]
        duration: Option<f64>,
        #[serde(default)]
        message: Option<String>,
    }
    let Ok(e) = serde_json::from_str::<Raw>(raw) else {
        return Event::Other;
    };
    match e.kind.as_str() {
        "transcript.created" => Event::Created,
        "transcript.partial" => Event::Partial {
            text: e.text,
            is_final: e.is_final,
            speech_final: e.speech_final,
        },
        "transcript.done" => Event::Done {
            text: e.text,
            duration: e.duration,
        },
        "error" => Event::Error(e.message.unwrap_or_else(|| "erro sem mensagem".into())),
        _ => Event::Other,
    }
}

fn join(into: &mut String, piece: &str) {
    let piece = piece.trim();
    if piece.is_empty() {
        return;
    }
    if !into.is_empty() {
        into.push(' ');
    }
    into.push_str(piece);
}

/// Junta os `transcript.partial` no texto até agora. Três camadas: as frases já
/// acabadas (`speech_final`), os pedaços fixos da frase em curso (`is_final` sem
/// `speech_final`) e o parcial que ainda pode mudar.
#[derive(Debug, Default)]
struct Assembler {
    sentences: String,
    chunks: String,
    interim: String,
}

impl Assembler {
    fn partial(&mut self, text: &str, is_final: bool, speech_final: bool) {
        match (is_final, speech_final) {
            (false, _) => self.interim = text.trim().to_owned(),
            (true, false) => {
                join(&mut self.chunks, text);
                self.interim.clear();
            }
            (true, true) => {
                // O final de uma frase repete-a toda, incluindo os pedaços já fixos.
                // Se vier só a cauda, junta-se aos pedaços.
                let text = text.trim();
                let chunks = self.chunks.trim();
                if chunks.is_empty() || text.contains(chunks) {
                    join(&mut self.sentences, text);
                } else {
                    join(&mut self.sentences, chunks);
                    join(&mut self.sentences, text);
                }
                self.chunks.clear();
                self.interim.clear();
            }
        }
    }

    fn text(&self) -> String {
        let mut out = self.sentences.clone();
        join(&mut out, &self.chunks);
        join(&mut out, &self.interim);
        out
    }
}

fn event_error(message: &str) -> SttError {
    let lower = message.to_lowercase();
    if lower.contains("unauthor") || lower.contains("api key") || lower.contains("credential") {
        SttError::Unauthorized(message.to_owned())
    } else if lower.contains("rate limit") || lower.contains("too many") {
        SttError::RateLimited(message.to_owned())
    } else {
        SttError::BadRequest(message.to_owned())
    }
}

fn ws_error(e: tungstenite::Error) -> SttError {
    if let tungstenite::Error::Http(response) = &e {
        let status = response.status().as_u16();
        let body = response.body().as_deref().unwrap_or_default();
        return error_from_response(status, body, MODEL);
    }
    SttError::Network(e.to_string())
}

// ---------------------------------------------------------------------------
// A sessão
// ---------------------------------------------------------------------------

/// A tarefa de um ditado em streaming. Devolve o texto final; qualquer `Err` faz o
/// pipeline voltar ao caminho em lote.
async fn run(
    url: String,
    api_key: String,
    mut audio: AudioRx,
    partials: watch::Sender<String>,
) -> Result<Transcription, SttError> {
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| SttError::BadRequest(e.to_string()))?;
    let auth = HeaderValue::from_str(&format!("Bearer {api_key}"))
        .map_err(|_| SttError::Unauthorized("a chave tem carateres inválidos".into()))?;
    request.headers_mut().insert("Authorization", auth);

    let connecting = connect_async_with_config(request, None, true);
    let (mut ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
        .await
        .map_err(|_| SttError::ClientTimeout)?
        .map_err(ws_error)?;

    let mut assembler = Assembler::default();
    let mut created = false;
    let mut audio_done = false;
    let mut ended_at: Option<Instant> = None;
    let mut sent_bytes = 0usize;
    let mut done: Option<(String, Option<f64>)> = None;

    let created_timer = tokio::time::sleep(CREATED_TIMEOUT);
    let done_timer = tokio::time::sleep(Duration::from_secs(86_400));
    tokio::pin!(created_timer, done_timer);

    while done.is_none() {
        tokio::select! {
            // Só se envia depois de `transcript.created`; até lá o áudio fica no canal.
            chunk = audio.recv(), if created && !audio_done => match chunk {
                Some(first) => {
                    let mut buf = first.to_vec();
                    while let Ok(more) = audio.try_recv() {
                        buf.extend_from_slice(&more);
                    }
                    sent_bytes += buf.len();
                    for frame in buf.chunks(MAX_FRAME) {
                        ws.send(Message::binary(frame.to_vec())).await.map_err(ws_error)?;
                    }
                }
                None => {
                    audio_done = true;
                    ended_at = Some(Instant::now());
                    // `finalize` fecha já a frase em curso (não espera pelo silêncio).
                    ws.send(Message::text(r#"{"type":"finalize"}"#)).await.map_err(ws_error)?;
                    ws.send(Message::text(r#"{"type":"audio.done"}"#)).await.map_err(ws_error)?;
                    done_timer.as_mut().reset(tokio::time::Instant::now() + DONE_TIMEOUT);
                }
            },
            msg = ws.next() => match msg {
                None | Some(Ok(Message::Close(_))) => {
                    return Err(SttError::Network("o servidor fechou a ligação".into()));
                }
                Some(Err(e)) => return Err(ws_error(e)),
                Some(Ok(Message::Text(raw))) => match parse_event(&raw) {
                    Event::Created => created = true,
                    Event::Partial { text, is_final, speech_final } => {
                        assembler.partial(&text, is_final, speech_final);
                        let shown = assembler.text();
                        partials.send_if_modified(|current| {
                            let changed = *current != shown;
                            if changed {
                                *current = shown;
                            }
                            changed
                        });
                    }
                    Event::Done { text, duration } => done = Some((text, duration)),
                    Event::Error(message) => return Err(event_error(&message)),
                    Event::Other => {}
                },
                Some(Ok(_)) => {}
            },
            () = &mut created_timer, if !created => return Err(SttError::ClientTimeout),
            () = &mut done_timer, if audio_done => return Err(SttError::ClientTimeout),
        }
    }

    let (done_text, duration) = done.unwrap_or_default();
    let assembled = assembler.text();
    let text = if done_text.trim().is_empty() {
        assembled.clone()
    } else {
        done_text.trim().to_owned()
    };
    tracing::debug!(%done_text, %assembled, "stream xai terminado");
    let seconds = duration.unwrap_or(sent_bytes as f64 / BYTES_PER_SEC);
    let _ = ws.close(None).await;
    Ok(Transcription {
        text,
        usage: Usage {
            seconds: Some(seconds),
            cost: Some(seconds * STREAM_USD_PER_SEC),
            ..Usage::default()
        },
        generation_id: None,
        elapsed: ended_at.map(|t| t.elapsed()).unwrap_or_default(),
        transport: "ws",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    #[test]
    fn portuguese_means_european_portuguese() {
        assert_eq!(api_language("pt"), "pt-PT");
        assert_eq!(api_language("en"), "en");
    }

    #[test]
    fn stream_url_carries_the_configuration() {
        let x = Xai::new(crate::stt::http_client().unwrap(), "k".into());
        let url = x.stream_url(Some("pt"));
        assert!(url.starts_with("wss://api.x.ai/v1/stt?model=grok-voice-transcribe-2.0"));
        assert!(url.contains("sample_rate=16000"));
        assert!(url.contains("encoding=pcm"));
        assert!(url.contains("interim_results=true"));
        assert!(url.ends_with("&language=pt-PT&format=true"));
        // Sem idioma, deteção automática e sem formatação (exige idioma).
        assert!(!x.stream_url(None).contains("language"));
        assert!(!x.stream_url(None).contains("format"));
    }

    #[test]
    fn parses_the_documented_events() {
        assert_eq!(
            parse_event(r#"{"type":"transcript.created"}"#),
            Event::Created
        );
        assert_eq!(
            parse_event(
                r#"{"type":"transcript.partial","text":"olá","words":[],"is_final":true,"speech_final":false,"start":0.0,"duration":1.0}"#
            ),
            Event::Partial {
                text: "olá".into(),
                is_final: true,
                speech_final: false
            }
        );
        assert_eq!(
            parse_event(r#"{"type":"transcript.done","text":"olá mundo","duration":2.5}"#),
            Event::Done {
                text: "olá mundo".into(),
                duration: Some(2.5)
            }
        );
        assert_eq!(
            parse_event(r#"{"type":"error","message":"bad audio"}"#),
            Event::Error("bad audio".into())
        );
        assert_eq!(parse_event("não é json"), Event::Other);
        assert_eq!(parse_event(r#"{"type":"algo.novo"}"#), Event::Other);
    }

    #[test]
    fn assembles_interim_chunks_and_sentences() {
        let mut a = Assembler::default();
        a.partial("eu quero", false, false);
        assert_eq!(a.text(), "eu quero");
        a.partial("eu quero dois", false, false);
        assert_eq!(a.text(), "eu quero dois", "o parcial é substituído, não somado");
        // Um pedaço fixo, depois mais um parcial.
        a.partial("eu quero dois desses", true, false);
        a.partial("por favor", false, false);
        assert_eq!(a.text(), "eu quero dois desses por favor");
        // O fim da frase repete tudo: não duplica.
        a.partial("eu quero dois desses por favor.", true, true);
        assert_eq!(a.text(), "eu quero dois desses por favor.");
        // Uma segunda frase junta-se.
        a.partial("obrigado", false, false);
        a.partial("Obrigado.", true, true);
        assert_eq!(a.text(), "eu quero dois desses por favor. Obrigado.");
    }

    #[test]
    fn a_final_with_only_the_tail_keeps_the_chunks() {
        let mut a = Assembler::default();
        a.partial("primeira parte", true, false);
        a.partial("segunda parte", true, true);
        assert_eq!(a.text(), "primeira parte segunda parte");
    }

    #[test]
    fn maps_error_events() {
        assert!(matches!(
            event_error("Invalid API key"),
            SttError::Unauthorized(_)
        ));
        assert!(matches!(
            event_error("rate limit exceeded"),
            SttError::RateLimited(_)
        ));
        assert!(matches!(event_error("bad audio"), SttError::BadRequest(_)));
    }

    /// Um servidor xAI de brincadeira: envia `created`, conta o áudio, responde a
    /// `finalize` com um final e a `audio.done` com `done`.
    async fn fake_server(listener: TcpListener, saw_auth: mpsc::UnboundedSender<bool>) {
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
        let (stream, _) = listener.accept().await.unwrap();
        let callback = |req: &Request, resp: Response| {
            let ok = req
                .headers()
                .get("authorization")
                .is_some_and(|v| v == "Bearer segredo");
            saw_auth.send(ok).unwrap();
            Ok(resp)
        };
        let mut ws = tokio_tungstenite::accept_hdr_async(stream, callback)
            .await
            .unwrap();
        ws.send(Message::text(r#"{"type":"transcript.created"}"#))
            .await
            .unwrap();
        let mut bytes = 0usize;
        while let Some(Ok(msg)) = ws.next().await {
            match msg {
                Message::Binary(b) => {
                    bytes += b.len();
                    if bytes == 6400 {
                        let partial = r#"{"type":"transcript.partial","text":"olá","is_final":false,"speech_final":false}"#;
                        ws.send(Message::text(partial)).await.unwrap();
                    }
                }
                Message::Text(t) if t.contains("finalize") => {
                    let fin = r#"{"type":"transcript.partial","text":"Olá, mundo.","is_final":true,"speech_final":true}"#;
                    ws.send(Message::text(fin)).await.unwrap();
                }
                Message::Text(t) if t.contains("audio.done") => {
                    let done = format!(
                        r#"{{"type":"transcript.done","text":"Olá, mundo.","duration":{}}}"#,
                        bytes as f64 / 32000.0
                    );
                    ws.send(Message::text(done)).await.unwrap();
                    break;
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn streams_audio_and_returns_the_final_text() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (auth_tx, mut auth_rx) = mpsc::unbounded_channel();
        let server = tokio::spawn(fake_server(listener, auth_tx));

        let (audio_tx, audio_rx) = mpsc::unbounded_channel::<Bytes>();
        let (partials_tx, mut partials_rx) = watch::channel(String::new());
        let url = format!("ws://127.0.0.1:{port}/v1/stt?model=x");
        let session = tokio::spawn(run(url, "segredo".into(), audio_rx, partials_tx));

        // 0,2 s de áudio em blocos de 50 ms, como o tap do microfone.
        for _ in 0..4 {
            audio_tx.send(Bytes::from(vec![0u8; 1600])).unwrap();
        }
        // Um parcial chega enquanto o áudio ainda está aberto.
        tokio::time::timeout(Duration::from_secs(3), partials_rx.changed())
            .await
            .expect("sem parcial")
            .unwrap();
        assert_eq!(*partials_rx.borrow(), "olá");
        drop(audio_tx);

        let result = tokio::time::timeout(Duration::from_secs(5), session)
            .await
            .expect("tempo esgotado")
            .unwrap()
            .unwrap();
        assert_eq!(result.text, "Olá, mundo.");
        assert_eq!(result.transport, "ws");
        assert_eq!(result.usage.seconds, Some(0.2));
        assert!(auth_rx.recv().await.unwrap(), "o header Authorization");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_dead_server_is_a_network_error() {
        // Porta fechada: ligar falha logo (e o pipeline passa ao lote).
        let (_tx, rx) = mpsc::unbounded_channel::<Bytes>();
        let (partials_tx, _) = watch::channel(String::new());
        let err = run("ws://127.0.0.1:1/v1/stt".into(), "k".into(), rx, partials_tx)
            .await
            .unwrap_err();
        assert!(matches!(err, SttError::Network(_)), "{err:?}");
    }
}
