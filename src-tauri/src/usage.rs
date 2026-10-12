//! Estatísticas de uso: palavras por dia, sequência de dias, mapa de calor e as
//! apps onde mais ditas. Só contagens: o texto nunca é guardado aqui.
//!
//! Um ficheiro pequeno (`app_data_dir/usage.json`) com uma entrada por dia, como o
//! `costs.json` tem uma por mês. Os dias são em hora LOCAL ("2026-10-12" é o dia
//! em que ditaste, onde estás), calculados no momento do ditado e gravados como
//! texto: nunca se reinterpretam depois, por isso mudar de fuso não os desloca.
//!
//! A parte que calcula (`summarize`) é pura: recebe os dias e a data de hoje, e
//! devolve o resumo. Assim testa-se sem relógio, sem disco e sem fuso horário.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, PoisonError},
};

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Dias guardados, no máximo (mais de um ano: o mapa de calor mostra 53 semanas).
const MAX_DAYS: usize = 400;
/// Apps diferentes guardadas por dia; as que menos palavras tiveram saem.
const MAX_APPS_PER_DAY: usize = 50;
/// Linhas que ficam em metrics.jsonl depois de as contas antigas passarem para aqui.
const KEEP_METRIC_LINES: usize = 5000;
/// Para estimar palavras a partir de carateres nas linhas antigas (que não as
/// guardavam): uma palavra em português tem uns 5 carateres mais o espaço.
const CHARS_PER_WORD: f64 = 6.0;
/// Colunas do mapa de calor: uma por semana, cerca de um ano.
pub const WEEKS: usize = 53;
const TOP_APPS: usize = 6;

/// O nome de uma app como se compara e guarda ("WhatsApp.exe" → "whatsapp").
pub fn app_key(name: &str) -> String {
    crate::config::app_key(name)
}

