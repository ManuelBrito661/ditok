//! O assistente com ferramentas: `POST /v1/responses` (a Responses API da xAI). É a única
//! maneira de o modelo **pesquisar na web e no X**: são ferramentas que correm nos servidores
//! da xAI (`web_search`, `x_search`) e que o `/chat/completions` não tem (o antigo
//! `search_parameters` foi retirado). O modelo decide sozinho quando pesquisa.
//!
//! A resposta chega em streaming, em eventos com `type`: o texto, o raciocínio (quando a xAI
//! o manda), as pesquisas que o modelo está a fazer e as fontes que citou. O formato segue o
//! da OpenAI, que a xAI imita; como não está todo documentado, a leitura é tolerante: um
//! evento que não se conhece é ignorado, e as pesquisas reconhecem-se por vários nomes.
//!
//! Três redes de segurança, para a conversa nunca falhar por causa disto:
//! - um modelo que recuse o esforço de raciocínio ou as ferramentas repete sem eles (e
//!   fica lembrado, como em `chat.rs`);
//! - uma chave sem acesso à Responses API (as chaves da xAI têm permissões por serviço)
//!   passa a usar o `/chat/completions`, sem pesquisa;
//! - um 404 (rota que não existe) tenta uma vez o `/chat/completions`.
//!
//! O pedido vai com `store: false`: a conversa não fica guardada na xAI.

use std::{
    collections::HashSet,
    sync::{
        Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    ApiError, Xai,
    chat::{
        self, ChatStream, Image, REASONING_HEADROOM, Role, SseLines, TICKS_PER_USD, Turn,
        is_effort_refusal, no_effort, reasoning_effort,
    },
    error_from_response, parse_json,
};

/// Onde o modelo pesquisou.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchKind {
    Web,
    X,
}

/// Uma pesquisa que o modelo fez. `query` pode vir vazia (a xAI ainda não disse o quê).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Search {
    pub kind: SearchKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub query: String,
}

