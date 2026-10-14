//! Speech-to-text: o trait comum e o que é partilhado entre providers.
//!
//! Porquê um trait: o orquestrador e o benchmark só precisam de "dá-me texto a partir
//! deste áudio". Cada provider (OpenRouter, Groq, …) implementa-o à sua maneira e o
//! resto do código não muda quando se acrescenta um novo.

// Só o motor local o usa (e os testes dele).
#[cfg(any(feature = "local", test))]
pub mod idle;
pub mod job;
pub mod local;
pub mod models;
pub mod openrouter;
pub mod xai;

use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use serde::Deserialize;

/// Parâmetros de um pedido. Emprestados (`&str`) em vez de `String`: o pedido só
/// precisa de os ler enquanto dura, por isso não há motivo para os copiar.
#[derive(Debug, Clone, Copy)]
pub struct SttRequest<'a> {
    /// Slug completo do modelo (ex.: `openai/whisper-large-v3-turbo`).
    pub model: &'a str,
    /// Código ISO-639-1 (ex.: `pt`). `None` = deteção automática.
    pub language: Option<&'a str>,
    /// Nomes e termos a favorecer na transcrição (do dicionário). Só a xAI os usa.
    pub keyterms: &'a [String],
}

/// Os termos do dicionário a dar ao motor de transcrição (`keyterm` da xAI: no máximo
/// 100, de 50 caracteres cada). Conta a forma escrita certa, a de `to`; as variantes
/// mal ouvidas (`from`) continuam a ser corrigidas depois, por `text.rs`.
pub fn keyterms(dictionary: &[crate::text::Replacement]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in dictionary {
        let term = r.to.trim();
        if term.is_empty() || term.chars().count() > 50 || out.iter().any(|t| t == term) {
            continue;
        }
        out.push(term.to_owned());
        if out.len() == 100 {
            break;
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct Transcription {
    pub text: String,
    pub usage: Usage,
    pub generation_id: Option<String>,
    /// Upload + transcrição (do envio do pedido ao fim da resposta).
    pub elapsed: Duration,
    /// Como o áudio foi enviado (`multipart` ou `json`).
    pub transport: &'static str,
}

/// Objeto `usage` devolvido pelo OpenRouter. Todos os campos são opcionais porque
/// variam de provider para provider.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub seconds: Option<f64>,
    pub cost: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum SttError {
    #[error("chave da API rejeitada ({0}). Confirma a chave guardada no cofre do sistema.")]
    Unauthorized(String),
    // 403: a chave existe mas o pedido foi recusado (limite da chave, moderação…).
    #[error("pedido recusado pelo servidor ({0})")]
    Forbidden(String),
    #[error("sem créditos suficientes na conta ({0})")]
    InsufficientCredits(String),
    #[error(
        "modelo \"{model}\" não encontrado: usa o slug completo, ex.: openai/whisper-large-v3-turbo"
    )]
    ModelNotFound { model: String },
    /// Motor local: o modelo escolhido não está descarregado (ou ficou incompleto).
    #[error("o modelo local \"{0}\" não está descarregado: descarrega-o nas Definições")]
    #[cfg_attr(not(feature = "local"), allow(dead_code))] // só o motor local o cria
    ModelMissing(String),
    /// Motor local: o modelo não carregou ou falhou ao descodificar.
    #[error("o motor local falhou: {0}")]
    Inference(String),
    #[error("demasiados pedidos; tenta daqui a pouco ({0})")]
    RateLimited(String),
    #[error(
        "o provider excedeu o tempo de processamento (limite de 60 s). Grava trechos mais curtos."
    )]
    ProviderTimeout,
    #[error("áudio demasiado grande para o provider (limite de 25 MB)")]
    PayloadTooLarge,
    #[error("pedido recusado pelo provider: {0}")]
    BadRequest(String),
    #[error("erro HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("sem resposta do servidor dentro do tempo limite")]
    ClientTimeout,
    #[error("erro de rede: {0}")]
    Network(String),
    #[error("resposta inesperada do provider: {0}")]
    Decode(String),
}

