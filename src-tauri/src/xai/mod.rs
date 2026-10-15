//! A xAI: o único fornecedor de modelos do Ditok. Um só cliente (`Xai`) e uma só chave
//! servem tudo:
//!
//! - `stt`: transcrição de voz, em lote (REST) e em streaming (WebSocket);
//! - `chat`: texto (limpeza, comandos, resumos) em `/chat/completions`;
//! - `models`: a lista de modelos de texto, para o seletor das definições;
//! - `retry`: repetição dos pedidos que falham por pouco.
//!
//! Sem traits nem `Arc<dyn …>`: com um fornecedor só, o cliente é um tipo concreto e
//! clonar é barato (o `reqwest::Client` e a chave são `Arc`s por dentro).

pub mod chat;
#[cfg(test)]
mod live;
pub mod models;
pub mod retry;
pub mod stt;

use std::{sync::Arc, time::Duration};

use reqwest::{Client, RequestBuilder};
use serde::Deserialize;

pub const API: &str = "https://api.x.ai/v1";

/// O modelo de texto por omissão: sem raciocínio (responde logo), que é o que a limpeza
/// de um ditado precisa. Os que raciocinam gastam segundos a pensar.
pub const DEFAULT_CHAT_MODEL: &str = "grok-4.20-0309-non-reasoning";

/// A página onde se criam as chaves.
pub const CONSOLE_KEYS_URL: &str = "https://console.x.ai";

/// O cliente da xAI. `Clone` copia dois ponteiros: pode ir para quantas tarefas for preciso.
#[derive(Clone)]
pub struct Xai {
    client: Client,
    api_key: Arc<str>,
    /// Só nos testes: um servidor local no lugar da xAI (ver `with_base`).
    #[cfg(test)]
    base: Option<Arc<str>>,
}

// A chave nunca aparece num log.
impl std::fmt::Debug for Xai {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Xai").finish_non_exhaustive()
    }
}

impl Xai {
    pub fn new(client: Client, api_key: String) -> Self {
        Self {
            client,
            api_key: api_key.into(),
            #[cfg(test)]
            base: None,
        }
    }

    /// Só para testes: fala com `base` (um servidor local) em vez de com a xAI.
    #[cfg(test)]
    pub(crate) fn with_base(client: Client, api_key: String, base: String) -> Self {
        Self {
            base: Some(base.into()),
            ..Self::new(client, api_key)
        }
    }

    fn url(&self, path: &str) -> String {
        #[cfg(test)]
        if let Some(base) = &self.base {
            return format!("{base}{path}");
        }
        format!("{API}{path}")
    }

    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }

    /// `POST {API}{path}` já autenticado. Partilha o pool de ligações (HTTP/2), por
    /// isso um pedido logo a seguir a outro não paga o handshake.
    pub(crate) fn post(&self, path: &str) -> RequestBuilder {
        self.client.post(self.url(path)).bearer_auth(&self.api_key)
    }

    pub(crate) fn get(&self, path: &str) -> RequestBuilder {
        self.client.get(self.url(path)).bearer_auth(&self.api_key)
    }

    /// Confirma que a chave é aceite, sem gastar nada (`GET /v1/models`).
    pub async fn check_key(&self) -> Result<(), ApiError> {
        let response = self.get("/models").send().await?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let body = response.bytes().await?;
        Err(error_from_response(status.as_u16(), &body, ""))
    }

    /// Abre (ou reaproveita) a ligação TCP + TLS + HTTP/2 ao servidor, para que o
    /// pedido verdadeiro (a limpeza com IA, o lote do "Tentar de novo") não pague o
    /// handshake. Chamado ao premir o atalho. O streaming liga-se à parte.
    pub async fn prewarm(&self) {
        let started = std::time::Instant::now();
        match self.client.head(format!("{API}/models")).send().await {
            Ok(r) => tracing::debug!(
                status = r.status().as_u16(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "pre-warm xai"
            ),
            Err(e) => tracing::debug!(error = %ApiError::from(e), "pre-warm xai falhou"),
        }
    }
}

