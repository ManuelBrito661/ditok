//! Texto com os modelos Grok: `POST /v1/chat/completions`. É o que a limpeza dos ditados
//! (`cleanup.rs`), o modo comando (`command.rs`) e os resumos (`summarize.rs`) usam.
//!
//! Dois cuidados que só a xAI pede:
//! - **Custo exato.** Cada resposta traz `usage.cost_in_usd_ticks` (1 USD = 10^10 ticks):
//!   o que a xAI cobrou mesmo, já com o desconto da cache de prompts. Não se estima nada.
//! - **Raciocínio.** Os modelos que raciocinam (`grok-4.5` e seguintes) não o deixam
//!   desligar; o mais baixo é `reasoning_effort: "low"`, e o `max_completion_tokens`
//!   conta os tokens de raciocínio. Por isso: sem `non-reasoning` no nome pede-se o
//!   esforço baixo e dá-se folga ao limite; se o modelo recusar o campo, repete-se sem ele.

use std::{
    collections::HashSet,
    sync::{Mutex, OnceLock, PoisonError},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use super::{ApiError, Xai, error_from_response, parse_json};

/// 1 USD = 10^10 ticks (a unidade de `cost_in_usd_ticks`).
const TICKS_PER_USD: f64 = 1e10;

/// Tokens a mais, para um modelo que raciocina não gastar o limite todo a pensar e
/// devolver uma resposta vazia.
const REASONING_HEADROOM: u32 = 2048;

/// Um pedido ao `/chat/completions`: uma mensagem de sistema e uma do utilizador.
pub struct Chat<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub user: &'a str,
    pub temperature: f32,
    pub max_tokens: u32,
    pub timeout: Duration,
    /// O que este pedido é ("cleanup", "command", "summary"). Vai no cabeçalho
    /// `x-grok-conv-id`, que encaminha pedidos parecidos para o mesmo servidor e melhora
    /// os acertos da cache de prompts (o prompt de sistema repete-se a cada ditado).
    pub purpose: &'static str,
}