impl From<reqwest::Error> for SttError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            Self::ClientTimeout
        } else {
            // O Display do reqwest só mostra o nível de topo ("error sending
            // request"); a causa útil (ex.: "dns error") está na cadeia de `source`.
            let mut msg = e.to_string();
            let mut source = std::error::Error::source(&e);
            while let Some(s) = source {
                msg.push_str(": ");
                msg.push_str(&s.to_string());
                source = s.source();
            }
            // Só os erros de ligação são "sem rede". Um corpo de resposta que não se
            // conseguiu ler (a ligação existia) é uma resposta inesperada.
            if e.is_decode() || e.is_body() {
                Self::Decode(msg)
            } else {
                Self::Network(msg)
            }
        }
    }
}

#[async_trait]
pub trait SttProvider: Send + Sync {
    fn name(&self) -> &'static str;

    /// Abre (ou reaproveita) a ligação TLS/HTTP2 ao servidor, para que o pedido
    /// verdadeiro não pague o handshake. Chamado ao premir o atalho.
    async fn prewarm(&self);

    /// `audio` é `Bytes`: passar para o pedido HTTP não copia os dados.
    async fn transcribe(
        &self,
        audio: Bytes,
        req: &SttRequest<'_>,
    ) -> Result<Transcription, SttError>;
}

/// Blocos de áudio para o stream: PCM s16le, 16 kHz, mono. O fim do canal (todos os
/// `Sender` largados) é o fim do áudio: o stream pede então o texto final.
pub type AudioRx = tokio::sync::mpsc::UnboundedReceiver<Bytes>;

/// Transcrição em tempo real: o áudio segue para o servidor enquanto se fala e o
/// texto vai aparecendo. Fica ao lado de `SttProvider` (que recebe o áudio completo):
/// o WAV continua a ser gravado sempre e serve de alternativa se o stream falhar.
pub trait StreamingStt: Send + Sync {
    /// Devolve logo, sem esperar pela ligação: a tarefa liga-se em fundo e o áudio
    /// que chegar entretanto fica no canal. `language`: código ISO-639-1 do utilizador;
    /// `keyterms`: os termos do dicionário (ver `keyterms`).
    fn open(&self, language: Option<String>, keyterms: Vec<String>, audio: AudioRx) -> LiveSession;
}

/// Um stream em curso. Largá-lo (ex.: Esc) aborta a tarefa e fecha a ligação.
pub struct LiveSession {
    /// O texto até agora (frases acabadas + a frase em curso), para mostrar ao vivo.
    pub partials: tokio::sync::watch::Receiver<String>,
    result: Option<tauri::async_runtime::JoinHandle<Result<Transcription, SttError>>>,
}

impl LiveSession {
    pub fn new(
        partials: tokio::sync::watch::Receiver<String>,
        result: tauri::async_runtime::JoinHandle<Result<Transcription, SttError>>,
    ) -> Self {
        Self {
            partials,
            result: Some(result),
        }
    }

    /// Espera pelo texto final. `Err` (incluindo o tempo esgotado) significa que se
    /// deve voltar ao caminho em lote com o WAV.
    pub async fn finish(mut self, wait: Duration) -> Result<Transcription, SttError> {
        let Some(handle) = self.result.take() else {
            return Err(SttError::Decode("stream já terminado".into()));
        };
        match tokio::time::timeout(wait, handle).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(SttError::Network(format!("tarefa do stream: {e}"))),
            Err(_) => Err(SttError::ClientTimeout),
        }
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        if let Some(handle) = self.result.take() {
            handle.abort();
        }
    }
}