/// O nome para mostrar de uma app guardada por `app_key`. As conhecidas levam o
/// nome certo ("whatsapp" → "WhatsApp"); as outras, a primeira letra em maiúscula.
pub fn app_label(key: &str) -> String {
    const KNOWN: &[(&str, &str)] = &[
        ("whatsapp", "WhatsApp"),
        ("windowsterminal", "Windows Terminal"),
        ("code", "VS Code"),
        ("visual studio code", "VS Code"),
        ("cursor", "Cursor"),
        ("chrome", "Chrome"),
        ("google chrome", "Chrome"),
        ("msedge", "Edge"),
        ("microsoft edge", "Edge"),
        ("firefox", "Firefox"),
        ("safari", "Safari"),
        ("arc", "Arc"),
        ("slack", "Slack"),
        ("discord", "Discord"),
        ("telegram", "Telegram"),
        ("outlook", "Outlook"),
        ("olk", "Outlook"),
        ("microsoft outlook", "Outlook"),
        ("winword", "Word"),
        ("microsoft word", "Word"),
        ("excel", "Excel"),
        ("powerpnt", "PowerPoint"),
        ("notepad", "Bloco de notas"),
        ("explorer", "Explorador"),
        ("teams", "Teams"),
        ("ms-teams", "Teams"),
        ("notion", "Notion"),
        ("obsidian", "Obsidian"),
        ("spotify", "Spotify"),
        ("mail", "Mail"),
        ("terminal", "Terminal"),
        ("iterm2", "iTerm"),
        ("claude", "Claude"),
    ];
    if let Some((_, label)) = KNOWN.iter().find(|(k, _)| *k == key) {
        return (*label).to_owned();
    }
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// O dia local em que um instante (UTC) caiu.
pub fn local_day(at: OffsetDateTime) -> NaiveDate {
    day_in(at.unix_timestamp(), &Local)
}

/// O mesmo para qualquer fuso (os testes usam um fuso fixo).
fn day_in<Tz: TimeZone>(secs: i64, tz: &Tz) -> NaiveDate {
    tz.timestamp_opt(secs, 0)
        .single()
        .map(|d| d.date_naive())
        .unwrap_or_default()
}

/// O que se ditou num dia.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayUsage {
    /// Palavras ditadas (o modo comando não conta: o texto é uma seleção reescrita).
    pub words: u64,
    /// Ditados e comandos que correram bem.
    pub dictations: u64,
    /// Duração do áudio dos ditados que deram palavras (para a velocidade de fala).
    pub audio_ms: u64,
    /// Palavras por app (nomes de `app_key`).
    #[serde(default)]
    pub apps: BTreeMap<String, u64>,
    /// As palavras vêm de uma estimativa (linhas antigas das métricas).
    #[serde(default)]
    pub estimated: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct UsageFile {
    /// As contas antigas de metrics.jsonl já foram passadas para aqui.
    #[serde(default)]
    backfilled: bool,
    #[serde(default)]
    days: BTreeMap<String, DayUsage>,
}

pub struct UsageStore {
    dir: PathBuf,
    /// Cada alteração lê, muda e escreve o ficheiro: o `Mutex` mantém a ordem.
    lock: Mutex<()>,
}

impl UsageStore {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            lock: Mutex::new(()),
        }
    }

    fn guard(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn path(&self) -> PathBuf {
        self.dir.join("usage.json")
    }

    fn metrics_path(&self) -> PathBuf {
        self.dir.join("metrics.jsonl")
    }

    /// Um usage.json que não se lê é posto de parte (`.bak`), como o histórico:
    /// tratá-lo como vazio e escrever por cima perdia as contas todas.
    fn read(&self) -> UsageFile {
        let path = self.path();
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                let backup = path.with_extension("json.bak");
                tracing::warn!(error = %e, "usage.json ilegível; guardado como .bak");
                let _ = std::fs::rename(&path, backup);
                UsageFile::default()
            }),
            Err(_) => UsageFile::default(),
        }
    }

    fn write(&self, file: &UsageFile) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec(file).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(tmp, path)
    }

    /// Passa uma vez as contas de metrics.jsonl (o que se ditou antes de existirem
    /// estas estatísticas) para os dias. Devolve se mudou alguma coisa.
    fn backfill(&self, file: &mut UsageFile) -> bool {
        if file.backfilled {
            return false;
        }
        let text = std::fs::read_to_string(self.metrics_path()).unwrap_or_default();
        for (day, past) in backfill_days(&text, &Local) {
            let entry = file.days.entry(day).or_default();
            entry.words += past.words;
            entry.dictations += past.dictations;
            entry.audio_ms += past.audio_ms;
            entry.estimated |= past.estimated;
        }
        file.backfilled = true;
        prune_days(&mut file.days);
        true
    }

    /// No arranque: faz a passagem das contas antigas e, só depois, deixa
    /// metrics.jsonl com as últimas linhas (cresce sem limite, e as contas já estão
    /// em `usage.json` e `costs.json`).
    pub fn startup(&self) {
        let _guard = self.guard();
        let mut file = self.read();
        if self.backfill(&mut file)
            && let Err(e) = self.write(&file)
        {
            tracing::warn!(error = %e, "não foi possível guardar as estatísticas antigas");
            return;
        }
        if let Err(e) = keep_last_lines(&self.metrics_path(), KEEP_METRIC_LINES) {
            tracing::warn!(error = %e, "não foi possível aparar metrics.jsonl");
        }
    }

    /// Soma um ditado que correu bem. `words` é 0 no modo comando.
    pub fn record(
        &self,
        day: NaiveDate,
        words: u64,
        audio_ms: u64,
        app: Option<&str>,
    ) -> std::io::Result<()> {
        let _guard = self.guard();
        let mut file = self.read();
        // Antes de somar: se a passagem das contas antigas ainda não aconteceu,
        // acontece já (senão ficava por fazer para sempre, com dias a meio).
        self.backfill(&mut file);
        let entry = file.days.entry(day.to_string()).or_default();
        entry.dictations += 1;
        if words > 0 {
            entry.words += words;
            entry.audio_ms += audio_ms;
            if let Some(app) = app {
                *entry.apps.entry(app.to_owned()).or_default() += words;
                prune_apps(&mut entry.apps);
            }
        }
        prune_days(&mut file.days);
        self.write(&file)
    }

    pub fn load(&self) -> BTreeMap<String, DayUsage> {
        let _guard = self.guard();
        let mut file = self.read();
        if self.backfill(&mut file) {
            let _ = self.write(&file);
        }
        file.days
    }

    /// "Apagar estatísticas": zera os dias. Fica marcado como já passado, para as
    /// contas antigas de metrics.jsonl não voltarem.
    pub fn clear(&self) -> std::io::Result<()> {
        let _guard = self.guard();
        self.write(&UsageFile {
            backfilled: true,
            days: BTreeMap::new(),
        })
    }
}

