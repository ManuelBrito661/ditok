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
    sync::{Arc, Mutex, OnceLock, PoisonError},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
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

/// A xAI recusou o `stream_options` (que serve para a resposta trazer o custo).
fn is_stream_options_refusal(e: &ApiError) -> bool {
    matches!(e, ApiError::BadRequest(m) if m.to_lowercase().contains("stream_options")
        || m.to_lowercase().contains("stream options"))
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

// ---------------------------------------------------------------- conversas em streaming

/// Quem disse uma mensagem de uma conversa. No JSON: "user" ou "assistant".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    fn api(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// Uma imagem anexada a uma mensagem (uma captura do ecrã): PNG ou JPEG já codificado. Os
/// bytes são partilhados, para a conversa a poder enviar a cada pedido sem os copiar.
#[derive(Debug, Clone)]
pub struct Image {
    pub mime: &'static str,
    pub data: Arc<Vec<u8>>,
}

impl Image {
    /// `data:image/png;base64,…`: a forma de mandar uma imagem dentro do próprio pedido.
    fn data_url(&self) -> String {
        format!(
            "data:{};base64,{}",
            self.mime,
            STANDARD.encode(self.data.as_slice())
        )
    }
}

/// Uma mensagem de uma conversa, tal como vai ao modelo.
#[derive(Debug, Clone)]
pub struct Turn {
    pub role: Role,
    pub content: String,
    /// Imagens que acompanham a mensagem (só as do utilizador).
    pub images: Vec<Image>,
}

/// Um pedido de conversa: o prompt de sistema e as mensagens até agora (a última é
/// do utilizador). A resposta vem a cair, bocado a bocado.
pub struct ChatStream<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub turns: &'a [Turn],
    pub temperature: f32,
    pub max_tokens: u32,
    pub timeout: Duration,
    /// Como em `Chat::purpose`.
    pub purpose: &'static str,
}

#[derive(Serialize)]
struct StreamRequest<'a> {
    model: &'a str,
    messages: Vec<StreamMessage<'a>>,
    temperature: f32,
    max_completion_tokens: u32,
    stream: bool,
    /// Sem isto o último bocado não traz o `usage` (e, com ele, o custo).
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct StreamMessage<'a> {
    role: &'a str,
    content: Content<'a>,
}

/// O conteúdo de uma mensagem: texto simples, ou, com imagens, uma lista de partes (o
/// formato do `/chat/completions` para modelos que veem).
#[derive(Serialize)]
#[serde(untagged)]
enum Content<'a> {
    Text(&'a str),
    Parts(Vec<Part<'a>>),
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Part<'a> {
    Text { text: &'a str },
    ImageUrl { image_url: ImageUrl<'a> },
}

#[derive(Serialize)]
struct ImageUrl<'a> {
    url: &'a str,
    /// "high": o texto pequeno de uma interface só se lê com o detalhe todo.
    detail: &'static str,
}

/// O prompt de sistema e as mensagens da conversa; `urls[i]` são as imagens de `turns[i]`.
fn stream_messages<'a>(
    system: &'a str,
    turns: &'a [Turn],
    urls: &'a [Vec<String>],
) -> Vec<StreamMessage<'a>> {
    let mut messages = vec![StreamMessage {
        role: "system",
        content: Content::Text(system),
    }];
    messages.extend(turns.iter().zip(urls).map(|(t, urls)| StreamMessage {
        role: t.role.api(),
        content: content(&t.content, urls),
    }));
    messages
}