/// O que um ditado precisa para falar com o exterior: quem transcreve e quem
/// trata o texto (limpeza, comandos, resumos). Com a nuvem são o mesmo cliente do
/// OpenRouter; com o motor local o `stt` é local e o `llm` só existe se houver
/// chave (sem ela, ditar funciona mas a limpeza e os comandos ficam de fora). Com a
/// xAI, o `stt` é a xAI, o `stream` é o mesmo cliente em tempo real e o `llm` idem.
///
/// `Clone` é barato: são `Arc`s.
#[derive(Clone)]
pub struct Providers {
    pub stt: std::sync::Arc<dyn SttProvider>,
    pub llm: Option<std::sync::Arc<openrouter::OpenRouter>>,
    /// `None` nos motores sem streaming (OpenRouter, local).
    pub stream: Option<std::sync::Arc<dyn StreamingStt>>,
}

/// Cliente HTTP partilhado por todos os providers. O `reqwest::Client` mantém um
/// pool de ligações; clonar o `Client` partilha o mesmo pool (é um `Arc` por dentro).
pub fn http_client() -> reqwest::Result<reqwest::Client> {
    // O rustls precisa de um motor criptográfico. Usamos o `ring` (compila sem NASM
    // nem CMake, ao contrário do aws-lc). `Err` aqui só significa "já instalado".
    let _ = rustls::crypto::ring::default_provider().install_default();

    reqwest::Client::builder()
        .user_agent(concat!("Ditok/", env!("CARGO_PKG_VERSION")))
        // Mantém a ligação viva entre ditados próximos…
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(30))
        .http2_keep_alive_interval(Duration::from_secs(20))
        .http2_keep_alive_while_idle(true)
        .connect_timeout(Duration::from_secs(10))
        // …e corta pedidos pendurados: o provider desiste aos 60 s.
        .timeout(Duration::from_secs(75))
        .build()
}

/// A mensagem de um corpo de erro JSON. Aceita o formato OpenAI/OpenRouter
/// (`{"error": {"message": …}}`) e os mais simples (`{"error": "…"}`, `{"message": "…"}`).
fn error_message(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let from = |v: &serde_json::Value| v.as_str().map(str::to_owned);
    from(&v["error"]["message"])
        .or_else(|| from(&v["error"]))
        .or_else(|| from(&v["message"]))
}

/// Traduz uma resposta de erro HTTP num `SttError` com significado.
pub fn error_from_response(status: u16, body: &[u8], model: &str) -> SttError {
    let message = error_message(body)
        .unwrap_or_else(|| String::from_utf8_lossy(body).chars().take(300).collect());
    let lower = message.to_lowercase();

    match status {
        401 => SttError::Unauthorized(message),
        403 => SttError::Forbidden(message),
        402 => SttError::InsufficientCredits(message),
        404 => SttError::ModelNotFound {
            model: model.to_owned(),
        },
        408 | 504 | 524 => SttError::ProviderTimeout,
        413 => SttError::PayloadTooLarge,
        429 => SttError::RateLimited(message),
        400 if lower.contains("model")
            && (lower.contains("not found") || lower.contains("not a valid")) =>
        {
            SttError::ModelNotFound {
                model: model.to_owned(),
            }
        }
        _ if lower.contains("timed out") || lower.contains("timeout") => SttError::ProviderTimeout,
        400 | 422 => SttError::BadRequest(message),
        _ => SttError::Http { status, message },
    }
}

#[derive(Deserialize)]
struct TranscriptionBody {
    text: String,
    #[serde(default)]
    usage: Usage,
}