fn prune_days(days: &mut BTreeMap<String, DayUsage>) {
    while days.len() > MAX_DAYS {
        // `BTreeMap` está ordenado pelo texto "AAAA-MM-DD": o primeiro é o mais antigo.
        let Some(oldest) = days.keys().next().cloned() else {
            break;
        };
        days.remove(&oldest);
    }
}

fn prune_apps(apps: &mut BTreeMap<String, u64>) {
    while apps.len() > MAX_APPS_PER_DAY {
        let Some(least) = apps
            .iter()
            .min_by_key(|(_, words)| **words)
            .map(|(app, _)| app.clone())
        else {
            break;
        };
        apps.remove(&least);
    }
}

/// Lê metrics.jsonl e devolve o que cada dia teve. Só conta ditados e comandos que
/// correram bem (sem erro, com colagem); transcrições de ficheiros não. As palavras
/// são estimadas a partir dos carateres, e o dia fica marcado como estimado.
fn backfill_days<Tz: TimeZone>(text: &str, tz: &Tz) -> BTreeMap<String, DayUsage> {
    let mut days: BTreeMap<String, DayUsage> = BTreeMap::new();
    for line in text.lines() {
        let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let flag = |key: &str| row.get(key).and_then(serde_json::Value::as_bool) == Some(true);
        let ok = row.get("error").is_none_or(serde_json::Value::is_null)
            && row.get("total_ms").is_some_and(|v| v.is_u64())
            && !flag("file");
        if !ok {
            continue;
        }
        let Some(at) = row
            .get("at")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        else {
            continue;
        };
        let day = at.with_timezone(tz).date_naive().to_string();
        let chars = row.get("chars").and_then(|v| v.as_u64()).unwrap_or(0);
        let words = if flag("command") {
            0
        } else {
            (chars as f64 / CHARS_PER_WORD).round() as u64
        };
        let entry = days.entry(day).or_default();
        entry.dictations += 1;
        entry.estimated = true;
        if words > 0 {
            entry.words += words;
            entry.audio_ms += row.get("audio_ms").and_then(|v| v.as_u64()).unwrap_or(0);
        }
    }
    days
}

/// Deixa só as últimas `keep` linhas do ficheiro (escreve num temporário e troca).
fn keep_last_lines(path: &Path, keep: usize) -> std::io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= keep {
        return Ok(());
    }
    let mut kept = lines[lines.len() - keep..].join("\n");
    kept.push('\n');
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, kept)?;
    std::fs::rename(tmp, path)
}

