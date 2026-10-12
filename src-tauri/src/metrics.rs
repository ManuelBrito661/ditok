//! Métricas por ditado (JSON Lines em `app_data_dir/metrics.jsonl`) e custo
//! acumulado por mês (`app_data_dir/costs.json`).
//!
//! O texto ditado NÃO é guardado aqui, só o número de caracteres: o histórico de
//! ditados é uma opção à parte, desligada por omissão (`history.rs`).

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
    /// Limpeza com LLM (só quando está ligada).
    pub cleanup_ms: Option<u64>,
    /// Clipboard + atalho de colar.
    pub paste_ms: Option<u64>,
    /// Soltar a tecla → texto colado. É o número que interessa.
    pub total_ms: Option<u64>,
    pub cost_usd: Option<f64>,
    pub cleanup_cost_usd: Option<f64>,
    pub chars: usize,
    /// Palavras do texto colado (ver `set_text`). 0 no modo comando: o texto é a
    /// seleção reescrita, não palavras ditadas.
    pub words: usize,
    /// A app onde o ditado começou (nome normalizado, ver `usage::app_key`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    pub generation_id: Option<String>,
    pub error: Option<String>,
    /// Transcrição de um ficheiro (`files.rs`), não um ditado: não conta para os
    /// percentis nem para o número de ditados, só para o custo.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub file: bool,
    /// Modo comando: `cleanup_ms`/`cleanup_cost_usd` são o pedido ao LLM com a
    /// instrução. Conta como um ditado (número, custo e latência).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub command: bool,
    /// Um resumo (`summarize.rs`): só interessa o custo (em `cleanup_cost_usd`) e o
    /// tempo (`cleanup_ms`). Não é um ditado: sem `total_ms`, não conta no número de
    /// ditados nem nos percentis.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub summary: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MonthTotal {
    pub cost_usd: f64,
    pub dictations: u64,
}

impl DictationMetrics {
    /// As métricas de um resumo (`summarize.rs`): o custo vai para os campos da
    /// limpeza, o tempo também, e sem `total_ms` não é contado como ditado.
    pub fn for_summary(model: &str, chars: usize, elapsed_ms: u64, cost: Option<f64>) -> Self {
        Self {
            at: now(),
            provider: "openrouter",
            model: model.to_owned(),
            transport: None,
            audio_ms: 0,
            wav_bytes: 0,
            mic_start_ms: None,
            release_to_wav_ms: 0,
            stt_ms: None,
            cleanup_ms: Some(elapsed_ms),
            paste_ms: None,
            total_ms: None,
            cost_usd: None,
            cleanup_cost_usd: cost,
            chars,
            words: 0,
            app: None,
            generation_id: None,
            error: None,
            file: false,
            command: false,
            summary: true,
        }
    }

    /// O único sítio que conta o texto: carateres e palavras vêm sempre do mesmo cálculo.
    pub fn set_text(&mut self, text: &str) {
        self.chars = text.chars().count();
        self.words = count_words(text);
    }
}

