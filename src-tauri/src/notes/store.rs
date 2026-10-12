//! As sessões gravadas no disco: `notas/<id>/meta.json` e `notas/<id>/transcript.jsonl`.
//!
//! A transcrição é um ficheiro de linhas JSON a que se **acrescenta** um enunciado de
//! cada vez (`append`), em vez de se reescrever um ficheiro grande: se a app fechar a
//! meio de uma reunião de uma hora, o que já estava transcrito está lá. Uma linha
//! cortada a meio (o último `append` interrompido) é ignorada na leitura.
//!
//! Tudo aqui é bloqueante (disco): quem chama usa `spawn_blocking`.

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::capture::Track;

/// Como se gravou.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Microfone e sistema: uma chamada ou uma reunião online.
    Meeting,
    /// Só o sistema: um vídeo, uma aula online.
    SystemOnly,
    /// Só o microfone: uma reunião ou aula presencial.
    InPerson,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub id: String,
    pub title: String,
    /// RFC 3339, hora local (é o que o utilizador vê).
    pub started_at: String,
    pub ended_at: Option<String>,
    pub mode: Mode,
    pub model: String,
    pub cost_usd: f64,
    /// Áudio enviado a transcrever.
    pub audio_ms: u64,
    /// Acabou por a app fechar (ou falhar) a meio, não por parar.
    pub interrupted: bool,
    pub finished: bool,
    /// Como se chama cada faixa no texto; o utilizador pode mudar ("Eu" → "Manuel").
    pub mic_name: String,
    pub system_name: String,
}