// ---------------------------------------------------------------- resumo

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppUsage {
    /// O nome para mostrar ("WhatsApp").
    pub app: String,
    pub words: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeatCell {
    /// "2026-10-12".
    pub date: String,
    pub words: u64,
    /// 0 = nada; 1 a 4 = de menos a mais palavras (por quartis dos dias com palavras).
    pub level: u8,
    /// Um dia que ainda não chegou (a última semana começa na segunda-feira).
    pub future: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageSummary {
    pub total_words: u64,
    pub total_dictations: u64,
    pub month_words: u64,
    /// Dias seguidos com ditados, até hoje (ou até ontem, se hoje ainda não ditaste).
    pub streak_current: u32,
    pub streak_best: u32,
    /// Palavras por minuto de fala; `None` sem dados.
    pub wpm: Option<f64>,
    /// Quanto tempo se poupou face a escrever à velocidade `typing_wpm`.
    pub saved_minutes: f64,
    pub typing_wpm: u32,
    pub top_apps: Vec<AppUsage>,
    /// 53 semanas × 7 dias, semana a semana (segunda a domingo), a acabar na semana de hoje.
    pub heat: Vec<HeatCell>,
    /// Há dias cujas palavras são uma estimativa.
    pub estimated: bool,
    pub first_day: Option<String>,
}

fn monday_of(day: NaiveDate) -> NaiveDate {
    day - Duration::days(i64::from(day.weekday().num_days_from_monday()))
}

/// O resumo para `today`. Não usa o relógio nem o fuso: quem chama diz que dia é.
pub fn summarize(
    days: &BTreeMap<String, DayUsage>,
    today: NaiveDate,
    typing_wpm: u32,
) -> UsageSummary {
    let parsed: BTreeMap<NaiveDate, &DayUsage> = days
        .iter()
        .filter_map(|(k, v)| k.parse::<NaiveDate>().ok().map(|d| (d, v)))
        .collect();

    let total_words: u64 = parsed.values().map(|d| d.words).sum();
    let total_dictations: u64 = parsed.values().map(|d| d.dictations).sum();
    let audio_ms: u64 = parsed.values().map(|d| d.audio_ms).sum();
    let month_words: u64 = parsed
        .iter()
        .filter(|(d, _)| d.year() == today.year() && d.month() == today.month())
        .map(|(_, v)| v.words)
        .sum();

    let (streak_current, streak_best) = streaks(&parsed, today);

    let audio_minutes = audio_ms as f64 / 60_000.0;
    let wpm = (total_words > 0 && audio_minutes > 0.0).then(|| total_words as f64 / audio_minutes);
    let typing = f64::from(typing_wpm.max(1));
    let saved_minutes = (total_words as f64 / typing - audio_minutes).max(0.0);

    let mut by_app: BTreeMap<&str, u64> = BTreeMap::new();
    for day in parsed.values() {
        for (app, words) in &day.apps {
            *by_app.entry(app.as_str()).or_default() += words;
        }
    }
    let mut top_apps: Vec<AppUsage> = by_app
        .into_iter()
        .map(|(app, words)| AppUsage {
            app: app_label(app),
            words,
        })
        .collect();
    // Mais palavras primeiro; em empate, por nome (a ordem fica estável).
    top_apps.sort_by(|a, b| b.words.cmp(&a.words).then_with(|| a.app.cmp(&b.app)));
    top_apps.truncate(TOP_APPS);

    UsageSummary {
        total_words,
        total_dictations,
        month_words,
        streak_current,
        streak_best,
        wpm,
        saved_minutes,
        typing_wpm,
        top_apps,
        heat: heat_grid(&parsed, today),
        estimated: parsed.values().any(|d| d.estimated),
        first_day: parsed.keys().next().map(NaiveDate::to_string),
    }
}

/// (sequência atual, melhor sequência). Um dia conta se teve ditados.
fn streaks(days: &BTreeMap<NaiveDate, &DayUsage>, today: NaiveDate) -> (u32, u32) {
    let active = |d: NaiveDate| days.get(&d).is_some_and(|u| u.dictations > 0);

    let mut best = 0;
    let mut run = 0;
    let mut previous: Option<NaiveDate> = None;
    for (&day, usage) in days {
        if usage.dictations == 0 {
            continue;
        }
        run = if previous.is_some_and(|p| p + Duration::days(1) == day) {
            run + 1
        } else {
            1
        };
        best = best.max(run);
        previous = Some(day);
    }

    // Hoje ainda pode vir um ditado: sem ditados hoje, a sequência conta até ontem.
    let mut day = if active(today) {
        today
    } else {
        today - Duration::days(1)
    };
    let mut current = 0;
    while active(day) {
        current += 1;
        day -= Duration::days(1);
    }
    (current, best)
}

fn heat_grid(days: &BTreeMap<NaiveDate, &DayUsage>, today: NaiveDate) -> Vec<HeatCell> {
    let start = monday_of(today) - Duration::weeks(WEEKS as i64 - 1);
    let words_on = |d: NaiveDate| days.get(&d).map_or(0, |u| u.words);

    let mut non_zero: Vec<u64> = (0..WEEKS * 7)
        .map(|i| words_on(start + Duration::days(i as i64)))
        .filter(|w| *w > 0)
        .collect();
    non_zero.sort_unstable();
    // Os limites dos níveis 1 a 3: os quartis dos dias com palavras.
    let at = |quarter: usize| -> u64 {
        non_zero
            .get((non_zero.len() * quarter / 4).min(non_zero.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0)
    };
    let (q1, q2, q3) = (at(1), at(2), at(3));

    (0..WEEKS * 7)
        .map(|i| {
            let date = start + Duration::days(i as i64);
            let words = words_on(date);
            let level = match words {
                0 => 0,
                w if w <= q1 => 1,
                w if w <= q2 => 2,
                w if w <= q3 => 3,
                _ => 4,
            };
            HeatCell {
                date: date.to_string(),
                words,
                level,
                future: date > today,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Weekday};

    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn day(words: u64) -> DayUsage {
        DayUsage {
            words,
            dictations: u64::from(words > 0) + 1,
            audio_ms: words * 400, // 150 palavras por minuto
            ..DayUsage::default()
        }
    }

    fn days(entries: &[(&str, u64)]) -> BTreeMap<String, DayUsage> {
        entries
            .iter()
            .map(|(d, w)| ((*d).to_owned(), day(*w)))
            .collect()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ditok-usage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn streak_counts_consecutive_days() {
        let d = days(&[("2026-10-10", 5), ("2026-10-11", 5), ("2026-10-12", 5)]);
        let s = summarize(&d, date("2026-10-12"), 40);
        assert_eq!((s.streak_current, s.streak_best), (3, 3));
    }

    #[test]
    fn streak_breaks_on_gap() {
        let d = days(&[("2026-10-08", 5), ("2026-10-09", 5), ("2026-10-12", 5)]);
        let s = summarize(&d, date("2026-10-12"), 40);
        assert_eq!(s.streak_current, 1);
        assert_eq!(s.streak_best, 2);
    }

    #[test]
    fn best_streak_survives_gap() {
        let d = days(&[
            ("2026-09-01", 5),
            ("2026-09-02", 5),
            ("2026-09-03", 5),
            ("2026-09-04", 5),
            ("2026-10-12", 5),
        ]);
        let s = summarize(&d, date("2026-10-12"), 40);
        assert_eq!((s.streak_current, s.streak_best), (1, 4));
    }

    #[test]
    fn today_without_dictations_keeps_streak_from_yesterday() {
        let d = days(&[("2026-10-10", 5), ("2026-10-11", 5)]);
        let s = summarize(&d, date("2026-10-12"), 40);
        assert_eq!(s.streak_current, 2, "o dia de hoje ainda não acabou");
        // Mas se ontem também falhou, acabou.
        let d = days(&[("2026-10-10", 5)]);
        assert_eq!(summarize(&d, date("2026-10-12"), 40).streak_current, 0);
    }

    #[test]
    fn heat_grid_is_53x7_monday_aligned() {
        // 2026-10-12 é uma segunda-feira.
        assert_eq!(date("2026-10-12").weekday(), Weekday::Mon);
        let s = summarize(&days(&[("2026-10-12", 10)]), date("2026-10-14"), 40);
        assert_eq!(s.heat.len(), WEEKS * 7);
        // A primeira célula é uma segunda-feira, 52 semanas antes da de hoje.
        assert_eq!(s.heat[0].date, "2025-10-13");
        assert_eq!(date(&s.heat[0].date).weekday(), Weekday::Mon);
        // A última semana tem 7 células: a segunda (12), terça, quarta (hoje) e os dias que faltam.
        let last_week = &s.heat[(WEEKS - 1) * 7..];
        assert_eq!(last_week[0].date, "2026-10-12");
        assert_eq!(last_week[0].words, 10);
        assert!(!last_week[2].future, "quarta-feira é hoje");
        assert!(last_week[3].future && last_week[6].future);
    }

    #[test]
    fn heat_levels_by_quantile() {
        let d = days(&[
            ("2026-10-01", 10),
            ("2026-10-02", 20),
            ("2026-10-03", 30),
            ("2026-10-04", 40),
            ("2026-10-05", 1000),
        ]);
        let s = summarize(&d, date("2026-10-12"), 40);
        let level = |date: &str| s.heat.iter().find(|c| c.date == date).unwrap().level;
        assert_eq!(level("2026-10-09"), 0, "sem palavras");
        assert!(level("2026-10-01") < level("2026-10-04"));
        assert_eq!(level("2026-10-05"), 4, "o maior fica no nível mais alto");
        assert!((1..=4).contains(&level("2026-10-02")));
    }

    #[test]
    fn empty_usage_is_all_zero() {
        let s = summarize(&BTreeMap::new(), date("2026-10-12"), 40);
        assert_eq!(
            (s.total_words, s.total_dictations, s.month_words),
            (0, 0, 0)
        );
        assert_eq!((s.streak_current, s.streak_best), (0, 0));
        assert_eq!(s.wpm, None);
        assert_eq!(s.saved_minutes, 0.0);
        assert!(s.top_apps.is_empty());
        assert!(s.heat.iter().all(|c| c.level == 0));
        assert!(!s.estimated);
        assert_eq!(s.first_day, None);
    }

    #[test]
    fn wpm_and_time_saved() {
        // 1500 palavras em 10 minutos de fala = 150 wpm; a 40 wpm demorava 37,5 min.
        let d = days(&[("2026-10-12", 1500)]);
        let s = summarize(&d, date("2026-10-12"), 40);
        assert_eq!(s.wpm.map(|w| w.round()), Some(150.0));
        assert!((s.saved_minutes - 27.5).abs() < 0.01, "{}", s.saved_minutes);
        // Quem escreve mais depressa do que fala não poupa tempo, nunca fica negativo.
        assert_eq!(summarize(&d, date("2026-10-12"), 300).saved_minutes, 0.0);
    }

    #[test]
    fn month_words_only_counts_this_month() {
        let d = days(&[("2026-09-30", 100), ("2026-10-01", 7), ("2026-10-12", 5)]);
        let s = summarize(&d, date("2026-10-12"), 40);
        assert_eq!((s.month_words, s.total_words), (12, 112));
    }

    #[test]
    fn top_apps_are_summed_sorted_and_labelled() {
        let mut d = days(&[("2026-10-11", 30), ("2026-10-12", 30)]);
        d.get_mut("2026-10-11").unwrap().apps =
            [("whatsapp".to_owned(), 20), ("code".to_owned(), 10)].into();
        d.get_mut("2026-10-12").unwrap().apps =
            [("whatsapp".to_owned(), 5), ("obscura".to_owned(), 25)].into();
        let s = summarize(&d, date("2026-10-12"), 40);
        let apps: Vec<(&str, u64)> = s
            .top_apps
            .iter()
            .map(|a| (a.app.as_str(), a.words))
            .collect();
        assert_eq!(apps, [("Obscura", 25), ("WhatsApp", 25), ("VS Code", 10)]);
    }

    #[test]
    fn labels_and_keys() {
        assert_eq!(app_key("WhatsApp.exe"), "whatsapp");
        assert_eq!(app_label("windowsterminal"), "Windows Terminal");
        assert_eq!(app_label("minhaapp"), "Minhaapp");
        assert_eq!(app_label(""), "");
    }

    #[test]
    fn local_day_follows_the_timezone() {
        // 2026-10-12 23:30 UTC é já dia 13 em Lisboa (UTC+1 no horário de verão)...
        let secs = 1_791_847_800; // 2026-10-12T23:30:00Z
        let lisbon = FixedOffset::east_opt(3600).unwrap();
        let la = FixedOffset::west_opt(7 * 3600).unwrap();
        assert_eq!(day_in(secs, &lisbon), date("2026-10-13"));
        // ... e ainda dia 12 em Los Angeles.
        assert_eq!(day_in(secs, &la), date("2026-10-12"));
    }

    #[test]
    fn a_day_with_a_clock_change_is_still_one_day() {
        // Dois ditados a 25 h de distância (dia com 25 horas) caem em dias diferentes
        // e seguidos; as sequências trabalham com datas, não com horas.
        let d = days(&[("2026-10-24", 5), ("2026-10-25", 5), ("2026-10-26", 5)]);
        assert_eq!(summarize(&d, date("2026-10-26"), 40).streak_best, 3);
    }

    #[test]
    fn count_words_unicode() {
        use crate::metrics::count_words;
        assert_eq!(count_words("Olá, Pedro! Tudo bem?"), 4);
        assert_eq!(count_words("  espaços   a mais \n e linhas"), 5);
        assert_eq!(count_words("- — …"), 0, "só pontuação não são palavras");
        assert_eq!(count_words("ação coração 42"), 3);
        assert_eq!(count_words(""), 0);
    }

    #[test]
    fn backfill_skips_files_and_errors() {
        let rows = [
            r#"{"at":"2026-10-11T10:00:00Z","audio_ms":6000,"total_ms":900,"chars":120,"error":null}"#,
            // Uma transcrição de ficheiro: não é um ditado teu.
            r#"{"at":"2026-10-11T11:00:00Z","audio_ms":60000,"total_ms":null,"chars":9000,"error":null,"file":true}"#,
            // Um erro.
            r#"{"at":"2026-10-11T12:00:00Z","audio_ms":3000,"total_ms":null,"chars":0,"error":"falhou"}"#,
            // Um comando: conta um ditado, não palavras.
            r#"{"at":"2026-10-11T13:00:00Z","audio_ms":2000,"total_ms":1500,"chars":300,"error":null,"command":true}"#,
            "isto não é json",
        ]
        .join("\n");
        let days = backfill_days(&rows, &FixedOffset::east_opt(0).unwrap());
        assert_eq!(days.len(), 1);
        let d = &days["2026-10-11"];
        assert_eq!(d.dictations, 2);
        assert_eq!(d.words, 20, "120 carateres / 6");
        assert_eq!(
            d.audio_ms, 6000,
            "o áudio do comando não entra na velocidade de fala"
        );
        assert!(d.estimated);
    }

    #[test]
    fn backfill_is_idempotent() {
        let dir = temp_dir("backfill");
        std::fs::write(
            dir.join("metrics.jsonl"),
            r#"{"at":"2026-10-11T10:00:00Z","audio_ms":6000,"total_ms":900,"chars":120,"error":null}"#,
        )
        .unwrap();
        let store = UsageStore::new(dir.clone());
        let first = store.load();
        let second = store.load();
        assert_eq!(first, second);
        assert_eq!(first.values().map(|d| d.dictations).sum::<u64>(), 1);
        // Um ditado novo soma-se sem repetir o passado.
        store
            .record(date("2026-10-12"), 10, 4000, Some("whatsapp"))
            .unwrap();
        let after = store.load();
        assert_eq!(after.values().map(|d| d.dictations).sum::<u64>(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn record_before_startup_still_backfills_first() {
        let dir = temp_dir("order");
        std::fs::write(
            dir.join("metrics.jsonl"),
            r#"{"at":"2026-10-01T10:00:00Z","audio_ms":6000,"total_ms":900,"chars":60,"error":null}"#,
        )
        .unwrap();
        let store = UsageStore::new(dir.clone());
        store.record(date("2026-10-12"), 5, 1000, None).unwrap();
        let days = store.load();
        assert_eq!(days.len(), 2, "o passado e o ditado novo");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn clear_does_not_bring_the_past_back() {
        let dir = temp_dir("clear");
        std::fs::write(
            dir.join("metrics.jsonl"),
            r#"{"at":"2026-10-11T10:00:00Z","audio_ms":6000,"total_ms":900,"chars":120,"error":null}"#,
        )
        .unwrap();
        let store = UsageStore::new(dir.clone());
        assert_eq!(store.load().len(), 1);
        store.clear().unwrap();
        assert!(store.load().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn commands_count_a_dictation_but_no_words() {
        let dir = temp_dir("command");
        let store = UsageStore::new(dir.clone());
        store
            .record(date("2026-10-12"), 0, 3000, Some("code"))
            .unwrap();
        let d = &store.load()["2026-10-12"];
        assert_eq!((d.dictations, d.words, d.audio_ms), (1, 0, 0));
        assert!(d.apps.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prune_apps_per_day_and_days() {
        let mut apps: BTreeMap<String, u64> =
            (0..60).map(|i| (format!("app{i:02}"), i + 1)).collect();
        prune_apps(&mut apps);
        assert_eq!(apps.len(), MAX_APPS_PER_DAY);
        assert!(
            !apps.contains_key("app00"),
            "a que menos palavras teve saiu"
        );
        assert!(apps.contains_key("app59"));

        let mut many: BTreeMap<String, DayUsage> = (0..MAX_DAYS + 5)
            .map(|i| {
                (
                    (date("2025-01-01") + Duration::days(i as i64)).to_string(),
                    DayUsage::default(),
                )
            })
            .collect();
        prune_days(&mut many);
        assert_eq!(many.len(), MAX_DAYS);
        assert!(!many.contains_key("2025-01-01"), "o mais antigo saiu");
    }

    #[test]
    fn unreadable_usage_is_backed_up() {
        let dir = temp_dir("corrupt");
        std::fs::write(dir.join("usage.json"), b"{ nao e json").unwrap();
        let store = UsageStore::new(dir.clone());
        assert!(store.load().is_empty());
        assert!(dir.join("usage.json.bak").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn metrics_file_keeps_only_the_last_lines() {
        let dir = temp_dir("rotate");
        let path = dir.join("metrics.jsonl");
        let text: String = (0..10).map(|i| format!("{{\"n\":{i}}}\n")).collect();
        std::fs::write(&path, text).unwrap();
        keep_last_lines(&path, 3).unwrap();
        let left = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            left.lines().collect::<Vec<_>>(),
            [r#"{"n":7}"#, r#"{"n":8}"#, r#"{"n":9}"#]
        );
        // Com menos linhas do que o limite, não toca no ficheiro.
        keep_last_lines(&path, 100).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), left);
        let _ = std::fs::remove_dir_all(dir);
    }
}