/// Cliente HTTP partilhado por tudo o que fala com a xAI. O `reqwest::Client` mantém
/// um pool de ligações; clonar o `Client` partilha o mesmo pool.
pub fn http_client() -> reqwest::Result<Client> {
    // O rustls precisa de um motor criptográfico. Usamos o `ring` (compila sem NASM
    // nem CMake, ao contrário do aws-lc). `Err` aqui só significa "já instalado".
    let _ = rustls::crypto::ring::default_provider().install_default();

    Client::builder()
        .user_agent(concat!("Ditok/", env!("CARGO_PKG_VERSION")))
        // Tudo o que se fala é https/wss: nunca em claro, nem por um redirecionamento.
        .https_only(true)
        // Mantém a ligação viva entre ditados próximos…
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(30))
        .http2_keep_alive_interval(Duration::from_secs(20))
        .http2_keep_alive_while_idle(true)
        .connect_timeout(Duration::from_secs(10))
        // …e corta pedidos pendurados. Os pedidos mais demorados (resumos, a separação de
        // oradores) pedem o seu próprio limite em `RequestBuilder::timeout`.
        .timeout(Duration::from_secs(75))
        .build()
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("chave da xAI rejeitada ({0}). Confirma a chave guardada nas Definições.")]
    Unauthorized(String),
    // 403: a chave existe mas o pedido foi recusado (moderação, permissões…).
    #[error("pedido recusado pela xAI ({0})")]
    Forbidden(String),
    // 403: a chave é válida mas foi criada sem acesso a este serviço (as chaves da xAI têm
    // permissões por endpoint, escolhidas ao criá-las na consola).
    #[error(
        "a chave da xAI não tem permissão para usar {0}: em console.x.ai, edita a chave e dá-lhe acesso a esse serviço (ou cria uma chave com todos)"
    )]
    NoPermission(String),
    #[error("sem créditos suficientes na conta da xAI ({0})")]
    InsufficientCredits(String),
    #[error("modelo \"{model}\" não encontrado na xAI")]
    ModelNotFound { model: String },
    #[error("demasiados pedidos; tenta daqui a pouco ({0})")]
    RateLimited(String),
    #[error("a xAI demorou demasiado a processar o pedido")]
    ProviderTimeout,
    #[error("ficheiro de áudio demasiado grande para a xAI (limite de 500 MB)")]
    PayloadTooLarge,
    #[error("pedido recusado pela xAI: {0}")]
    BadRequest(String),
    #[error("erro HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("sem resposta do servidor dentro do tempo limite")]
    ClientTimeout,
    #[error("erro de rede: {0}")]
    Network(String),
    #[error("resposta inesperada da xAI: {0}")]
    Decode(String),
}