/// Lê `{"text": …, "usage": {…}}`, o formato de resposta comum a OpenRouter e Groq.
pub fn parse_transcription(body: &[u8]) -> Result<(String, Usage), SttError> {
    let parsed: TranscriptionBody =
        serde_json::from_slice(body).map_err(|e| SttError::Decode(e.to_string()))?;
    Ok((parsed.text, parsed.usage))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_documented_openrouter_response() {
        let body = br#"{
            "text": "Hello, this is a test of speech-to-text transcription.",
            "usage": { "seconds": 9.2, "total_tokens": 113, "input_tokens": 83, "output_tokens": 30, "cost": 0.000508 }
        }"#;
        let (text, usage) = parse_transcription(body).unwrap();
        assert_eq!(
            text,
            "Hello, this is a test of speech-to-text transcription."
        );
        assert_eq!(
            usage,
            Usage {
                seconds: Some(9.2),
                cost: Some(0.000508),
                input_tokens: Some(83),
                output_tokens: Some(30),
                total_tokens: Some(113),
            }
        );
    }

    #[test]
    fn parses_response_without_usage_and_extra_fields() {
        let body =
            r#"{"text":"olá","x_groq":{"id":"req_1"},"usage":{"seconds":1.5,"cost":0.0001}}"#;
        let (text, usage) = parse_transcription(body.as_bytes()).unwrap();
        assert_eq!(text, "olá");
        assert_eq!(usage.seconds, Some(1.5));
        assert_eq!(usage.total_tokens, None);

        let (_, usage) = parse_transcription(br#"{"text":""}"#).unwrap();
        assert_eq!(usage, Usage::default());
    }

    #[test]
    fn malformed_body_is_a_decode_error() {
        assert!(matches!(
            parse_transcription(b"<html>502</html>"),
            Err(SttError::Decode(_))
        ));
        assert!(matches!(
            parse_transcription(br#"{"usage":{}}"#),
            Err(SttError::Decode(_))
        ));
    }

    #[test]
    fn maps_status_codes_to_errors() {
        let body = br#"{"error":{"code":401,"message":"No auth credentials found"}}"#;
        assert!(
            matches!(error_from_response(401, body, "m"), SttError::Unauthorized(m) if m == "No auth credentials found")
        );
        assert!(matches!(
            error_from_response(403, b"{}", "m"),
            SttError::Forbidden(_)
        ));
        assert!(matches!(
            error_from_response(402, b"{}", "m"),
            SttError::InsufficientCredits(_)
        ));
        assert!(
            matches!(error_from_response(404, b"", "x/y"), SttError::ModelNotFound { model } if model == "x/y")
        );
        assert!(matches!(
            error_from_response(413, b"", "m"),
            SttError::PayloadTooLarge
        ));
        assert!(
            matches!(error_from_response(429, b"slow down", "m"), SttError::RateLimited(m) if m == "slow down")
        );
        assert!(matches!(
            error_from_response(524, b"", "m"),
            SttError::ProviderTimeout
        ));
        assert!(matches!(
            error_from_response(500, b"boom", "m"),
            SttError::Http { status: 500, .. }
        ));
    }

    #[test]
    fn recognises_timeouts_and_bad_models_in_messages() {
        let body = br#"{"error":{"message":"Upstream request timed out after 60s"}}"#;
        assert!(matches!(
            error_from_response(502, body, "m"),
            SttError::ProviderTimeout
        ));

        let body = br#"{"error":{"message":"chirp-3 is not a valid model ID"}}"#;
        assert!(matches!(
            error_from_response(400, body, "chirp-3"),
            SttError::ModelNotFound { .. }
        ));

        let body = br#"{"error":{"message":"response_format 'text' is not supported"}}"#;
        assert!(matches!(
            error_from_response(400, body, "m"),
            SttError::BadRequest(_)
        ));
    }
}

#[cfg(test)]
mod keyterm_tests {
    use super::keyterms;
    use crate::text::Replacement;

    fn r(from: &str, to: &str) -> Replacement {
        Replacement {
            from: from.into(),
            to: to.into(),
        }
    }

    #[test]
    fn key_terms_come_from_the_written_form_without_repeats_or_oversize() {
        let long = "x".repeat(51);
        let dictionary = [
            r("tori, tauri", "Tauri"),
            r("", "Ditok"),
            r("outra", "Tauri"),
            r("vazio", "  "),
            r("longo", &long),
        ];
        assert_eq!(keyterms(&dictionary), ["Tauri", "Ditok"]);
    }

    #[test]
    fn key_terms_stop_at_one_hundred() {
        let dictionary: Vec<Replacement> = (0..150).map(|i| r("", &format!("termo{i}"))).collect();
        assert_eq!(keyterms(&dictionary).len(), 100);
    }
}
