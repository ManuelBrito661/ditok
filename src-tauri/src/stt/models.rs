//! Cache em disco das listas de modelos do OpenRouter (`app_cache_dir`), válida por
//! 24 h. Evita descarregar ~750 KB de JSON (lista de modelos de texto) sempre que
//! se abre a janela, e permite mostrar a lista mesmo sem rede.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::openrouter::ModelInfo;

pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    /// Modelos de STT (`output_modalities=transcription`).
    Transcription,
    /// Modelos de texto, para a limpeza com LLM (`output_modalities=text`).
    Text,
}

impl ModelKind {
    pub fn output_modality(self) -> &'static str {
        match self {
            Self::Transcription => "transcription",
            Self::Text => "text",
        }
    }
    fn file(self, dir: &Path) -> PathBuf {
        dir.join(format!("models-{}.json", self.output_modality()))
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

pub fn read(dir: &Path, kind: ModelKind) -> Option<ModelList> {
    let bytes = std::fs::read(kind.file(dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn write(dir: &Path, kind: ModelKind, list: &ModelList) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = kind.file(dir).with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec(list).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(tmp, kind.file(dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

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
    fn roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("ditok-models-test-{}", std::process::id()));
        let list = ModelList {
            fetched_at: datetime!(2026-10-11 12:00 UTC),
            models: vec![ModelInfo {
                id: "openai/whisper-large-v3-turbo".into(),
                name: "Whisper".into(),
                prompt_price: Some(0.00000333),
                completion_price: Some(0.0),
            }],
            stale: false,
        };
        write(&dir, ModelKind::Transcription, &list).unwrap();
        let back = read(&dir, ModelKind::Transcription).unwrap();
        assert_eq!(back.models, list.models);
        assert!(read(&dir, ModelKind::Text).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
