//! A lista de modelos de texto da xAI (`GET /v1/language-models`), com os preços, e a
//! cache dela em disco (`app_cache_dir`), válida por 24 h. Evita um pedido sempre que se
//! abre a janela e permite mostrar a lista mesmo sem rede.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{ApiError, Xai, parse_json, read_body};

pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Um modelo tal como aparece no seletor da UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    /// USD por token de entrada.
    pub prompt_price: Option<f64>,
    /// USD por token de saída.
    pub completion_price: Option<f64>,
    /// Raciocina antes de responder (mais lento e mais caro para limpar um ditado).
    #[serde(default)]
    pub reasoning: bool,
    /// Também lê imagens (`input_modalities` tem "image"): serve para as capturas do ecrã.
    /// Uma cache escrita antes deste campo não o tem: conta como "não", até ser renovada.
    #[serde(default)]
    pub vision: bool,
}

#[derive(Deserialize)]
struct Body {
    models: Vec<Raw>,
}

#[derive(Deserialize)]
struct Raw {
    id: String,
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
    #[serde(default)]
    prompt_text_token_price: Option<u64>,
    #[serde(default)]
    completion_text_token_price: Option<u64>,
    #[serde(default)]
    capabilities: Option<Capabilities>,
}

#[derive(Deserialize, Default)]
struct Capabilities {
    #[serde(default)]
    reasoning_effort: Vec<String>,
}

/// Os preços da API vêm em cêntimos de dólar por 100 milhões de tokens (12500 = 1,25 US$
/// por milhão); a UI quer dólares por token.
fn usd_per_token(v: u64) -> f64 {
    v as f64 / 1e10
}