impl From<reqwest::Error> for ApiError {
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

/// A mensagem de um corpo de erro JSON. A xAI responde `{"code": "…", "error": "…"}`;
/// aceita-se também o formato OpenAI (`{"error": {"message": …}}`) e `{"message": …}`.
fn error_message(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let from = |v: &serde_json::Value| v.as_str().map(str::to_owned);
    from(&v["error"]["message"])
        .or_else(|| from(&v["error"]))
        .or_else(|| from(&v["message"]))
}

/// Traduz uma resposta de erro HTTP num `ApiError` com significado.
pub fn error_from_response(status: u16, body: &[u8], model: &str) -> ApiError {
    let message = error_message(body)
        .unwrap_or_else(|| String::from_utf8_lossy(body).chars().take(300).collect());
    let lower = message.to_lowercase();
    // A xAI avisa que a conta não tem créditos ou chegou ao limite mensal com um 403
    // (ou um 429) cuja mensagem o diz; só o texto o distingue de uma recusa a sério.
    let out_of_money =
        lower.contains("credit") || lower.contains("spending limit") || lower.contains("balance");

    match status {
        401 => ApiError::Unauthorized(message),
        402 => ApiError::InsufficientCredits(message),
        403 | 429 if out_of_money => ApiError::InsufficientCredits(message),
        403 if lower.contains("endpoint is denied") || lower.contains("permission") => {
            ApiError::NoPermission(denied_service(&message))
        }
        403 => ApiError::Forbidden(message),
        404 => ApiError::ModelNotFound {
            model: model.to_owned(),
        },
        408 | 504 | 524 => ApiError::ProviderTimeout,
        413 => ApiError::PayloadTooLarge,
        429 => ApiError::RateLimited(message),
        // Algumas rotas da xAI respondem 400 a uma chave inválida.
        400 if lower.contains("api key") => ApiError::Unauthorized(message),
        400 if lower.contains("model")
            && (lower.contains("not found") || lower.contains("does not exist")) =>
        {
            ApiError::ModelNotFound {
                model: model.to_owned(),
            }
        }
        _ if lower.contains("timed out") || lower.contains("timeout") => ApiError::ProviderTimeout,
        400 | 422 => ApiError::BadRequest(message),
        _ => ApiError::Http { status, message },
    }
}

/// O serviço que uma mensagem de acesso negado nomeia ("Access to the chat endpoint is
/// denied…" → "o chat"). Se não for no formato esperado, uma palavra genérica.
fn denied_service(message: &str) -> String {
    let lower = message.to_lowercase();
    lower
        .split_once("access to the ")
        .and_then(|(_, rest)| rest.split_once(" endpoint"))
        .map(|(name, _)| name.trim())
        .filter(|name| !name.is_empty() && name.len() <= 30)
        .map_or_else(
            || "este serviço".to_owned(),
            |name| format!("o serviço \"{name}\""),
        )
}

/// O que uma chave deixa fazer, serviço a serviço. `None` = funciona; `Some(motivo)` = a xAI
/// recusou (ou não deu para testar).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Access {
    /// Transcrição de voz (`/v1/stt`): ditar, ficheiros, notas.
    pub transcribe: Option<String>,
    /// Texto (`/v1/chat/completions`): limpeza, comandos, resumos.
    pub text: Option<String>,
}

impl Access {
    /// Regista o resultado (sem a chave) e devolve-o, para encadear.
    pub fn logged(self) -> Self {
        match (&self.transcribe, &self.text) {
            (None, None) => tracing::info!("a chave da xAI pode transcrever e escrever texto"),
            (t, x) => {
                tracing::warn!(transcribe = ?t, text = ?x, "a chave da xAI tem acessos recusados")
            }
        }
        self
    }

    /// A chave deixa fazer tudo o que o Ditok precisa?
    pub fn is_complete(&self) -> bool {
        self.transcribe.is_none() && self.text.is_none()
    }
}

impl Xai {
    /// Pergunta à xAI se a chave pode transcrever e escrever texto. Gasta uma fração de
    /// cêntimo: 0,1 s de silêncio e uma resposta de um token. As chaves da xAI têm
    /// permissões por serviço; uma criada só para a voz transcreve mas não limpa o texto.
    pub async fn probe(&self) -> Access {
        // Só um erro de acesso conta como "recusado": um 200, uma resposta vazia ou um
        // "áudio sem fala" provam que o serviço respondeu à chave.
        let verdict = |result: Result<(), ApiError>| match result {
            Ok(()) | Err(ApiError::Decode(_) | ApiError::BadRequest(_)) => None,
            Err(e) => Some(e.to_string()),
        };
        let silence = crate::audio::wav::encode_pcm16_mono(&[0.0; 1_600], 16_000);
        let req = stt::SttRequest {
            language: None,
            keyterms: &[],
        };
        let (transcribe, text) = tokio::join!(
            async { verdict(self.transcribe(silence, &req).await.map(drop)) },
            async {
                verdict(
                    self.chat(&chat::Chat {
                        model: DEFAULT_CHAT_MODEL,
                        system: "Responde só com a palavra ok.",
                        user: "ok",
                        temperature: 0.0,
                        max_tokens: 1,
                        timeout: Duration::from_secs(15),
                        purpose: "ditok-probe",
                    })
                    .await
                    .map(drop),
                )
            }
        );
        Access { transcribe, text }
    }
}