/// A resposta do modelo (já sem espaços nas pontas, nunca vazia) e o que custou.
pub struct Answer {
    pub text: String,
    /// USD, tal como a xAI os cobrou; `None` se a resposta não o trouxe.
    pub cost: Option<f64>,
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    messages: [Message<'a>; 2],
    temperature: f32,
    max_completion_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct Response {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
    /// "length" quando o modelo esgotou o limite de tokens antes de terminar.
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Usage {
    prompt_tokens: u64,
    completion_tokens: u64,
    cost_in_usd_ticks: Option<f64>,
    prompt_tokens_details: PromptDetails,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PromptDetails {
    cached_tokens: u64,
}

/// Os modelos que já se viu recusarem o `reasoning_effort` (não raciocinam).
fn no_effort() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(Mutex::default)
}

/// O esforço de raciocínio a pedir a `model`: nenhum nos que dizem `non-reasoning` ou que
/// já recusaram o campo, o mais baixo nos outros.
fn reasoning_effort(model: &str) -> Option<&'static str> {
    if model.contains("non-reasoning")
        || no_effort()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(model)
    {
        None
    } else {
        Some("low")
    }
}

fn is_effort_refusal(e: &ApiError) -> bool {
    matches!(e, ApiError::BadRequest(m) if m.to_lowercase().contains("reasoning"))
}

impl Xai {
    /// Envia o pedido e devolve o texto da primeira escolha. Uma resposta vazia é um
    /// erro (com o `finish_reason`, que diz porquê).
    pub async fn chat(&self, req: &Chat<'_>) -> Result<Answer, ApiError> {
        let mut effort = reasoning_effort(req.model);
        loop {
            let body = Request {
                model: req.model,
                messages: [
                    Message {
                        role: "system",
                        content: req.system,
                    },
                    Message {
                        role: "user",
                        content: req.user,
                    },
                ],
                temperature: req.temperature,
                max_completion_tokens: req.max_tokens
                    + if effort.is_some() {
                        REASONING_HEADROOM
                    } else {
                        0
                    },
                stream: false,
                reasoning_effort: effort,
            };
            let response = self
                .post("/chat/completions")
                .header("x-grok-conv-id", req.purpose)
                .timeout(req.timeout)
                .json(&body)
                .send()
                .await?;
            let status = response.status();
            let bytes = response.bytes().await?;
            if !status.is_success() {
                let error = error_from_response(status.as_u16(), &bytes, req.model);
                if effort.is_some() && is_effort_refusal(&error) {
                    tracing::info!(
                        model = req.model,
                        "o modelo não aceita reasoning_effort; a repetir sem ele"
                    );
                    no_effort()
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(req.model.to_owned());
                    effort = None;
                    continue;
                }
                return Err(error);
            }
            return answer_from(&bytes, req.purpose);
        }
    }
}

fn answer_from(bytes: &[u8], purpose: &str) -> Result<Answer, ApiError> {
    let parsed: Response = parse_json(bytes)?;
    let choice = parsed
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::Decode("resposta sem escolhas".into()))?;
    let usage = parsed.usage.unwrap_or_default();
    tracing::debug!(
        purpose,
        prompt_tokens = usage.prompt_tokens,
        cached_tokens = usage.prompt_tokens_details.cached_tokens,
        completion_tokens = usage.completion_tokens,
        "chat"
    );
    let text = choice.message.content.unwrap_or_default();
    if text.trim().is_empty() {
        // Dizer porquê poupa uma tarde a adivinhar (foi assim que se descobriu o
        // problema do raciocínio: finish_reason=length).
        let reason = choice.finish_reason.as_deref().unwrap_or("?");
        return Err(ApiError::Decode(format!(
            "resposta sem texto (finish_reason={reason})"
        )));
    }
    Ok(Answer {
        text: text.trim().to_owned(),
        cost: usage.cost_in_usd_ticks.map(|t| t / TICKS_PER_USD),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_has_the_expected_shape() {
        let body = Request {
            model: "grok-4.20-0309-non-reasoning",
            messages: [
                Message {
                    role: "system",
                    content: "s",
                },
                Message {
                    role: "user",
                    content: "u",
                },
            ],
            temperature: 0.0,
            max_completion_tokens: 100,
            stream: false,
            reasoning_effort: None,
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["max_completion_tokens"], 100);
        assert_eq!(v["stream"], false);
        assert_eq!(v["messages"][1]["role"], "user");
        assert!(
            v.get("reasoning_effort").is_none(),
            "sem campo quando não raciocina"
        );
        let with = Request {
            reasoning_effort: Some("low"),
            ..body
        };
        assert_eq!(
            serde_json::to_value(&with).unwrap()["reasoning_effort"],
            "low"
        );
    }

    #[test]
    fn only_reasoning_models_get_an_effort() {
        assert_eq!(reasoning_effort("grok-4.20-0309-non-reasoning"), None);
        assert_eq!(reasoning_effort("grok-4.7"), Some("low"));
        // Depois de um modelo recusar o campo, não se volta a pedir.
        no_effort()
            .lock()
            .unwrap()
            .insert("grok-sem-esforco".into());
        assert_eq!(reasoning_effort("grok-sem-esforco"), None);
    }

    #[test]
    fn the_cost_comes_from_the_ticks_the_api_reports() {
        // O exemplo da documentação: 37756000 ticks = 0,0038 USD.
        let body = r#"{"choices":[{"message":{"content":" Olá. "},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":120,"completion_tokens":8,"cost_in_usd_ticks":37756000,
                     "prompt_tokens_details":{"cached_tokens":100}}}"#;
        let a = answer_from(body.as_bytes(), "t").unwrap();
        assert_eq!(a.text, "Olá.");
        assert!((a.cost.unwrap() - 0.0037756).abs() < 1e-12);
        // Sem `usage`, o custo é desconhecido (não zero).
        let bare = br#"{"choices":[{"message":{"content":"x"}}]}"#;
        assert_eq!(answer_from(bare, "t").unwrap().cost, None);
    }

    #[test]
    fn an_empty_answer_says_why() {
        let body = br#"{"choices":[{"message":{"content":""},"finish_reason":"length"}]}"#;
        let err = answer_from(body, "t").err().unwrap();
        assert!(err.to_string().contains("finish_reason=length"), "{err}");
        assert!(matches!(
            answer_from(br#"{"choices":[]}"#, "t"),
            Err(ApiError::Decode(_))
        ));
    }

    #[test]
    fn a_refusal_of_the_effort_field_is_recognised() {
        assert!(is_effort_refusal(&ApiError::BadRequest(
            "Model does not support parameter reasoningEffort".into()
        )));
        assert!(!is_effort_refusal(&ApiError::BadRequest(
            "outra coisa".into()
        )));
        assert!(!is_effort_refusal(&ApiError::Unauthorized("x".into())));
    }
}
