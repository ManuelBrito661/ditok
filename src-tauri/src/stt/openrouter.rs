//! OpenRouter: `POST /api/v1/audio/transcriptions`.
//!
//! Dois formatos de envio (confirmados na documentação oficial):
//! - **multipart/form-data** compatível com OpenAI (por omissão): o WAV vai em bruto,
//!   sem os ~33% extra do base64.
//! - **JSON** com o áudio em base64 (`input_audio`): usado como alternativa e quando
//!   o multipart é recusado com um 400.

use std::time::Instant;

use async_trait::async_trait;
use base64::Engine as _;
use bytes::Bytes;
use reqwest::{
    Body, Client, RequestBuilder,
    multipart::{Form, Part},
};
use serde::{Deserialize, Serialize};

use super::{
    SttError, SttProvider, SttRequest, Transcription, error_from_response, parse_transcription,
};

pub const BASE_URL: &str = "https://openrouter.ai/api/v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Multipart,
    Json,
}

impl Transport {
    fn as_str(self) -> &'static str {
        match self {
            Self::Multipart => "multipart",
            Self::Json => "json",
        }
    }
}

pub struct OpenRouter {
    client: Client,
    api_key: String,
    transport: Transport,
}

// Implementação manual de `Debug` para que a chave nunca apareça num log.
impl std::fmt::Debug for OpenRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenRouter")
            .field("transport", &self.transport)
            .finish_non_exhaustive()
    }
}

impl OpenRouter {
    pub fn new(client: Client, api_key: String, transport: Transport) -> Self {
        Self {
            client,
            api_key,
            transport,
        }
    }

    fn multipart(&self, audio: Bytes, req: &SttRequest<'_>) -> Result<RequestBuilder, SttError> {
        let len = audio.len() as u64;
        // `Body::from(Bytes)` aponta para o mesmo buffer: o áudio não é copiado.
        let file = Part::stream_with_length(Body::from(audio), len)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(SttError::from)?;
        let mut form = Form::new()
            .text("model", req.model.to_owned())
            .text("response_format", "json")
            .part("file", file);
        if let Some(lang) = req.language {
            form = form.text("language", lang.to_owned());
        }
        Ok(self
            .client
            .post(format!("{BASE_URL}/audio/transcriptions"))
            .multipart(form))
    }

    fn json(&self, audio: &[u8], req: &SttRequest<'_>) -> RequestBuilder {
        let body = JsonRequest {
            model: req.model,
            input_audio: InputAudio {
                data: base64::engine::general_purpose::STANDARD.encode(audio),
                format: "wav",
            },
            language: req.language,
        };
        self.client
            .post(format!("{BASE_URL}/audio/transcriptions"))
            .json(&body)
    }

    async fn send(
        &self,
        builder: RequestBuilder,
        req: &SttRequest<'_>,
        transport: Transport,
    ) -> Result<Transcription, SttError> {
        let started = Instant::now();
        let response = builder
            .bearer_auth(&self.api_key)
            .header("X-Title", "Ditok")
            .send()
            .await?;
        let status = response.status();
        let generation_id = response
            .headers()
            .get("x-generation-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = response.bytes().await?;
        let elapsed = started.elapsed();

        if !status.is_success() {
            return Err(error_from_response(status.as_u16(), &body, req.model));
        }
        let (text, usage) = parse_transcription(&body)?;
        Ok(Transcription {
            text,
            usage,
            generation_id,
            elapsed,
            transport: transport.as_str(),
        })
    }
}

#[derive(Serialize)]
struct JsonRequest<'a> {
    model: &'a str,
    input_audio: InputAudio,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<&'a str>,
}

#[derive(Serialize)]
struct InputAudio {
    data: String,
    format: &'static str,
}

#[async_trait]
impl SttProvider for OpenRouter {
    fn name(&self) -> &'static str {
        "openrouter"
    }

    async fn prewarm(&self) {
        // Qualquer pedido ao mesmo host serve para abrir TCP + TLS + HTTP/2; o HEAD
        // não traz corpo. A resposta em si não interessa.
        let started = Instant::now();
        match self.client.head(format!("{BASE_URL}/models")).send().await {
            Ok(r) => tracing::debug!(
                status = r.status().as_u16(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "pre-warm openrouter"
            ),
            Err(e) => tracing::debug!(error = %SttError::from(e), "pre-warm openrouter falhou"),
        }
    }

    async fn transcribe(
        &self,
        audio: Bytes,
        req: &SttRequest<'_>,
    ) -> Result<Transcription, SttError> {
        match self.transport {
            Transport::Json => {
                self.send(self.json(&audio, req), req, Transport::Json)
                    .await
            }
            Transport::Multipart => {
                // `audio.clone()` só copia o ponteiro: precisamos de o manter para o
                // caso de ter de repetir em JSON.
                match self
                    .send(
                        self.multipart(audio.clone(), req)?,
                        req,
                        Transport::Multipart,
                    )
                    .await
                {
                    Err(SttError::BadRequest(msg)) => {
                        tracing::warn!(%msg, "multipart recusado; a repetir com JSON base64");
                        self.send(self.json(&audio, req), req, Transport::Json)
                            .await
                    }
                    other => other,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_request_matches_documented_shape() {
        let body = JsonRequest {
            model: "openai/whisper-large-v3-turbo",
            input_audio: InputAudio {
                data: "UklGRg==".into(),
                format: "wav",
            },
            language: Some("pt"),
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "model": "openai/whisper-large-v3-turbo",
                "input_audio": { "data": "UklGRg==", "format": "wav" },
                "language": "pt"
            })
        );

        let body = JsonRequest {
            language: None,
            ..body
        };
        assert!(
            serde_json::to_value(&body)
                .unwrap()
                .get("language")
                .is_none()
        );
    }

    #[test]
    fn transport_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&Transport::Multipart).unwrap(),
            "\"multipart\""
        );
        assert_eq!(
            serde_json::from_str::<Transport>("\"json\"").unwrap(),
            Transport::Json
        );
    }
}