/// Uma resposta HTTP já lida: o estado e o corpo, ou o erro que ela traduz.
pub(crate) async fn read_body(
    response: reqwest::Response,
    model: &str,
) -> Result<bytes::Bytes, ApiError> {
    let status = response.status();
    let body = response.bytes().await?;
    if status.is_success() {
        Ok(body)
    } else {
        Err(error_from_response(status.as_u16(), &body, model))
    }
}

/// Lê um corpo JSON, com o erro de leitura já traduzido.
pub(crate) fn parse_json<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| ApiError::Decode(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_status_codes_to_errors() {
        let body = br#"{"code":"x","error":"Incorrect API key provided"}"#;
        assert!(
            matches!(error_from_response(401, body, "m"), ApiError::Unauthorized(m) if m.contains("API key"))
        );
        // Algumas rotas respondem 400 a uma chave inválida.
        assert!(matches!(
            error_from_response(400, body, "m"),
            ApiError::Unauthorized(_)
        ));
        assert!(matches!(
            error_from_response(403, b"{}", "m"),
            ApiError::Forbidden(_)
        ));
        assert!(matches!(
            error_from_response(402, b"{}", "m"),
            ApiError::InsufficientCredits(_)
        ));
        assert!(
            matches!(error_from_response(404, b"", "grok-x"), ApiError::ModelNotFound { model } if model == "grok-x")
        );
        assert!(matches!(
            error_from_response(413, b"", "m"),
            ApiError::PayloadTooLarge
        ));
        assert!(
            matches!(error_from_response(429, b"slow down", "m"), ApiError::RateLimited(m) if m == "slow down")
        );
        assert!(matches!(
            error_from_response(504, b"", "m"),
            ApiError::ProviderTimeout
        ));
        assert!(matches!(
            error_from_response(500, b"boom", "m"),
            ApiError::Http { status: 500, .. }
        ));
    }

    #[test]
    fn no_credits_is_told_apart_from_a_plain_refusal() {
        let body = br#"{"code":"x","error":"Your team has either used all available credits or reached its monthly spending limit."}"#;
        assert!(matches!(
            error_from_response(403, body, "m"),
            ApiError::InsufficientCredits(_)
        ));
        assert!(matches!(
            error_from_response(429, body, "m"),
            ApiError::InsufficientCredits(_)
        ));
        let flagged = br#"{"error":"content flagged by moderation"}"#;
        assert!(matches!(
            error_from_response(403, flagged, "m"),
            ApiError::Forbidden(_)
        ));
    }

    #[test]
    fn a_key_without_access_to_a_service_says_which() {
        let body = br#"{"code":"x","error":"Access to the chat endpoint is denied. Please ensure you're using the correct credentials."}"#;
        let err = error_from_response(403, body, "m");
        assert!(
            matches!(&err, ApiError::NoPermission(s) if s == "o serviço \"chat\""),
            "{err:?}"
        );
        assert!(err.to_string().contains("console.x.ai"));
        // Fora do formato conhecido, uma palavra genérica.
        let other = br#"{"error":"You do not have permission to do that"}"#;
        assert!(
            matches!(error_from_response(403, other, "m"), ApiError::NoPermission(s) if s == "este serviço")
        );
    }

    #[test]
    fn recognises_timeouts_and_bad_models_in_messages() {
        let body = br#"{"error":{"message":"Upstream request timed out after 60s"}}"#;
        assert!(matches!(
            error_from_response(502, body, "m"),
            ApiError::ProviderTimeout
        ));
        let body = br#"{"error":"The model grok-9 does not exist"}"#;
        assert!(matches!(
            error_from_response(400, body, "grok-9"),
            ApiError::ModelNotFound { .. }
        ));
        let body = br#"{"error":"unsupported parameter"}"#;
        assert!(matches!(
            error_from_response(400, body, "m"),
            ApiError::BadRequest(_)
        ));
    }

    #[test]
    fn the_key_never_shows_up_in_debug_output() {
        let x = Xai::new(http_client().unwrap(), "xai-segredo".into());
        assert!(!format!("{x:?}").contains("segredo"));
    }
}