/// Palavras separadas por espaços. "Olá-mundo" conta uma; um número, uma.
pub fn count_words(text: &str) -> usize {
    text.split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .count()
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
    ///
    /// Com um orçamento definido, devolve um aviso se este ditado o fez passar os
    /// 80 % ou os 100 %.
    pub fn record(
        &self,
        m: &DictationMetrics,
        budget: Option<f64>,
    ) -> std::io::Result<Option<BudgetAlert>> {
        self.write(m, budget, true)
    }

    /// Um "Reprocessar": regista a linha e o custo, mas não conta como ditado novo
    /// (o ditado já estava no número).
    pub fn record_reprocess(
        &self,
        m: &DictationMetrics,
        budget: Option<f64>,
    ) -> std::io::Result<Option<BudgetAlert>> {
        self.write(m, budget, false)
    }

    fn write(
        &self,
        m: &DictationMetrics,
        budget: Option<f64>,
        new_dictation: bool,
    ) -> std::io::Result<Option<BudgetAlert>> {
        std::fs::create_dir_all(&self.dir)?;
        let mut line = serde_json::to_vec(m).map_err(std::io::Error::other)?;
        line.push(b'\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("metrics.jsonl"))?
            .write_all(&line)?;

        // O custo conta sempre que houve cobrança (mesmo que a colagem tenha
        // falhado depois); o número de ditados só conta os que colaram texto.
        let cost = m.cost_usd.unwrap_or(0.0) + m.cleanup_cost_usd.unwrap_or(0.0);
        let counted = u64::from(new_dictation && m.error.is_none() && m.total_ms.is_some());
        if cost <= 0.0 && counted == 0 {
            return Ok(None);
        }
        let before = add_to_month(
            &self.dir.join("costs.json"),
            &month_key(m.at),
            cost,
            counted,
        )?;
        Ok(budget.and_then(|b| BudgetAlert::crossed(before, before + cost, b)))
    }

    pub fn months(&self) -> BTreeMap<String, MonthTotal> {
        std::fs::read(self.dir.join("costs.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Bloqueante. Lê as últimas `recent` linhas de metrics.jsonl e calcula os
    /// percentis de latência dos ditados bem-sucedidos.
    pub fn stats(&self, recent: usize) -> Stats {
        let text = std::fs::read_to_string(self.dir.join("metrics.jsonl")).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let mut rows: Vec<MetricsRow> = lines[lines.len().saturating_sub(recent)..]
            .iter()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        rows.reverse(); // mais recente primeiro

        // Os ficheiros e os resumos têm custo, mas não latência de ditado.
        let ok: Vec<&MetricsRow> = rows
            .iter()
            .filter(|r| r.error.is_none() && !r.file && !r.summary)
            .collect();
        let pick = |f: fn(&MetricsRow) -> Option<u64>| {
            percentiles(ok.iter().filter_map(|r| f(r)).collect())
        };
        Stats {
            current_month: month_key(now()),
            months: self.months(),
            total: pick(|r| r.total_ms),
            stt: pick(|r| r.stt_ms),
            cleanup: pick(|r| r.cleanup_ms),
            recent: rows,
        }
    }
}

/// Uma linha de metrics.jsonl lida de volta (só os campos que a UI mostra).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsRow {
    pub at: String,
    pub model: String,
    pub audio_ms: u64,
    pub stt_ms: Option<u64>,
    #[serde(default)]
    pub cleanup_ms: Option<u64>,
    pub paste_ms: Option<u64>,
    pub total_ms: Option<u64>,
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub cleanup_cost_usd: Option<f64>,
    pub error: Option<String>,
    #[serde(default)]
    pub file: bool,
    #[serde(default)]
    pub command: bool,
    #[serde(default)]
    pub summary: bool,
    /// Só as linhas escritas depois de existir o campo; as antigas têm 0.
    #[serde(default)]
    pub chars: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Percentiles {
    pub count: usize,
    pub p50: u64,
    pub p90: u64,
    pub max: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub current_month: String,
    pub months: BTreeMap<String, MonthTotal>,
    pub total: Option<Percentiles>,
    pub stt: Option<Percentiles>,
    pub cleanup: Option<Percentiles>,
    pub recent: Vec<MetricsRow>,
}

/// Percentil pelo método do "rank mais próximo": sem interpolação, devolve sempre
/// um valor que foi realmente medido.
fn percentiles(mut values: Vec<u64>) -> Option<Percentiles> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let at =
        |p: f64| values[((p * values.len() as f64).ceil() as usize).clamp(1, values.len()) - 1];
    Some(Percentiles {
        count: values.len(),
        p50: at(0.5),
        p90: at(0.9),
        max: values[values.len() - 1],
    })
}

/// "2026-10". Em UTC, como a faturação do OpenRouter.
fn month_key(at: OffsetDateTime) -> String {
    format!("{:04}-{:02}", at.year(), u8::from(at.month()))
}

/// Aviso de orçamento, mostrado numa notificação.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BudgetAlert {
    /// Passou os 80 %.
    Near { spent: f64, budget: f64 },
    /// Chegou ao orçamento.
    Reached { spent: f64, budget: f64 },
}

impl BudgetAlert {
    pub const NEAR: f64 = 0.8;

    /// Compara o gasto do mês antes e depois de um ditado. Só avisa quando um
    /// limite é atravessado, por isso cada aviso aparece uma vez por mês sem ser
    /// preciso guardar que já foi dado; no mês seguinte a conta recomeça do zero.
    pub fn crossed(before: f64, after: f64, budget: f64) -> Option<Self> {
        let spent = after;
        if before < budget && after >= budget {
            Some(Self::Reached { spent, budget })
        } else if before < budget * Self::NEAR && after >= budget * Self::NEAR {
            Some(Self::Near { spent, budget })
        } else {
            None
        }
    }

    pub fn message(self) -> String {
        match self {
            Self::Near { spent, budget } => format!(
                "Já gastaste ${spent:.2} dos ${budget:.2} do orçamento deste mês ({:.0} %).",
                spent / budget * 100.0
            ),
            Self::Reached { spent, budget } => format!(
                "Chegaste ao orçamento deste mês: ${spent:.2} de ${budget:.2}. O Ditok continua a \
                 funcionar; podes mudar o orçamento nas definições."
            ),
        }
    }
}

/// Soma ao mês e devolve o custo que o mês tinha antes.
fn add_to_month(path: &Path, month: &str, cost: f64, dictations: u64) -> std::io::Result<f64> {
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
    let before = entry.cost_usd;
    entry.cost_usd += cost;
    entry.dictations += dictations;

    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&totals).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(tmp, path)?;
    Ok(before)
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
            cleanup_ms: None,
            paste_ms: Some(20),
            total_ms: Some(630),
            cost_usd: cost,
            cleanup_cost_usd: None,
            chars: 80,
            words: 14,
            app: None,
            generation_id: Some("gen-1".into()),
            file: false,
            command: false,
            summary: false,
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

        assert_eq!(
            store
                .record(&sample(Some(0.0002), None), Some(0.0004))
                .unwrap(),
            None
        );
        let alert = store
            .record(&sample(Some(0.0003), None), Some(0.0004))
            .unwrap();
        assert!(
            matches!(alert, Some(BudgetAlert::Reached { .. })),
            "{alert:?}"
        );
        store.record(&sample(None, Some("timeout")), None).unwrap(); // falhas não contam

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

        // As estatísticas leem de volta e ignoram a linha com erro nos percentis.
        let stats = store.stats(10);
        assert_eq!(stats.recent.len(), 3);
        assert_eq!(
            stats.recent[0].error.as_deref(),
            Some("timeout"),
            "mais recente primeiro"
        );
        assert_eq!(stats.total.map(|p| p.count), Some(2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn summary_metrics_flag_excluded_from_percentiles() {
        let dir = std::env::temp_dir().join(format!("ditok-summary-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = MetricsStore::new(dir.clone());
        store.record(&sample(Some(0.0001), None), None).unwrap();

        // Um resumo: custa dinheiro (na linha do custo da limpeza) mas não tem
        // `total_ms`, por isso não é um ditado.
        let mut summary = sample(None, None);
        summary.summary = true;
        summary.total_ms = None;
        summary.cleanup_cost_usd = Some(0.002);
        summary.cleanup_ms = Some(9000);
        store.record(&summary, None).unwrap();

        let totals = store.months();
        let month = totals["2026-10"];
        assert_eq!(month.dictations, 1, "o resumo não conta como ditado");
        assert!((month.cost_usd - 0.0021).abs() < 1e-12, "mas o custo soma");

        let stats = store.stats(10);
        assert_eq!(stats.recent.len(), 2);
        assert!(
            stats.recent[0].summary,
            "a linha lê-se de volta com a marca"
        );
        assert_eq!(
            stats.cleanup, None,
            "os 9 s do resumo não entram nos percentis da limpeza"
        );
        assert_eq!(stats.total.map(|p| p.count), Some(1));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn budget_alerts_fire_once_when_crossing() {
        let near = |b, a| BudgetAlert::crossed(b, a, 10.0);
        assert_eq!(near(7.0, 7.9), None);
        assert!(matches!(near(7.9, 8.0), Some(BudgetAlert::Near { spent, .. }) if spent == 8.0));
        assert_eq!(near(8.0, 9.0), None, "já tinha avisado aos 80 %");
        assert!(matches!(near(9.9, 10.1), Some(BudgetAlert::Reached { .. })));
        assert_eq!(near(10.1, 10.5), None, "já tinha avisado aos 100 %");
        // Um ditado caro que salta os dois limites dá só o aviso mais grave.
        assert!(matches!(near(7.0, 11.0), Some(BudgetAlert::Reached { .. })));
        assert_eq!(
            BudgetAlert::Near {
                spent: 8.123,
                budget: 10.0
            }
            .message(),
            "Já gastaste $8.12 dos $10.00 do orçamento deste mês (81 %)."
        );
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        assert_eq!(percentiles(vec![]), None);
        let p = percentiles(vec![500]).unwrap();
        assert_eq!((p.p50, p.p90, p.max), (500, 500, 500));
        let p = percentiles((1..=10).map(|i| i * 100).collect()).unwrap();
        assert_eq!((p.count, p.p50, p.p90, p.max), (10, 500, 900, 1000));
        let p = percentiles(vec![900, 100, 300]).unwrap();
        assert_eq!((p.p50, p.p90), (300, 900));
    }
}