/// Uma fonte citada na resposta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// O que vai chegando. As pesquisas e as fontes vêm sempre como a lista inteira até agora
/// (uma pesquisa pode ganhar a `query` depois de ter aparecido).
#[derive(Debug)]
pub enum Piece<'a> {
    Text(&'a str),
    Reasoning(&'a str),
    Searches(&'a [Search]),
    Sources(&'a [Source]),
}

/// A resposta completa.
#[derive(Debug)]
pub struct Reply {
    /// Já sem espaços nas pontas; nunca vazio.
    pub text: String,
    /// USD, tal como a xAI os cobrou (com as pesquisas, se a xAI as incluir no custo).
    pub cost: Option<f64>,
    pub reasoning_tokens: Option<u64>,
    pub searches: Vec<Search>,
    pub sources: Vec<Source>,
    /// Usou o `/chat/completions` (sem pesquisa) em vez da Responses API.
    pub fallback: bool,
}

/// Este modelo raciocina antes de responder (a janela mostra "A pensar…").
pub fn reasons(model: &str) -> bool {
    reasoning_effort(model).is_some()
}

// ---------------------------------------------------------------- o pedido

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    instructions: &'a str,
    input: Vec<InputMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<[Tool; 2]>,
    temperature: f32,
    max_output_tokens: u32,
    stream: bool,
    store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<Effort>,
}

#[derive(Serialize)]
struct Tool {
    #[serde(rename = "type")]
    kind: &'static str,
}

const TOOLS: [Tool; 2] = [Tool { kind: "web_search" }, Tool { kind: "x_search" }];

#[derive(Serialize)]
struct Effort {
    effort: &'static str,
}

#[derive(Serialize)]
struct InputMessage<'a> {
    role: &'static str,
    content: Content<'a>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Content<'a> {
    Text(&'a str),
    Parts(Vec<Part<'a>>),
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Part<'a> {
    InputImage {
        image_url: &'a str,
        /// "high": o texto pequeno de uma interface só se lê com o detalhe todo.
        detail: &'static str,
    },
    InputText {
        text: &'a str,
    },
}

/// As mensagens da conversa; `urls[i]` são as imagens de `turns[i]` (primeiro as imagens).
fn input<'a>(turns: &'a [Turn], urls: &'a [Vec<String>]) -> Vec<InputMessage<'a>> {
    turns
        .iter()
        .zip(urls)
        .map(|(t, urls)| InputMessage {
            role: t.role.api(),
            content: if urls.is_empty() || t.role != Role::User {
                Content::Text(&t.content)
            } else {
                let mut parts: Vec<Part<'a>> = urls
                    .iter()
                    .map(|url| Part::InputImage {
                        image_url: url,
                        detail: "high",
                    })
                    .collect();
                parts.push(Part::InputText { text: &t.content });
                Content::Parts(parts)
            },
        })
        .collect()
}

// ---------------------------------------------------------------- lembranças

/// Os modelos que já recusaram as ferramentas.
fn no_tools() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(Mutex::default)
}

fn remember(set: &Mutex<HashSet<String>>, model: &str) {
    set.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(model.to_owned());
}

fn knows(set: &Mutex<HashSet<String>>, model: &str) -> bool {
    set.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(model)
}

/// A chave não tem acesso à Responses API: usa-se o chat até a chave mudar
/// (`forget_no_responses`) ou a app reiniciar.
static NO_RESPONSES: AtomicBool = AtomicBool::new(false);

/// Uma chave nova pode ter o acesso que a antiga não tinha.
pub fn forget_no_responses() {
    NO_RESPONSES.store(false, Ordering::Relaxed);
}

fn is_tools_refusal(e: &ApiError) -> bool {
    matches!(e, ApiError::BadRequest(m) if {
        let m = m.to_lowercase();
        m.contains("tool") || m.contains("web_search") || m.contains("x_search")
    })
}

// ---------------------------------------------------------------- o que chega

/// Um item de saída que é uma chamada a uma ferramenta de pesquisa? Devolve onde pesquisou.
/// A xAI pode chamá-los `web_search_call`/`x_search_call`, ou uma chamada genérica com o nome
/// da ferramenta (`x_keyword_search`, `browse_page`…).
fn search_kind(item: &Value) -> Option<SearchKind> {
    let kind = item["type"].as_str()?;
    if !kind.ends_with("_call") {
        return None;
    }
    let name = item["name"].as_str().unwrap_or("");
    let label = format!("{kind} {name}").to_lowercase();
    let is_x = label.starts_with("x_")
        || label.contains(" x_")
        || ["x_search", "x_keyword", "x_semantic", "x_user", "x_thread"]
            .iter()
            .any(|w| label.contains(w));
    Some(if is_x { SearchKind::X } else { SearchKind::Web })
}

/// O que se pesquisou: em `action.query` (formato OpenAI), `query`, ou nos argumentos da
/// chamada (texto JSON ou objeto). Um endereço aberto também conta.
fn search_query(item: &Value) -> String {
    let pick = |v: &Value| -> Option<String> {
        ["query", "keywords", "url"]
            .iter()
            .find_map(|k| v[*k].as_str().map(str::to_owned))
            .or_else(|| v["queries"][0].as_str().map(str::to_owned))
    };
    let arguments = match &item["arguments"] {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        other => other.clone(),
    };
    pick(&item["action"])
        .or_else(|| pick(item))
        .or_else(|| pick(&arguments))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// Uma fonte a partir de uma anotação (`url_citation`), de um texto (só o endereço) ou de um
/// objeto com `url`. Só endereços http(s).
fn source_of(v: &Value) -> Option<Source> {
    let (url, title) = match v {
        Value::String(url) => (url.as_str(), None),
        _ => (
            v["url"].as_str()?,
            v["title"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_owned),
        ),
    };
    let url = url.trim();
    (url.starts_with("https://") || url.starts_with("http://")).then(|| Source {
        url: url.to_owned(),
        title,
    })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cost_in_usd_ticks: Option<f64>,
    output_tokens_details: OutputDetails,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OutputDetails {
    reasoning_tokens: Option<u64>,
}

#[derive(Default)]
struct State {
    lines: SseLines,
    text: String,
    /// As pesquisas, com o id do item (para uma pesquisa que ganha a query mais tarde).
    searches: Vec<(String, Search)>,
    sources: Vec<Source>,
    cost: Option<f64>,
    reasoning_tokens: Option<u64>,
    /// Porque acabou sem texto ("max_output_tokens"…).
    incomplete: Option<String>,
    done: bool,
}

impl State {
    fn feed(&mut self, chunk: &[u8], on: &mut impl FnMut(Piece<'_>)) -> Result<(), ApiError> {
        for data in self.lines.push(chunk) {
            self.event(&data, on)?;
        }
        Ok(())
    }

    fn event(&mut self, data: &str, on: &mut impl FnMut(Piece<'_>)) -> Result<(), ApiError> {
        if self.done || data.trim() == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let v: Value = parse_json(data.as_bytes())?;
        let kind = v["type"].as_str().unwrap_or("");
        match kind {
            "response.output_text.delta" => {
                if let Some(delta) = v["delta"].as_str().filter(|d| !d.is_empty()) {
                    self.text.push_str(delta);
                    on(Piece::Text(delta));
                }
            }
            k if k.starts_with("response.reasoning") && k.ends_with(".delta") => {
                if let Some(delta) = v["delta"].as_str().filter(|d| !d.is_empty()) {
                    on(Piece::Reasoning(delta));
                }
            }
            "response.output_item.added" | "response.output_item.done" => {
                self.item(&v["item"], on);
            }
            "response.output_text.annotation.added" => {
                if let Some(source) = source_of(&v["annotation"]) {
                    self.add_sources([source], on);
                }
            }
            "response.completed" | "response.incomplete" => {
                self.completed(&v["response"], on);
                self.done = true;
            }
            "response.failed" | "error" => {
                let message = v["response"]["error"]["message"]
                    .as_str()
                    .or_else(|| v["error"]["message"].as_str())
                    .or_else(|| v["message"].as_str())
                    .unwrap_or("erro durante a resposta");
                return Err(ApiError::Decode(message.to_owned()));
            }
            _ => {}
        }
        Ok(())
    }

    /// Um item de saída: se for uma pesquisa, junta-a (ou completa a que já lá estava).
    fn item(&mut self, item: &Value, on: &mut impl FnMut(Piece<'_>)) {
        let Some(kind) = search_kind(item) else {
            return;
        };
        let id = item["id"].as_str().unwrap_or("").to_owned();
        let query = search_query(item);
        let known = self
            .searches
            .iter_mut()
            .find(|(i, _)| !id.is_empty() && *i == id);
        match known {
            Some((_, search)) if search.query.is_empty() && !query.is_empty() => {
                search.query = query;
            }
            Some(_) => return,
            None => self.searches.push((id, Search { kind, query })),
        }
        let list = self.search_list();
        on(Piece::Searches(&list));
    }

    fn search_list(&self) -> Vec<Search> {
        self.searches.iter().map(|(_, s)| s.clone()).collect()
    }

    fn add_sources(
        &mut self,
        sources: impl IntoIterator<Item = Source>,
        on: &mut impl FnMut(Piece<'_>),
    ) {
        let before = self.sources.len();
        for source in sources {
            match self.sources.iter_mut().find(|s| s.url == source.url) {
                Some(known) if known.title.is_none() => known.title = source.title,
                Some(_) => {}
                None => self.sources.push(source),
            }
        }
        if self.sources.len() != before {
            on(Piece::Sources(&self.sources));
        }
    }

    /// O fim: o custo, os tokens de raciocínio, e o que só vem no resumo final (as fontes em
    /// `citations`, as anotações e as pesquisas que não chegaram por evento, o texto se não
    /// veio aos bocados).
    fn completed(&mut self, response: &Value, on: &mut impl FnMut(Piece<'_>)) {
        let usage: Usage = serde_json::from_value(response["usage"].clone()).unwrap_or_default();
        tracing::debug!(
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            reasoning_tokens = usage.output_tokens_details.reasoning_tokens,
            server_side_tools = %response["usage"]["server_side_tool_usage"],
            "responses: uso"
        );
        self.cost = usage.cost_in_usd_ticks.map(|t| t / TICKS_PER_USD);
        self.reasoning_tokens = usage.output_tokens_details.reasoning_tokens;
        if response["status"].as_str() == Some("incomplete") {
            self.incomplete = response["incomplete_details"]["reason"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| Some("incomplete".to_owned()));
        }

        let mut text = String::new();
        let mut found = Vec::new();
        for item in response["output"].as_array().into_iter().flatten() {
            if search_kind(item).is_some() {
                self.item(item, on);
                continue;
            }
            for part in item["content"].as_array().into_iter().flatten() {
                if part["type"].as_str() == Some("output_text") {
                    text.push_str(part["text"].as_str().unwrap_or(""));
                    found.extend(
                        part["annotations"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(source_of),
                    );
                }
            }
        }
        found.extend(
            response["citations"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(source_of),
        );
        self.add_sources(found, on);
        if self.text.trim().is_empty() && !text.trim().is_empty() {
            self.text = text;
            on(Piece::Text(&self.text));
        }
    }

    fn finish(mut self, on: &mut impl FnMut(Piece<'_>)) -> Result<Reply, ApiError> {
        if let Some(data) = self.lines.finish() {
            self.event(&data, on)?;
        }
        let text = self.text.trim();
        if text.is_empty() {
            let reason = self.incomplete.as_deref().unwrap_or("?");
            return Err(ApiError::Decode(format!(
                "resposta sem texto (motivo: {reason})"
            )));
        }
        Ok(Reply {
            text: text.to_owned(),
            cost: self.cost,
            reasoning_tokens: self.reasoning_tokens,
            searches: self.searches.into_iter().map(|(_, s)| s).collect(),
            sources: self.sources,
            fallback: false,
        })
    }
}

// ---------------------------------------------------------------- o pedido em si

impl Xai {
    /// A conversa em streaming, com pesquisa na web e no X. Recebe o mesmo pedido que
    /// `chat_stream` e devolve a resposta completa; `on` recebe cada coisa assim que chega.
    /// Se a ligação cair a meio, o erro vem sem a resposta: quem chama já recebeu o que
    /// chegou.
    pub async fn respond_stream(
        &self,
        req: &ChatStream<'_>,
        mut on: impl FnMut(Piece<'_>),
    ) -> Result<Reply, ApiError> {
        if NO_RESPONSES.load(Ordering::Relaxed) {
            return self.chat_fallback(req, on).await;
        }
        let mut effort = reasoning_effort(req.model);
        let mut tools = !knows(no_tools(), req.model);
        // Codificar uma captura em base64 custa; faz-se uma vez, mesmo que o pedido se repita.
        let urls: Vec<Vec<String>> = req
            .turns
            .iter()
            .map(|t| t.images.iter().map(Image::data_url).collect())
            .collect();
        loop {
            let body = Body {
                model: req.model,
                instructions: req.system,
                input: input(req.turns, &urls),
                tools: tools.then_some(TOOLS),
                temperature: req.temperature,
                max_output_tokens: req.max_tokens
                    + if effort.is_some() {
                        REASONING_HEADROOM
                    } else {
                        0
                    },
                stream: true,
                store: false,
                reasoning: effort.map(|effort| Effort { effort }),
            };
            let response = self
                .post("/responses")
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
                    tracing::info!(model = req.model, "responses: sem esforço de raciocínio");
                    remember(no_effort(), req.model);
                    effort = None;
                    continue;
                }
                if tools && is_tools_refusal(&error) {
                    tracing::info!(model = req.model, error = %error, "responses: o modelo não aceita as ferramentas; a repetir sem pesquisa");
                    remember(no_tools(), req.model);
                    tools = false;
                    continue;
                }
                match error {
                    ApiError::NoPermission(_) => {
                        tracing::warn!(
                            "a chave não tem acesso à Responses API; o assistente fica sem pesquisa"
                        );
                        NO_RESPONSES.store(true, Ordering::Relaxed);
                        return self.chat_fallback(req, on).await;
                    }
                    ApiError::ModelNotFound { .. } if status.as_u16() == 404 => {
                        tracing::warn!("a Responses API respondeu 404; a tentar o chat");
                        return self.chat_fallback(req, on).await;
                    }
                    error => return Err(error),
                }
            }
            let mut state = State::default();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                state.feed(&chunk?, &mut on)?;
                if state.done {
                    break;
                }
            }
            return state.finish(&mut on);
        }
    }

    /// O assistente sem pesquisa, pelo `/chat/completions`.
    async fn chat_fallback(
        &self,
        req: &ChatStream<'_>,
        mut on: impl FnMut(Piece<'_>),
    ) -> Result<Reply, ApiError> {
        let chat::Answer { text, cost } = self
            .chat_stream(req, |delta| on(Piece::Text(delta)))
            .await?;
        Ok(Reply {
            text,
            cost,
            reasoning_tokens: None,
            searches: Vec::new(),
            sources: Vec::new(),
            fallback: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// O SSE de uma resposta com raciocínio, duas pesquisas (uma na web, uma no X) e fontes,
    /// no formato da OpenAI que a xAI segue.
    const FULL: &str = r#"event: response.created
data: {"type":"response.created","response":{"id":"r1","status":"in_progress"}}

event: response.output_item.added
data: {"type":"response.output_item.added","output_index":0,"item":{"id":"rs1","type":"reasoning","summary":[]}}

event: response.reasoning_summary_text.delta
data: {"type":"response.reasoning_summary_text.delta","item_id":"rs1","delta":"Preciso de "}

event: response.reasoning_summary_text.delta
data: {"type":"response.reasoning_summary_text.delta","item_id":"rs1","delta":"notícias."}

event: response.output_item.added
data: {"type":"response.output_item.added","output_index":1,"item":{"id":"ws1","type":"web_search_call","status":"in_progress"}}

event: response.output_item.done
data: {"type":"response.output_item.done","output_index":1,"item":{"id":"ws1","type":"web_search_call","status":"completed","action":{"type":"search","query":"xAI notícias"}}}

event: response.output_item.done
data: {"type":"response.output_item.done","output_index":2,"item":{"id":"xs1","type":"custom_tool_call","name":"x_keyword_search","arguments":"{\"query\":\"from:xai\",\"limit\":5}","status":"completed"}}

event: response.output_text.delta
data: {"type":"response.output_text.delta","item_id":"m1","delta":"A xAI lançou"}

event: response.output_text.delta
data: {"type":"response.output_text.delta","item_id":"m1","delta":" o Grok 5 [[1]](https://x.ai/news)."}

event: response.output_text.annotation.added
data: {"type":"response.output_text.annotation.added","annotation":{"type":"url_citation","url":"https://x.ai/news","title":"Notícias da xAI"}}

event: response.completed
data: {"type":"response.completed","response":{"id":"r1","status":"completed","citations":["https://x.ai/news","https://x.com/xai/status/1","javascript:alert(1)"],"output":[],"usage":{"input_tokens":900,"output_tokens":120,"output_tokens_details":{"reasoning_tokens":80},"cost_in_usd_ticks":123000000}}}

"#;

    fn run(body: &str) -> (Vec<String>, Result<Reply, ApiError>) {
        let mut seen = Vec::new();
        let result = {
            let mut state = State::default();
            let mut on = |p: Piece<'_>| seen.push(label(p));
            match state.feed(body.as_bytes(), &mut on) {
                Ok(()) => state.finish(&mut on),
                Err(e) => Err(e),
            }
        };
        (seen, result)
    }

    fn label(p: Piece<'_>) -> String {
        match p {
            Piece::Text(t) => format!("text:{t}"),
            Piece::Reasoning(t) => format!("reasoning:{t}"),
            Piece::Searches(s) => format!(
                "searches:{}",
                s.iter()
                    .map(|s| format!("{:?}={}", s.kind, s.query))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Piece::Sources(s) => format!("sources:{}", s.len()),
        }
    }

    #[test]
    fn a_full_answer_brings_reasoning_searches_text_and_sources() {
        let (seen, reply) = run(FULL);
        let reply = reply.unwrap();
        assert_eq!(
            seen,
            [
                "reasoning:Preciso de ",
                "reasoning:notícias.",
                "searches:Web=",
                "searches:Web=xAI notícias",
                "searches:Web=xAI notícias,X=from:xai",
                "text:A xAI lançou",
                "text: o Grok 5 [[1]](https://x.ai/news).",
                "sources:1",
                "sources:2",
            ]
        );
        assert_eq!(
            reply.text,
            "A xAI lançou o Grok 5 [[1]](https://x.ai/news)."
        );
        assert_eq!(reply.reasoning_tokens, Some(80));
        assert!((reply.cost.unwrap() - 0.0123).abs() < 1e-12);
        assert_eq!(reply.searches.len(), 2);
        assert_eq!(reply.searches[1].kind, SearchKind::X);
        // O `javascript:` das citações nunca vira fonte.
        assert_eq!(
            reply.sources,
            [
                Source {
                    url: "https://x.ai/news".into(),
                    title: Some("Notícias da xAI".into())
                },
                Source {
                    url: "https://x.com/xai/status/1".into(),
                    title: None
                },
            ]
        );
        assert!(!reply.fallback);
    }

    #[test]
    fn text_and_sources_that_only_come_in_the_final_summary_count() {
        let body = r#"data: {"type":"response.completed","response":{"status":"completed","output":[{"type":"web_search_call","id":"w","action":{"query":"tempo em Lisboa"}},{"type":"message","content":[{"type":"output_text","text":"Sol.","annotations":[{"type":"url_citation","url":"https://ipma.pt","title":"IPMA"}]}]}],"usage":{}}}

"#;
        let (seen, reply) = run(body);
        let reply = reply.unwrap();
        assert_eq!(reply.text, "Sol.");
        assert_eq!(reply.searches[0].query, "tempo em Lisboa");
        assert_eq!(reply.sources[0].title.as_deref(), Some("IPMA"));
        assert!(seen.contains(&"text:Sol.".to_owned()));
    }

    #[test]
    fn an_answer_cut_by_the_token_limit_says_why() {
        let body = "data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"output\":[]}}\n\n";
        let err = run(body).1.err().unwrap();
        assert!(err.to_string().contains("max_output_tokens"), "{err}");
    }

    #[test]
    fn an_error_in_the_middle_is_an_error() {
        let body = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Olá\"}\n\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"servidor ocupado\"}}}\n\n";
        let (seen, result) = run(body);
        assert_eq!(seen, ["text:Olá"]);
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("servidor ocupado")
        );
    }

    #[test]
    fn unknown_events_are_ignored() {
        let body = "data: {\"type\":\"response.web_search_call.searching\",\"item_id\":\"w\"}\n\ndata: {\"type\":\"response.something_new\"}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\ndata: [DONE]\n\n";
        assert_eq!(run(body).1.unwrap().text, "ok");
    }

    #[test]
    fn searches_are_recognised_by_several_names() {
        let web = serde_json::json!({"type":"web_search_call","action":{"query":"a"}});
        let x = serde_json::json!({"type":"x_search_call","query":"b"});
        let generic = serde_json::json!({"type":"function_call","name":"browse_page","arguments":{"url":"https://a.pt"}});
        let message = serde_json::json!({"type":"message"});
        assert_eq!(search_kind(&web), Some(SearchKind::Web));
        assert_eq!(search_kind(&x), Some(SearchKind::X));
        assert_eq!(search_kind(&generic), Some(SearchKind::Web));
        assert_eq!(search_kind(&message), None);
        assert_eq!(search_query(&web), "a");
        assert_eq!(search_query(&x), "b");
        assert_eq!(search_query(&generic), "https://a.pt");
    }

    #[test]
    fn the_request_has_the_tools_the_images_and_no_storage() {
        let turns = [
            Turn {
                role: Role::User,
                content: "o que é isto?".into(),
                images: Vec::new(),
            },
            Turn {
                role: Role::Assistant,
                content: "um erro".into(),
                images: Vec::new(),
            },
        ];
        let urls = vec![vec!["data:image/png;base64,AA==".to_owned()], Vec::new()];
        let body = Body {
            model: "grok-x",
            instructions: "sistema",
            input: input(&turns, &urls),
            tools: Some(TOOLS),
            temperature: 0.3,
            max_output_tokens: 100,
            stream: true,
            store: false,
            reasoning: Some(Effort { effort: "low" }),
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["store"], false);
        assert_eq!(v["tools"][0]["type"], "web_search");
        assert_eq!(v["tools"][1]["type"], "x_search");
        assert_eq!(v["reasoning"]["effort"], "low");
        assert_eq!(v["instructions"], "sistema");
        let parts = v["input"][0]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "input_image");
        assert_eq!(parts[0]["image_url"], "data:image/png;base64,AA==");
        assert_eq!(parts[1]["type"], "input_text");
        assert_eq!(v["input"][1]["role"], "assistant");
        assert_eq!(v["input"][1]["content"], "um erro");
    }
}

#[cfg(test)]
mod http_tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    const HEAD_SSE: &str =
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";

    /// Cada item de `script` é a resposta inteira de uma ligação. Devolve o cliente e o
    /// (caminho, corpo) de cada pedido que chegou.
    async fn fake_server(script: Vec<String>) -> (Xai, Arc<Mutex<Vec<(String, Value)>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            for reply in script {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (body_at, len) = loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
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
                let head = String::from_utf8_lossy(&buf[..body_at]).to_lowercase();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                log.lock().unwrap().push((
                    path,
                    serde_json::from_slice(&buf[body_at..body_at + len]).unwrap(),
                ));
                socket.write_all(reply.as_bytes()).await.unwrap();
                let _ = socket.shutdown().await;
            }
        });
        let _ = crate::xai::http_client();
        let client = reqwest::Client::builder().build().unwrap();
        (Xai::with_base(client, "k".into(), base), seen)
    }

    fn json_error(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn ok_stream() -> String {
        format!(
            "{HEAD_SSE}data: {{\"type\":\"response.output_text.delta\",\"delta\":\"Olá\"}}\n\n\
             data: {{\"type\":\"response.completed\",\"response\":{{\"status\":\"completed\",\"output\":[],\"usage\":{{\"cost_in_usd_ticks\":10000000}}}}}}\n\n"
        )
    }

    fn chat_stream_ok() -> String {
        format!(
            "{HEAD_SSE}data: {{\"choices\":[{{\"delta\":{{\"content\":\"Olá do chat\"}}}}]}}\n\ndata: [DONE]\n\n"
        )
    }

    fn turns() -> [Turn; 1] {
        [Turn {
            role: Role::User,
            content: "u".into(),
            images: Vec::new(),
        }]
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
    async fn it_asks_the_responses_api_with_the_tools() {
        let (xai, seen) = fake_server(vec![ok_stream()]).await;
        let turns = turns();
        let reply = xai
            .respond_stream(&request("resp-a-non-reasoning", &turns), |_| {})
            .await
            .unwrap();
        assert_eq!(reply.text, "Olá");
        assert!((reply.cost.unwrap() - 0.001).abs() < 1e-12);
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, "/responses");
        assert_eq!(seen[0].1["tools"][0]["type"], "web_search");
        assert_eq!(seen[0].1["store"], false);
        assert!(seen[0].1.get("reasoning").is_none(), "não raciocina");
    }

    #[tokio::test]
    async fn a_model_that_refuses_the_tools_answers_without_them() {
        let refusal = json_error(
            "400 Bad Request",
            "{\"error\":\"Model resp-b does not support tools\"}",
        );
        let (xai, seen) = fake_server(vec![refusal, ok_stream()]).await;
        let turns = turns();
        let reply = xai
            .respond_stream(&request("resp-b-non-reasoning", &turns), |_| {})
            .await
            .unwrap();
        assert_eq!(reply.text, "Olá");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[1].1.get("tools").is_none(), "repetiu sem ferramentas");
        assert!(knows(no_tools(), "resp-b-non-reasoning"));
    }

    #[tokio::test]
    async fn a_404_falls_back_to_the_chat_once() {
        let missing = json_error("404 Not Found", "{\"error\":\"not found\"}");
        let (xai, seen) = fake_server(vec![missing, chat_stream_ok()]).await;
        let turns = turns();
        let mut texts = Vec::new();
        let reply = xai
            .respond_stream(&request("resp-c-non-reasoning", &turns), |p| {
                if let Piece::Text(t) = p {
                    texts.push(t.to_owned());
                }
            })
            .await
            .unwrap();
        assert_eq!(reply.text, "Olá do chat");
        assert!(reply.fallback);
        assert_eq!(texts, ["Olá do chat"]);
        let seen = seen.lock().unwrap();
        assert_eq!(seen[1].0, "/chat/completions");
        assert!(
            !NO_RESPONSES.load(Ordering::Relaxed),
            "um 404 não fica lembrado"
        );
    }
}
