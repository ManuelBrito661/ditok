//! Histórico opcional dos últimos ditados (com o texto), só em disco local
//! (`app_data_dir/history.json`). Desligado por omissão.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::config::Style;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// O texto colado.
    pub text: String,
    /// O texto antes da limpeza com LLM, se a limpeza o alterou.
    pub raw_text: Option<String>,
    pub model: String,
    pub audio_ms: u64,
    pub total_ms: Option<u64>,
    /// `false` se o texto não foi colado (a janela em foco mudou ou a colagem
    /// falhou) e ficou só no clipboard. Entradas antigas, sem o campo, foram coladas.
    #[serde(default = "pasted_by_default")]
    pub pasted: bool,
    /// A app onde o ditado começou ("whatsapp.exe"), se se soube qual era.
    #[serde(default)]
    pub app: Option<String>,
    /// O estilo aplicado, só quando mudou alguma coisa (não é o Predefinido e a
    /// limpeza está ligada). Entradas antigas não o têm.
    #[serde(default)]
    pub style: Option<Style>,
}

fn pasted_by_default() -> bool {
    true
}

pub struct HistoryStore {
    path: PathBuf,
}

impl HistoryStore {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            path: data_dir.join("history.json"),
        }
    }

    /// Mais recente primeiro.
    pub fn list(&self) -> Vec<HistoryEntry> {
        let mut entries = self.read();
        entries.reverse();
        entries
    }

    /// Bloqueante. Acrescenta e corta para os últimos `limit` ditados.
    pub fn push(&self, entry: HistoryEntry, limit: usize) -> std::io::Result<()> {
        let mut entries = self.read();
        entries.push(entry);
        let excess = entries.len().saturating_sub(limit);
        entries.drain(..excess);
        self.write(&entries)
    }

    pub fn clear(&self) -> std::io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    fn read(&self) -> Vec<HistoryEntry> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn write(&self, entries: &[HistoryEntry]) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec(entries).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(tmp, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn entry(text: &str) -> HistoryEntry {
        HistoryEntry {
            at: datetime!(2026-10-11 12:00 UTC),
            text: text.into(),
            raw_text: None,
            model: "m/m".into(),
            audio_ms: 1000,
            total_ms: Some(800),
            pasted: true,
            app: None,
            style: None,
        }
    }

    #[test]
    fn keeps_only_the_latest_entries_newest_first() {
        let dir = std::env::temp_dir().join(format!("ditok-history-test-{}", std::process::id()));
        let store = HistoryStore::new(dir.clone());
        for t in ["um", "dois", "três"] {
            store.push(entry(t), 2).unwrap();
        }
        let texts: Vec<String> = store.list().into_iter().map(|e| e.text).collect();
        assert_eq!(texts, ["três", "dois"]);
        store.clear().unwrap();
        assert!(store.list().is_empty());
        store.clear().unwrap(); // apagar duas vezes não é erro
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn old_entries_without_the_pasted_field_count_as_pasted() {
        let json = r#"{"at":"2026-10-11T12:00:00Z","text":"olá","raw_text":null,"model":"m","audio_ms":1,"total_ms":null}"#;
        let e: HistoryEntry = serde_json::from_str(json).unwrap();
        assert!(e.pasted);
        assert_eq!((e.app, e.style), (None, None), "sem app nem estilo");
    }
}