impl Meta {
    pub fn speaker(&self, track: Track) -> &str {
        match track {
            Track::Mic => &self.mic_name,
            Track::System => &self.system_name,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Line {
    pub track: Track,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// O pedido falhou: o texto é um marcador, não fala.
    #[serde(default)]
    pub failed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("identificador de sessão inválido")]
    BadId,
    #[error("a sessão não existe")]
    NotFound,
    #[error("erro no disco: {0}")]
    Io(#[from] std::io::Error),
    #[error("meta.json inválido: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct NotesStore {
    dir: PathBuf,
}

/// Só dígitos e hífenes: o id vira nome de pasta, e não pode ser `..\..\x`.
fn checked(id: &str) -> Result<&str, StoreError> {
    let ok = !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_digit() || c == '-');
    if ok { Ok(id) } else { Err(StoreError::BadId) }
}

impl NotesStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("notas"),
        }
    }

    fn session_dir(&self, id: &str) -> Result<PathBuf, StoreError> {
        Ok(self.dir.join(checked(id)?))
    }

    /// Um id novo, a partir da hora local, que não colide com nenhuma pasta.
    pub fn new_id(&self, now: chrono::DateTime<chrono::Local>) -> String {
        let base = now.format("%Y%m%d-%H%M%S").to_string();
        let mut id = base.clone();
        let mut n = 1;
        while self.dir.join(&id).exists() {
            n += 1;
            id = format!("{base}-{n}");
        }
        id
    }

    pub fn create(&self, meta: &Meta) -> Result<(), StoreError> {
        fs::create_dir_all(self.session_dir(&meta.id)?)?;
        self.save_meta(meta)
    }

    /// Escreve para um ficheiro ao lado e renomeia: um `meta.json` nunca fica a meio.
    pub fn save_meta(&self, meta: &Meta) -> Result<(), StoreError> {
        let dir = self.session_dir(&meta.id)?;
        let tmp = dir.join("meta.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
        fs::rename(tmp, dir.join("meta.json"))?;
        Ok(())
    }

    pub fn append(&self, id: &str, line: &Line) -> Result<(), StoreError> {
        let path = self.session_dir(id)?.join("transcript.jsonl");
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut bytes = serde_json::to_vec(line)?;
        bytes.push(b'\n');
        // Um só `write_all`: a linha entra inteira ou (num corte) fica truncada.
        file.write_all(&bytes)?;
        Ok(())
    }

    pub fn meta(&self, id: &str) -> Result<Meta, StoreError> {
        let path = self.session_dir(id)?.join("meta.json");
        match fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    /// As linhas pela ordem de chegada. Linhas ilegíveis (a última, cortada) saltam-se.
    pub fn lines(&self, id: &str) -> Result<Vec<Line>, StoreError> {
        let path = self.session_dir(id)?.join("transcript.jsonl");
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        Ok(BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str::<Line>(&l).ok())
            .collect())
    }

    /// Todas as sessões, a mais recente primeiro. Uma pasta sem `meta.json` válido é ignorada.
    pub fn list(&self) -> Vec<Meta> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut all: Vec<Meta> = entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|id| self.meta(&id).ok())
            .collect();
        all.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(b.id.cmp(&a.id)));
        all
    }

    /// O resumo guardado ao lado da transcrição (`summary.md`), para não se pagar duas vezes.
    pub fn save_summary(&self, id: &str, text: &str) -> Result<(), StoreError> {
        fs::write(self.session_dir(id)?.join("summary.md"), text)?;
        Ok(())
    }

    pub fn summary(&self, id: &str) -> Result<Option<String>, StoreError> {
        match fs::read_to_string(self.session_dir(id)?.join("summary.md")) {
            Ok(t) => Ok(Some(t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn delete(&self, id: &str) -> Result<(), StoreError> {
        let dir = self.session_dir(id)?;
        match fs::remove_dir_all(dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    /// Sessões que ficaram abertas (a app fechou ou falhou a meio): fecham-se como
    /// "interrompidas", com o que já estava transcrito. `skip`: a que está a gravar agora.
    pub fn recover(&self, skip: Option<&str>) -> Vec<String> {
        let mut fixed = Vec::new();
        for mut meta in self.list() {
            if meta.finished || skip == Some(meta.id.as_str()) {
                continue;
            }
            meta.finished = true;
            meta.interrupted = true;
            if meta.ended_at.is_none() {
                // O fim real não se sabe: o último enunciado é o melhor palpite.
                meta.ended_at = Some(meta.started_at.clone());
            }
            if self.save_meta(&meta).is_ok() {
                fixed.push(meta.id);
            }
        }
        fixed
    }
}

// ---------------------------------------------------------------- texto

/// As linhas prontas a mostrar ou exportar: por ordem de tempo, e (numa reunião) sem
/// o eco. Sem auscultadores o microfone apanha o que sai das colunas e a mesma frase
/// aparece nas duas faixas; fica a do sistema, que é a limpa.
pub fn for_display(lines: &[Line], mode: Mode) -> Vec<Line> {
    let mut sorted: Vec<Line> = lines.to_vec();
    sorted.sort_by_key(|l| (l.start_ms, l.end_ms));
    if mode != Mode::Meeting {
        return sorted;
    }
    let system: Vec<&Line> = sorted
        .iter()
        .filter(|l| l.track == Track::System && !l.failed)
        .collect();
    let keep: Vec<bool> = sorted
        .iter()
        .map(|l| !(l.track == Track::Mic && system.iter().any(|s| is_echo(l, s))))
        .collect();
    sorted
        .into_iter()
        .zip(keep)
        .filter_map(|(l, k)| k.then_some(l))
        .collect()
}

fn words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// `mic` é eco de `system` se se sobrepõem no tempo (com folga: o som demora a chegar
/// ao microfone e cada faixa corta à sua maneira) e dizem quase as mesmas palavras.
fn is_echo(mic: &Line, system: &Line) -> bool {
    const SLACK_MS: u64 = 3_000;
    const MIN_WORDS: usize = 3;
    let overlaps =
        mic.start_ms < system.end_ms + SLACK_MS && system.start_ms < mic.end_ms + SLACK_MS;
    if !overlaps || mic.failed {
        return false;
    }
    let (a, b) = (words(&mic.text), words(&system.text));
    if a.len() < MIN_WORDS {
        return false;
    }
    let common = a.intersection(&b).count();
    // Quase todas as palavras do microfone existem no sistema.
    common as f32 / a.len() as f32 >= 0.75
}

fn stamp(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// Junta falas seguidas do mesmo orador num só parágrafo, com a hora da primeira.
fn turns<'a>(meta: &'a Meta, lines: &[Line]) -> Vec<(&'a str, u64, String)> {
    let mut out: Vec<(&str, u64, String)> = Vec::new();
    for l in lines {
        let who = meta.speaker(l.track);
        let text = if l.failed {
            "[trecho não transcrito]".to_owned()
        } else {
            l.text.trim().to_owned()
        };
        if text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some((w, _, t)) if *w == who => {
                t.push(' ');
                t.push_str(&text);
            }
            _ => out.push((who, l.start_ms, text)),
        }
    }
    out
}

pub fn export_markdown(meta: &Meta, lines: &[Line]) -> String {
    let mut out = format!("# {}\n\n{}\n\n", meta.title, meta.started_at);
    for (who, at, text) in turns(meta, lines) {
        out.push_str(&format!("**{who}** ({}): {text}\n\n", stamp(at)));
    }
    out
}

pub fn export_text(meta: &Meta, lines: &[Line]) -> String {
    let mut out = format!("{}\n{}\n\n", meta.title, meta.started_at);
    for (who, at, text) in turns(meta, lines) {
        out.push_str(&format!("[{}] {who}: {text}\n\n", stamp(at)));
    }
    out
}

/// O texto corrido para resumir: falas seguidas por "Nome: …", sem horas.
pub fn plain_for_summary(meta: &Meta, lines: &[Line]) -> String {
    turns(meta, lines)
        .into_iter()
        .map(|(who, _, text)| format!("{who}: {text}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str) -> Meta {
        Meta {
            id: id.into(),
            title: "Reunião".into(),
            started_at: "2026-10-12T10:00:00+01:00".into(),
            ended_at: None,
            mode: Mode::Meeting,
            model: "m".into(),
            cost_usd: 0.0,
            audio_ms: 0,
            interrupted: false,
            finished: false,
            mic_name: "Eu".into(),
            system_name: "Outros".into(),
        }
    }

    fn line(track: Track, start: u64, end: u64, text: &str) -> Line {
        Line {
            track,
            start_ms: start,
            end_ms: end,
            text: text.into(),
            failed: false,
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ditok-notes-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn session_state_roundtrip_and_listing() {
        let dir = tmp("list");
        let store = NotesStore::new(&dir);
        let mut a = meta("20261012-100000");
        let mut b = meta("20261012-110000");
        a.started_at = "2026-10-12T10:00:00+01:00".into();
        b.started_at = "2026-10-12T11:00:00+01:00".into();
        store.create(&a).unwrap();
        store.create(&b).unwrap();
        let list = store.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, b.id, "a mais recente primeiro");
        store.delete(&a.id).unwrap();
        assert_eq!(store.list().len(), 1);
        assert!(matches!(store.delete(&a.id), Err(StoreError::NotFound)));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn transcript_append_survives_crash() {
        let dir = tmp("crash");
        let store = NotesStore::new(&dir);
        let m = meta("20261012-100000");
        store.create(&m).unwrap();
        store
            .append(&m.id, &line(Track::Mic, 0, 1000, "olá"))
            .unwrap();
        store
            .append(&m.id, &line(Track::System, 1500, 2500, "bom dia"))
            .unwrap();
        // O processo morreu a meio da terceira linha.
        let path = dir.join("notas").join(&m.id).join("transcript.jsonl");
        let mut f = OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(br#"{"track":"mic","start_ms":3000,"end_m"#)
            .unwrap();
        let lines = store.lines(&m.id).unwrap();
        assert_eq!(lines.len(), 2, "a linha cortada ignora-se");
        assert_eq!(lines[1].text, "bom dia");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn orphan_session_detected() {
        let dir = tmp("orphan");
        let store = NotesStore::new(&dir);
        let running = meta("20261012-100000");
        let orphan = meta("20261012-110000");
        let mut done = meta("20261012-120000");
        done.finished = true;
        for m in [&running, &orphan, &done] {
            store.create(m).unwrap();
        }
        let fixed = store.recover(Some(&running.id));
        assert_eq!(fixed, vec![orphan.id.clone()]);
        let m = store.meta(&orphan.id).unwrap();
        assert!(m.finished && m.interrupted);
        assert!(
            !store.meta(&running.id).unwrap().finished,
            "a ativa não se toca"
        );
        assert!(!store.meta(&done.id).unwrap().interrupted);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn ids_cannot_escape_the_folder() {
        let store = NotesStore::new(Path::new("x"));
        for bad in ["", "..", "../a", "a/b", "a\\b", "20261012-10000x"] {
            assert!(matches!(store.meta(bad), Err(StoreError::BadId)), "{bad}");
        }
    }

    #[test]
    fn new_id_never_collides() {
        let dir = tmp("ids");
        let store = NotesStore::new(&dir);
        let now = chrono::Local::now();
        let a = store.new_id(now);
        store.create(&meta(&a)).unwrap();
        let b = store.new_id(now);
        assert_ne!(a, b);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn echo_dedupe_drops_similar_overlapping_mic_segment() {
        let lines = vec![
            line(
                Track::System,
                10_000,
                14_000,
                "vamos rever o orçamento do próximo trimestre",
            ),
            line(
                Track::Mic,
                10_600,
                14_300,
                "vamos rever o orçamento do próximo trimestre",
            ),
            line(Track::Mic, 20_000, 22_000, "concordo com isso"),
        ];
        let shown = for_display(&lines, Mode::Meeting);
        assert_eq!(shown.len(), 2);
        assert!(
            shown
                .iter()
                .all(|l| l.text != lines[1].text || l.track == Track::System)
        );
    }

    #[test]
    fn echo_dedupe_keeps_distinct_or_distant_speech() {
        let lines = vec![
            line(
                Track::System,
                10_000,
                14_000,
                "vamos rever o orçamento do próximo trimestre",
            ),
            // A mesma frase, mas muito depois: fala mesmo repetida por ti.
            line(
                Track::Mic,
                60_000,
                64_000,
                "vamos rever o orçamento do próximo trimestre",
            ),
            // Ao mesmo tempo mas a dizer outra coisa (falaram os dois).
            line(Track::Mic, 10_500, 13_000, "sim mas primeiro as férias"),
            // Muito curta: não se arrisca a apagar um "sim, sim".
            line(Track::Mic, 11_000, 11_500, "sim"),
        ];
        assert_eq!(for_display(&lines, Mode::Meeting).len(), 4);
        // Fora de uma reunião não se mexe em nada.
        assert_eq!(for_display(&lines, Mode::InPerson).len(), 4);
    }

    #[test]
    fn export_markdown_groups_by_speaker() {
        let m = meta("20261012-100000");
        let lines = vec![
            line(Track::Mic, 1_000, 2_000, "Bom dia."),
            line(Track::Mic, 3_000, 4_000, "Vamos começar."),
            line(Track::System, 65_000, 66_000, "Bom dia a todos."),
        ];
        let md = export_markdown(&m, &lines);
        assert!(md.starts_with("# Reunião"));
        assert!(
            md.contains("**Eu** (00:01): Bom dia. Vamos começar."),
            "{md}"
        );
        assert!(md.contains("**Outros** (01:05): Bom dia a todos."), "{md}");
        assert_eq!(md.matches("**Eu**").count(), 1);
    }

    #[test]
    fn failed_chunks_are_marked_in_exports() {
        let m = meta("20261012-100000");
        let mut l = line(Track::System, 0, 1000, "");
        l.failed = true;
        assert!(export_text(&m, &[l]).contains("[trecho não transcrito]"));
    }

    #[test]
    fn hour_long_timestamps_include_hours() {
        assert_eq!(stamp(3_725_000), "1:02:05");
        assert_eq!(stamp(59_000), "00:59");
    }
}