/// As imagens primeiro e o texto a seguir (a xAI aceita qualquer ordem).
fn content<'a>(text: &'a str, urls: &'a [String]) -> Content<'a> {
    if urls.is_empty() {
        return Content::Text(text);
    }
    let mut parts: Vec<Part<'a>> = urls
        .iter()
        .map(|url| Part::ImageUrl {
            image_url: ImageUrl {
                url,
                detail: "high",
            },
        })
        .collect();
    parts.push(Part::Text { text });
    Content::Parts(parts)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
    usage: Option<Usage>,
    /// Alguns servidores avisam de um erro a meio da resposta com um bocado só disto.
    error: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StreamChoice {
    delta: StreamDelta,
    finish_reason: Option<String>,
}

/// Os modelos que raciocinam mandam também `reasoning_content`: é ignorado (não é
/// parte da resposta).
#[derive(Deserialize, Default)]
#[serde(default)]
struct StreamDelta {
    content: Option<String>,
}

/// Parte o que chega da rede em linhas. Um bocado pode acabar a meio de uma linha
/// (ou de um carácter UTF-8), por isso guarda-se o resto até chegar a mudança de linha.
#[derive(Default)]
struct SseLines {
    buf: Vec<u8>,
}

impl SseLines {
    /// Acrescenta `chunk` e devolve o conteúdo das linhas `data:` que ficaram completas.
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(end) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=end).collect();
            out.extend(data_of(&line));
        }
        out
    }

    /// A resposta acabou: uma última linha sem mudança de linha também conta.
    fn finish(&mut self) -> Option<String> {
        let line = std::mem::take(&mut self.buf);
        data_of(&line)
    }
}