fn parse_models(body: &[u8]) -> Result<Vec<ModelInfo>, ApiError> {
    let parsed: Body = parse_json(body)?;
    let mut models: Vec<ModelInfo> = parsed
        .models
        .into_iter()
        // Só os que leem e escrevem texto: é o que a limpeza e os resumos precisam.
        .filter(|m| {
            m.input_modalities.iter().any(|x| x == "text")
                && m.output_modalities.iter().any(|x| x == "text")
                // Os "multi-agent" só falam pela API de respostas: não servem aqui.
                && !m.id.contains("multi-agent")
        })
        .map(|m| ModelInfo {
            // Os que têm esforço configurável, e os que dizem "reasoning" no nome (sempre a
            // raciocinar, sem esforço para escolher).
            reasoning: m
                .capabilities
                .is_some_and(|c| !c.reasoning_effort.is_empty())
                || (m.id.contains("reasoning") && !m.id.contains("non-reasoning")),
            vision: m.input_modalities.iter().any(|x| x == "image"),
            prompt_price: m.prompt_text_token_price.map(usd_per_token),
            completion_price: m.completion_text_token_price.map(usd_per_token),
            id: m.id,
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

impl Xai {
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, ApiError> {
        let response = self.get("/language-models").send().await?;
        parse_models(&read_body(response, "").await?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelList {
    #[serde(with = "time::serde::rfc3339")]
    pub fetched_at: OffsetDateTime,
    pub models: Vec<ModelInfo>,
    /// `true` quando a lista vem da cache porque a rede falhou.
    #[serde(default)]
    pub stale: bool,
}

impl ModelList {
    pub fn is_fresh(&self, now: OffsetDateTime) -> bool {
        let age = now - self.fetched_at;
        age >= time::Duration::ZERO && age < TTL
    }
}

fn file(dir: &Path) -> PathBuf {
    dir.join("models-xai.json")
}

pub fn read(dir: &Path) -> Option<ModelList> {
    let bytes = std::fs::read(file(dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn write(dir: &Path, list: &ModelList) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = file(dir).with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec(list).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(tmp, file(dir))?;
    // As listas do tempo do OpenRouter já não servem para nada.
    for legacy in ["models-text.json", "models-transcription.json"] {
        let _ = std::fs::remove_file(dir.join(legacy));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn parses_text_models_and_converts_prices_to_usd_per_token() {
        let body = br#"{"models":[
            {"id":"grok-4.7","input_modalities":["text","image"],"output_modalities":["text"],
             "prompt_text_token_price":20000,"completion_text_token_price":60000,
             "capabilities":{"reasoning_effort":["low","medium","high"],"default_reasoning_effort":"high"}},
            {"id":"grok-4.20-0309-non-reasoning","input_modalities":["text"],"output_modalities":["text"],
             "prompt_text_token_price":12500,"completion_text_token_price":25000,
             "capabilities":{"reasoning_effort":[]}},
            {"id":"grok-imagine-image","input_modalities":["text"],"output_modalities":["image"]}
        ]}"#;
        let models = parse_models(body).unwrap();
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["grok-4.20-0309-non-reasoning", "grok-4.7"],
            "ordenados, e sem os que não escrevem texto"
        );
        // 12500 = 1,25 US$ por milhão de tokens.
        assert!((models[0].prompt_price.unwrap() * 1e6 - 1.25).abs() < 1e-9);
        assert!((models[0].completion_price.unwrap() * 1e6 - 2.5).abs() < 1e-9);
        assert!(!models[0].reasoning);
        assert!(models[1].reasoning);
        assert!(!models[0].vision, "só texto");
        assert!(models[1].vision, "lê imagens");
    }

    #[test]
    fn multi_agent_models_are_left_out_and_named_reasoning_models_are_flagged() {
        let body = br#"{"models":[
            {"id":"grok-4.20-multi-agent-0309","input_modalities":["text"],"output_modalities":["text"]},
            {"id":"grok-4.20-0309-reasoning","input_modalities":["text"],"output_modalities":["text"],"capabilities":{"reasoning_effort":[]}},
            {"id":"grok-4.20-0309-non-reasoning","input_modalities":["text"],"output_modalities":["text"]}
        ]}"#;
        let models = parse_models(body).unwrap();
        let by_id = |id: &str| models.iter().find(|m| m.id == id);
        assert!(by_id("grok-4.20-multi-agent-0309").is_none());
        assert!(
            by_id("grok-4.20-0309-reasoning").unwrap().reasoning,
            "sempre a raciocinar"
        );
        assert!(!by_id("grok-4.20-0309-non-reasoning").unwrap().reasoning);
    }

    #[test]
    fn a_model_without_prices_has_none() {
        let body =
            br#"{"models":[{"id":"x","input_modalities":["text"],"output_modalities":["text"]}]}"#;
        let models = parse_models(body).unwrap();
        assert_eq!(models[0].prompt_price, None);
        assert!(!models[0].reasoning);
        assert!(matches!(parse_models(b"[]"), Err(ApiError::Decode(_))));
    }

    #[test]
    fn an_old_cache_without_the_vision_field_still_reads() {
        let old = r#"{"fetched_at":"2026-10-11T12:00:00Z","models":[{"id":"grok-4.7","prompt_price":2e-6,"completion_price":6e-6,"reasoning":true}]}"#;
        let list: ModelList = serde_json::from_str(old).unwrap();
        assert!(!list.models[0].vision);
    }

    #[test]
    fn freshness_uses_ttl() {
        let list = ModelList {
            fetched_at: datetime!(2026-10-11 12:00 UTC),
            models: vec![],
            stale: false,
        };
        assert!(list.is_fresh(datetime!(2026-10-12 11:59 UTC)));
        assert!(!list.is_fresh(datetime!(2026-10-12 12:00 UTC)));
        // Relógio que andou para trás: não confiar na cache.
        assert!(!list.is_fresh(datetime!(2026-10-11 11:00 UTC)));
    }

    #[test]
    fn roundtrips_through_disk_and_clears_the_old_lists() {
        let dir = std::env::temp_dir().join(format!("ditok-models-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("models-text.json"), "{}").unwrap();
        let list = ModelList {
            fetched_at: datetime!(2026-10-11 12:00 UTC),
            models: vec![ModelInfo {
                id: "grok-4.20-0309-non-reasoning".into(),
                prompt_price: Some(1.25e-6),
                completion_price: Some(2.5e-6),
                reasoning: false,
                vision: false,
            }],
            stale: false,
        };
        assert!(read(&dir).is_none());
        write(&dir, &list).unwrap();
        assert_eq!(read(&dir).unwrap().models, list.models);
        assert!(!dir.join("models-text.json").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
