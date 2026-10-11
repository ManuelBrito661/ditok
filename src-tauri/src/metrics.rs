//! Métricas por ditado (JSON Lines em `app_data_dir/metrics.jsonl`) e custo
//! acumulado por mês (`app_data_dir/costs.json`).
//!
//! O texto ditado NÃO é guardado aqui, só o número de caracteres: o histórico de
//! ditados é uma opção à parte, desligada por omissão (fase 5).

use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize)]
pub struct DictationMetrics {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub provider: &'static str,
    pub model: String,
    pub transport: Option<&'static str>,
    pub audio_ms: u64,
    pub wav_bytes: usize,
    pub mic_start_ms: Option<u64>,
    /// Soltar a tecla → WAV pronto.
    pub release_to_wav_ms: u64,
    /// Upload + transcrição.
    pub stt_ms: Option<u64>,
    /// Clipboard + atalho de colar.
    pub paste_ms: Option<u64>,
    /// Soltar a tecla → texto colado. É o número que interessa.
    pub total_ms: Option<u64>,
    pub cost_usd: Option<f64>,
    pub chars: usize,
    pub generation_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MonthTotal {
    pub cost_usd: f64,
    pub dictations: u64,
}

pub struct MetricsStore {
    dir: PathBuf,
}

impl MetricsStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Bloqueante: chamar a partir de `spawn_blocking`. Só há um ditado de cada vez
    /// (a máquina de estados ignora o atalho enquanto processa), por isso não há
    /// escritas concorrentes nestes ficheiros.
    pub fn record(&self, m: &DictationMetrics) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let mut line = serde_json::to_vec(m).map_err(std::io::Error::other)?;
        line.push(b'\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("metrics.jsonl"))?
            .write_all(&line)?;

        if m.error.is_none() {
            add_to_month(
                &self.dir.join("costs.json"),
                &month_key(m.at),
                m.cost_usd.unwrap_or(0.0),
            )?;
        }
        Ok(())
    }
}

/// "2026-10". Em UTC, como a faturação do OpenRouter.
fn month_key(at: OffsetDateTime) -> String {
    format!("{:04}-{:02}", at.year(), u8::from(at.month()))
}

fn add_to_month(path: &Path, month: &str, cost: f64) -> std::io::Result<()> {
    // BTreeMap mantém os meses ordenados no ficheiro.
    let mut totals: BTreeMap<String, MonthTotal> = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "costs.json inválido; a recomeçar a contagem");
            BTreeMap::new()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => return Err(e),
    };
    let entry = totals.entry(month.to_owned()).or_default();
    entry.cost_usd += cost;
    entry.dictations += 1;

    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&totals).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(tmp, path)
}

pub fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn sample(cost: Option<f64>, error: Option<&str>) -> DictationMetrics {
        DictationMetrics {
            at: datetime!(2026-10-11 12:00 UTC),
            provider: "openrouter",
            model: "openai/whisper-large-v3-turbo".into(),
            transport: Some("multipart"),
            audio_ms: 5000,
            wav_bytes: 160_044,
            mic_start_ms: Some(46),
            release_to_wav_ms: 3,
            stt_ms: Some(600),
            paste_ms: Some(20),
            total_ms: Some(630),
            cost_usd: cost,
            chars: 80,
            generation_id: Some("gen-1".into()),
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn month_key_is_zero_padded() {
        assert_eq!(month_key(datetime!(2026-03-01 0:00 UTC)), "2026-03");
    }

    #[test]
    fn records_lines_and_accumulates_monthly_cost() {
        let dir = std::env::temp_dir().join(format!("ditok-metrics-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = MetricsStore::new(dir.clone());

        store.record(&sample(Some(0.0002), None)).unwrap();
        store.record(&sample(Some(0.0003), None)).unwrap();
        store.record(&sample(None, Some("timeout"))).unwrap(); // falhas não contam

        let lines = std::fs::read_to_string(dir.join("metrics.jsonl")).unwrap();
        assert_eq!(lines.lines().count(), 3);
        let first: serde_json::Value = serde_json::from_str(lines.lines().next().unwrap()).unwrap();
        assert_eq!(first["at"], "2026-10-11T12:00:00Z");
        assert_eq!(first["total_ms"], 630);

        let totals: BTreeMap<String, MonthTotal> =
            serde_json::from_slice(&std::fs::read(dir.join("costs.json")).unwrap()).unwrap();
        let oct = totals["2026-10"];
        assert_eq!(oct.dictations, 2);
        assert!((oct.cost_usd - 0.0005).abs() < 1e-12);
        let _ = std::fs::remove_dir_all(dir);
    }
}