/// `data: {...}` → `{...}`. Comentários (`: keep-alive`), outros campos e linhas
/// vazias dão `None`.
fn data_of(line: &[u8]) -> Option<String> {
    let line = String::from_utf8_lossy(line);
    let rest = line.trim_end_matches(['\r', '\n']).strip_prefix("data:")?;
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// O que se foi juntando de uma resposta em streaming.
#[derive(Default)]
struct StreamState {
    lines: SseLines,
    text: String,
    cost: Option<f64>,
    finish_reason: Option<String>,
    /// Chegou o `[DONE]`.
    done: bool,
}

impl StreamState {
    /// Processa o que chegou da rede; `on_text` recebe cada bocado de texto novo.
    fn feed(&mut self, chunk: &[u8], on_text: &mut impl FnMut(&str)) -> Result<(), ApiError> {
        for data in self.lines.push(chunk) {
            self.event(&data, on_text)?;
        }
        Ok(())
    }

    fn event(&mut self, data: &str, on_text: &mut impl FnMut(&str)) -> Result<(), ApiError> {
        if self.done {
            return Ok(());
        }
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let chunk: StreamChunk = parse_json(data.as_bytes())?;
        if let Some(error) = chunk.error {
            let message = error["message"]
                .as_str()
                .or_else(|| error.as_str())
                .unwrap_or("erro durante a resposta");
            return Err(ApiError::Decode(message.to_owned()));
        }
        if let Some(usage) = chunk.usage {
            self.cost = usage.cost_in_usd_ticks.map(|t| t / TICKS_PER_USD);
        }
        for choice in chunk.choices {
            if let Some(text) = choice.delta.content.filter(|t| !t.is_empty()) {
                self.text.push_str(&text);
                on_text(&text);
            }
            if choice.finish_reason.is_some() {
                self.finish_reason = choice.finish_reason;
            }
        }
        Ok(())
    }

    /// O fim da resposta. Sem texto é um erro, com o `finish_reason` (diz porquê).
    fn finish(mut self, purpose: &str, on_text: &mut impl FnMut(&str)) -> Result<Answer, ApiError> {
        if let Some(data) = self.lines.finish() {
            self.event(&data, on_text)?;
        }
        tracing::debug!(
            purpose,
            chars = self.text.chars().count(),
            finish_reason = self.finish_reason.as_deref(),
            "chat em streaming"
        );
        let text = self.text.trim();
        if text.is_empty() {
            let reason = self.finish_reason.as_deref().unwrap_or("?");
            return Err(ApiError::Decode(format!(
                "resposta sem texto (finish_reason={reason})"
            )));
        }
        Ok(Answer {
            text: text.to_owned(),
            cost: self.cost,
        })
    }
}

impl Xai {
    /// Como `chat`, mas com a conversa toda e a resposta em streaming: `on_text` é
    /// chamado com cada bocado de texto assim que chega. Devolve a resposta completa e
    /// o custo. Se a ligação cair a meio, o erro vem sem a resposta: quem chama já
    /// recebeu o que chegou em `on_text`.
    pub async fn chat_stream(
        &self,
        req: &ChatStream<'_>,
        mut on_text: impl FnMut(&str),
    ) -> Result<Answer, ApiError> {
        let mut effort = reasoning_effort(req.model);
        let mut usage_option = true;
        // Codificar uma captura em base64 custa; faz-se uma vez, mesmo que o pedido se repita.
        let urls: Vec<Vec<String>> = req
            .turns
            .iter()
            .map(|t| t.images.iter().map(Image::data_url).collect())
            .collect();
        loop {
            let body = StreamRequest {
                model: req.model,
                messages: stream_messages(req.system, req.turns, &urls),
                temperature: req.temperature,
                max_completion_tokens: req.max_tokens
                    + if effort.is_some() {
                        REASONING_HEADROOM
                    } else {
                        0
                    },
                stream: true,
                stream_options: usage_option.then_some(StreamOptions {
                    include_usage: true,
                }),
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
            if !status.is_success() {
                let bytes = response.bytes().await?;
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
                if usage_option && is_stream_options_refusal(&error) {
                    tracing::info!("a xAI não aceita stream_options; a repetir sem ele");
                    usage_option = false;
                    continue;
                }
                return Err(error);
            }
            let mut state = StreamState::default();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                state.feed(&chunk?, &mut on_text)?;
                if state.done {
                    break;
                }
            }
            return state.finish(req.purpose, &mut on_text);
        }
    }
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

    /// Corre `chunks` pelo parser e devolve o texto de cada `on_text` e a resposta.
    fn stream(chunks: &[&[u8]]) -> (Vec<String>, Result<Answer, ApiError>) {
        let mut pieces = Vec::new();
        let mut state = StreamState::default();
        let mut on_text = |t: &str| pieces.push(t.to_owned());
        for chunk in chunks {
            if let Err(e) = state.feed(chunk, &mut on_text) {
                return (pieces, Err(e));
            }
            if state.done {
                break;
            }
        }
        let answer = state.finish("t", &mut on_text);
        (pieces, answer)
    }

    const HELLO: &str = r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":"Olá"},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"content":", mundo"},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: {"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":3,"cost_in_usd_ticks":37756000}}

data: [DONE]

"#;

    #[test]
    fn a_stream_is_assembled_with_its_cost() {
        let (pieces, answer) = stream(&[HELLO.as_bytes()]);
        assert_eq!(pieces, ["Olá", ", mundo"]);
        let answer = answer.unwrap();
        assert_eq!(answer.text, "Olá, mundo");
        assert!((answer.cost.unwrap() - 0.0037756).abs() < 1e-12);
    }

    #[test]
    fn chunks_may_end_anywhere() {
        // O mesmo fluxo, byte a byte: o resultado não pode mudar.
        let bytes: Vec<&[u8]> = HELLO.as_bytes().chunks(1).collect();
        let (pieces, answer) = stream(&bytes);
        assert_eq!(pieces, ["Olá", ", mundo"]);
        assert_eq!(answer.unwrap().text, "Olá, mundo");
    }

    #[test]
    fn a_character_split_between_chunks_survives() {
        // "ã" são dois bytes; o corte cai no meio deles.
        let line = "data: {\"choices\":[{\"delta\":{\"content\":\"não\"}}]}\n\n".as_bytes();
        let cut = line.iter().position(|&b| b == 0xC3).unwrap() + 1;
        let (pieces, answer) = stream(&[&line[..cut], &line[cut..]]);
        assert_eq!(pieces, ["não"]);
        assert_eq!(answer.unwrap().text, "não");
    }

    #[test]
    fn comments_crlf_and_a_last_line_without_newline_are_handled() {
        let body = ": keep-alive\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\r\n\r\ndata:{\"choices\":[{\"delta\":{\"content\":\"b\"}}]}";
        let (pieces, answer) = stream(&[body.as_bytes()]);
        assert_eq!(pieces, ["a", "b"]);
        assert_eq!(answer.unwrap().text, "ab");
    }

    #[test]
    fn nothing_after_done_counts() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\ndata: [DONE]\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"y\"}}]}\n\n";
        let (pieces, answer) = stream(&[body.as_bytes()]);
        assert_eq!(pieces, ["x"]);
        assert_eq!(answer.unwrap().text, "x");
    }

    #[test]
    fn reasoning_content_and_empty_deltas_are_not_text() {
        let body = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hmm\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"Ok\"}}]}\n\n";
        let (pieces, answer) = stream(&[body.as_bytes()]);
        assert_eq!(pieces, ["Ok"]);
        assert_eq!(answer.unwrap().text, "Ok");
    }

    #[test]
    fn a_stream_without_usage_has_no_cost() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\ndata: [DONE]\n\n";
        let answer = stream(&[body.as_bytes()]).1.unwrap();
        assert_eq!(answer.cost, None);
    }

    #[test]
    fn an_empty_stream_says_why() {
        let body =
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n";
        let err = stream(&[body.as_bytes()]).1.err().unwrap();
        assert!(err.to_string().contains("finish_reason=length"), "{err}");
        assert!(stream(&[]).1.is_err());
    }

    #[test]
    fn an_error_in_the_middle_of_the_stream_is_reported() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Olá\"}}]}\n\ndata: {\"error\":{\"message\":\"servidor ocupado\"}}\n\n";
        let (pieces, answer) = stream(&[body.as_bytes()]);
        // O que já tinha chegado foi entregue: quem chama fica com o parcial.
        assert_eq!(pieces, ["Olá"]);
        let err = answer.err().unwrap();
        assert!(err.to_string().contains("servidor ocupado"), "{err}");
    }

    #[test]
    fn garbage_is_a_decode_error() {
        let (_, answer) = stream(&[b"data: isto nao e json\n\n"]);
        assert!(matches!(answer, Err(ApiError::Decode(_))));
    }

    #[test]
    fn the_stream_request_has_the_expected_shape() {
        let turns = [
            Turn {
                role: Role::User,
                content: "u1".into(),
                images: Vec::new(),
            },
            Turn {
                role: Role::Assistant,
                content: "a1".into(),
                images: Vec::new(),
            },
        ];
        let urls = vec![Vec::new(), Vec::new()];
        let messages = stream_messages("s", &turns, &urls);
        let body = StreamRequest {
            model: "grok-4.20-0309-non-reasoning",
            messages,
            temperature: 0.3,
            max_completion_tokens: 100,
            stream: true,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            reasoning_effort: None,
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["stream"], true);
        assert_eq!(v["stream_options"]["include_usage"], true);
        let roles: Vec<_> = v["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["system", "user", "assistant"]);
        assert!(v.get("reasoning_effort").is_none());
        let without = StreamRequest {
            stream_options: None,
            ..body
        };
        assert!(
            serde_json::to_value(&without)
                .unwrap()
                .get("stream_options")
                .is_none()
        );
    }

    fn capture() -> Image {
        Image {
            mime: "image/png",
            data: Arc::new(vec![0x89, b'P', b'N', b'G']),
        }
    }

    #[test]
    fn text_only_messages_keep_a_plain_string() {
        let turns = [Turn {
            role: Role::User,
            content: "olá".into(),
            images: Vec::new(),
        }];
        let urls = vec![Vec::new()];
        let v = serde_json::to_value(stream_messages("s", &turns, &urls)).unwrap();
        assert_eq!(v[0]["content"], "s");
        assert_eq!(v[1]["content"], "olá", "sem imagens, o formato de sempre");
    }

    #[test]
    fn a_capture_goes_as_an_image_part_before_the_text() {
        let turns = [Turn {
            role: Role::User,
            content: "<instrucao>o que é isto?</instrucao>".into(),
            images: vec![capture()],
        }];
        let urls: Vec<Vec<String>> = turns
            .iter()
            .map(|t| t.images.iter().map(Image::data_url).collect())
            .collect();
        let v = serde_json::to_value(stream_messages("s", &turns, &urls)).unwrap();
        let parts = v[1]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "image_url");
        assert_eq!(parts[0]["image_url"]["detail"], "high");
        assert_eq!(
            parts[0]["image_url"]["url"],
            "data:image/png;base64,iVBORw=="
        );
        assert_eq!(parts[1]["type"], "text");
        assert_eq!(parts[1]["text"], "<instrucao>o que é isto?</instrucao>");
        // O prompt de sistema continua a ser texto simples.
        assert_eq!(v[0]["content"], "s");
    }

    #[test]
    fn a_refusal_of_stream_options_is_recognised() {
        assert!(is_stream_options_refusal(&ApiError::BadRequest(
            "Unknown field: stream_options".into()
        )));
        assert!(!is_stream_options_refusal(&ApiError::BadRequest(
            "outra coisa".into()
        )));
        assert!(!is_stream_options_refusal(&ApiError::Unauthorized(
            "stream_options".into()
        )));
    }

    /// Pedido real à xAI com a chave do cofre (custa uma fração de cêntimo): confirma que
    /// a resposta chega aos bocados, que o custo vem no fim e que uma conversa com
    /// mensagens anteriores é percebida. `cargo test live_chat_stream -- --ignored --nocapture`;
    /// modelo em DITOK_CLEANUP_MODEL.
    #[tokio::test]
    #[ignore]
    async fn live_chat_stream() {
        let key = crate::secrets::get().unwrap().expect("sem chave no cofre");
        let model = std::env::var("DITOK_CLEANUP_MODEL")
            .unwrap_or_else(|_| crate::xai::DEFAULT_CHAT_MODEL.into());
        let xai = Xai::new(crate::xai::http_client().unwrap(), key);
        let turns = [
            Turn {
                role: Role::User,
                content: "<instrucao>escreve uma saudação curta ao Pedro</instrucao>".into(),
                images: Vec::new(),
            },
            Turn {
                role: Role::Assistant,
                content: "Olá, Pedro! Espero que estejas bem.".into(),
                images: Vec::new(),
            },
            Turn {
                role: Role::User,
                content: "<instrucao>agora em inglês</instrucao>".into(),
                images: Vec::new(),
            },
        ];
        let mut pieces = 0;
        let started = std::time::Instant::now();
        let mut first = None;
        let answer = xai
            .chat_stream(
                &ChatStream {
                    model: &model,
                    system: "Respondes só com o texto pedido, sem comentários.",
                    turns: &turns,
                    temperature: 0.3,
                    max_tokens: 256,
                    timeout: Duration::from_secs(30),
                    purpose: "ditok-live-test",
                },
                |_| {
                    pieces += 1;
                    first.get_or_insert_with(|| started.elapsed());
                },
            )
            .await
            .unwrap();
        println!(
            "{:?}\n{pieces} bocados, o primeiro ao fim de {:?}, tudo em {:?}, custo {:?}",
            answer.text,
            first,
            started.elapsed(),
            answer.cost
        );
        assert!(pieces > 1, "devia chegar aos bocados");
        assert!(answer.cost.is_some(), "devia trazer o custo");
        assert!(
            answer.text.to_lowercase().contains("hello"),
            "{}",
            answer.text
        );
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

/// `chat_stream` de ponta a ponta, contra um servidor HTTP local em vez da xAI: o que os
/// testes do parser não apanham (ler o corpo aos bocados, repetir o pedido sem um campo
/// que a xAI recusa, erros, uma ligação cortada a meio).
#[cfg(test)]
mod http_tests {
    use std::sync::{Arc, Mutex};

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    const HEAD_SSE: &str =
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";

    /// O que o servidor viu de um pedido: os cabeçalhos (em minúsculas) e o corpo JSON.
    struct Seen {
        head: String,
        body: serde_json::Value,
    }

    /// Cada item de `script` é uma ligação: uma lista de (texto, atraso em ms antes de o
    /// escrever). No fim de cada uma o servidor fecha a ligação.
    async fn fake_server(script: Vec<Vec<(String, u64)>>) -> (Xai, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            for writes in script {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (body_at, len) = loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "o cliente fechou antes de acabar o pedido");
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (end + 4, len);
                    }
                };
                while buf.len() < body_at + len {
                    let n = socket.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                }
                log.lock().unwrap().push(Seen {
                    head: String::from_utf8_lossy(&buf[..body_at]).to_lowercase(),
                    body: serde_json::from_slice(&buf[body_at..body_at + len]).unwrap(),
                });
                for (text, delay) in writes {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    socket.write_all(text.as_bytes()).await.unwrap();
                    socket.flush().await.unwrap();
                }
                let _ = socket.shutdown().await;
            }
        });
        // O cliente da app só fala https; para o servidor local serve um normal (o
        // `http_client` instala o motor criptográfico de que o reqwest precisa).
        let _ = crate::xai::http_client();
        let client = reqwest::Client::builder().build().unwrap();
        (Xai::with_base(client, "chave-de-teste".into(), base), seen)
    }

    fn json_error(status: &str, body: &str) -> Vec<(String, u64)> {
        vec![(
            format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
            0,
        )]
    }

    fn delta(text: &str) -> String {
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\n")
    }

    /// Uma resposta que chega em três pedaços com pausas entre eles.
    fn good_stream() -> Vec<(String, u64)> {
        vec![
            (HEAD_SSE.to_owned(), 0),
            (delta("Olá"), 25),
            (delta(", mundo"), 25),
            (
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                 data: {\"choices\":[],\"usage\":{\"cost_in_usd_ticks\":37756000}}\n\n\
                 data: [DONE]\n\n"
                    .to_owned(),
                25,
            ),
        ]
    }

    fn turns() -> [Turn; 3] {
        [
            Turn {
                role: Role::User,
                content: "u1".into(),
                images: Vec::new(),
            },
            Turn {
                role: Role::Assistant,
                content: "a1".into(),
                images: Vec::new(),
            },
            Turn {
                role: Role::User,
                content: "u2".into(),
                images: Vec::new(),
            },
        ]
    }

    fn request<'a>(model: &'a str, turns: &'a [Turn]) -> ChatStream<'a> {
        ChatStream {
            model,
            system: "sistema",
            turns,
            temperature: 0.3,
            max_tokens: 100,
            timeout: Duration::from_secs(10),
            purpose: "ditok-test",
        }
    }

    #[tokio::test]
    async fn the_answer_arrives_in_pieces_over_http() {
        let (xai, seen) = fake_server(vec![good_stream()]).await;
        let turns = turns();
        let mut pieces = Vec::new();
        let answer = xai
            .chat_stream(&request("http-a-non-reasoning", &turns), |t| {
                pieces.push(t.to_owned())
            })
            .await
            .unwrap();
        assert_eq!(pieces, ["Olá", ", mundo"]);
        assert_eq!(answer.text, "Olá, mundo");
        assert!((answer.cost.unwrap() - 0.0037756).abs() < 1e-12);

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(
            seen[0].head.starts_with("post /chat/completions "),
            "{}",
            seen[0].head
        );
        assert!(
            seen[0]
                .head
                .contains("authorization: bearer chave-de-teste")
        );
        assert!(seen[0].head.contains("x-grok-conv-id: ditok-test"));
        let body = &seen[0].body;
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["model"], "http-a-non-reasoning");
        let messages = body["messages"].as_array().unwrap();
        let roles: Vec<_> = messages
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["system", "user", "assistant", "user"]);
        assert_eq!(messages[3]["content"], "u2");
        assert!(body.get("reasoning_effort").is_none(), "não raciocina");
    }

    /// Só contra um servidor local: nada vai à xAI.
    #[tokio::test]
    async fn a_capture_reaches_the_server_as_an_image_part_and_survives_a_retry() {
        let refusal = json_error(
            "400 Bad Request",
            "{\"error\":\"Model does not support parameter reasoningEffort\"}",
        );
        let (xai, seen) = fake_server(vec![refusal, good_stream()]).await;
        let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3];
        let turns = [
            Turn {
                role: Role::User,
                content: "<instrucao>o que é isto?</instrucao>".into(),
                images: vec![Image {
                    mime: "image/png",
                    data: Arc::new(png.clone()),
                }],
            },
            Turn {
                role: Role::Assistant,
                content: "Um erro de tipos.".into(),
                images: Vec::new(),
            },
            Turn {
                role: Role::User,
                content: "<instrucao>e como se corrige?</instrucao>".into(),
                images: Vec::new(),
            },
        ];
        let answer = xai
            .chat_stream(&request("http-d-grok-reasoner", &turns), |_| {})
            .await
            .unwrap();
        assert_eq!(answer.text, "Olá, mundo");

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "o pedido repetiu-se sem o esforço");
        let expected_url = format!("data:image/png;base64,{}", STANDARD.encode(&png));
        for request in seen.iter() {
            let messages = request.body["messages"].as_array().unwrap();
            assert_eq!(messages[0]["content"], "sistema");
            let parts = messages[1]["content"].as_array().unwrap();
            assert_eq!(parts[0]["type"], "image_url");
            assert_eq!(parts[0]["image_url"]["url"], expected_url.as_str());
            assert_eq!(parts[0]["image_url"]["detail"], "high");
            assert_eq!(parts[1]["text"], "<instrucao>o que é isto?</instrucao>");
            // As mensagens seguintes continuam a ser texto simples.
            assert_eq!(messages[2]["content"], "Um erro de tipos.");
            assert_eq!(
                messages[3]["content"],
                "<instrucao>e como se corrige?</instrucao>"
            );
        }
    }

    #[tokio::test]
    async fn a_reasoning_model_gets_the_effort_and_retries_without_it_when_refused() {
        let refusal = json_error(
            "400 Bad Request",
            "{\"error\":\"Model does not support parameter reasoningEffort\"}",
        );
        let (xai, seen) = fake_server(vec![refusal, good_stream()]).await;
        let turns = turns();
        let answer = xai
            .chat_stream(&request("http-b-grok-reasoner", &turns), |_| {})
            .await
            .unwrap();
        assert_eq!(answer.text, "Olá, mundo");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].body["reasoning_effort"], "low");
        assert!(
            seen[1].body.get("reasoning_effort").is_none(),
            "repetiu sem o campo"
        );
    }

    #[tokio::test]
    async fn it_retries_without_stream_options_when_refused() {
        let refusal = json_error(
            "400 Bad Request",
            "{\"error\":\"Unknown field: stream_options\"}",
        );
        let (xai, seen) = fake_server(vec![refusal, good_stream()]).await;
        let turns = turns();
        let answer = xai
            .chat_stream(&request("http-c-non-reasoning", &turns), |_| {})
            .await
            .unwrap();
        assert_eq!(answer.text, "Olá, mundo");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[0].body.get("stream_options").is_some());
        assert!(
            seen[1].body.get("stream_options").is_none(),
            "repetiu sem o campo"
        );
    }

    #[tokio::test]
    async fn http_errors_are_translated() {
        let denied = json_error("401 Unauthorized", "{\"error\":\"Incorrect API key\"}");
        let (xai, seen) = fake_server(vec![denied]).await;
        let turns = turns();
        let mut pieces = 0;
        let err = xai
            .chat_stream(&request("http-d-non-reasoning", &turns), |_| pieces += 1)
            .await
            .err()
            .unwrap();
        assert!(matches!(err, ApiError::Unauthorized(_)), "{err:?}");
        assert_eq!(pieces, 0);
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "um erro de verdade não se repete"
        );
    }

    #[tokio::test]
    async fn a_connection_cut_in_the_middle_is_an_error_after_the_text_that_arrived() {
        // Corpo em pedaços ("chunked") que acaba sem o pedaço final: a ligação caiu.
        let part = delta("Olá");
        let cut = vec![
            (
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n"
                    .to_owned(),
                0,
            ),
            (format!("{:x}\r\n{part}\r\n", part.len()), 20),
        ];
        let (xai, _) = fake_server(vec![cut]).await;
        let turns = turns();
        let mut pieces = Vec::new();
        let err = xai
            .chat_stream(&request("http-e-non-reasoning", &turns), |t| {
                pieces.push(t.to_owned())
            })
            .await
            .err()
            .unwrap();
        // Quem chama já recebeu o que chegou e mostra-o com o erro.
        assert_eq!(pieces, ["Olá"]);
        assert!(
            matches!(err, ApiError::Decode(_) | ApiError::Network(_)),
            "{err:?}"
        );
    }
}
