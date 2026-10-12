//! Importar e exportar o dicionário e as expansões, para passar de um computador
//! para outro ou para trazer uma lista que já tens.
//!
//! Três formatos:
//! - **JSON** (o do Ditok): `{ "version": 1, "dictionary": [...], "snippets": [...] }`;
//! - **CSV**: `tipo;de;para` (o `tipo` é `dicionario` ou `expansao`). Também se
//!   importa um CSV de duas colunas (`de;para`, só palavras) ou de uma (só a palavra).
//!   Exporta-se com `;` e BOM, que é o que o Excel português abre bem;
//! - **TXT**: uma palavra por linha (só vocabulário: ensina a palavra ao modelo e
//!   acerta as maiúsculas, sem variantes).
//!
//! Tudo aqui é puro (bytes dentro, bytes fora): nada de ficheiros nem de janelas,
//! por isso testa-se sem a app. Os diálogos e a gravação ficam em `commands.rs`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    config::{MAX_ENTRIES, MAX_PHRASE_CHARS, MAX_SNIPPET_CHARS},
    text::{Replacement, Snippet},
};

/// Maior ficheiro que se lê. Uma lista de 1000 entradas fica muito abaixo disto.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

const BOM: &str = "\u{feff}";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Json,
    Csv,
    Txt,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Csv => "csv",
            Self::Txt => "txt",
        }
    }

    /// O formato pelo fim do nome do ficheiro (sem o ponto, sem distinguir maiúsculas).
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "json" => Some(Self::Json),
            "csv" => Some(Self::Csv),
            "txt" => Some(Self::Txt),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("o ficheiro tem mais de 2 MB")]
    TooBig,
    #[error("o ficheiro não está em UTF-8")]
    NotUtf8,
    #[error("JSON inválido: {0}")]
    Json(String),
    #[error("o ficheiro é de uma versão mais recente do Ditok (versão {0}): atualiza a app")]
    Version(u32),
    #[error("CSV inválido: há aspas que não fecham")]
    Csv,
    #[error("o ficheiro não tem nenhuma entrada")]
    Empty,
    #[error("com estas entradas passavas das {MAX_ENTRIES} permitidas em cada lista")]
    TooMany,
}

/// O que um ficheiro traz, ainda sem o comparar com o que já tens.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub dictionary: Vec<Replacement>,
    pub snippets: Vec<Snippet>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JsonFile {
    version: u32,
    #[serde(default)]
    dictionary: Vec<Replacement>,
    #[serde(default)]
    snippets: Vec<Snippet>,
}

const JSON_VERSION: u32 = 1;

// ---------------------------------------------------------------- ler

pub fn parse(bytes: &[u8], format: Format) -> Result<Parsed, IoError> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(IoError::TooBig);
    }
    let text = decode(bytes, format)?;
    let text = text.strip_prefix(BOM).unwrap_or(&text);
    let parsed = match format {
        Format::Json => parse_json(text)?,
        Format::Csv => parse_csv(text)?,
        Format::Txt => parse_txt(text),
    };
    if parsed.dictionary.is_empty() && parsed.snippets.is_empty() {
        return Err(IoError::Empty);
    }
    Ok(parsed)
}

/// O JSON tem de ser UTF-8. Um CSV ou TXT gravado pelo Excel em "ANSI" não é
/// UTF-8; nesse caso cada byte lê-se como a letra de mesmo número (Latin-1), o
/// que acerta nos acentos do português.
fn decode(bytes: &[u8], format: Format) -> Result<String, IoError> {
    match String::from_utf8(bytes.to_vec()) {
        Ok(s) => Ok(s),
        Err(_) if format == Format::Json => Err(IoError::NotUtf8),
        Err(_) => Ok(bytes.iter().map(|&b| char::from(b)).collect()),
    }
}

fn parse_json(text: &str) -> Result<Parsed, IoError> {
    let file: JsonFile = serde_json::from_str(text).map_err(|e| IoError::Json(e.to_string()))?;
    if file.version > JSON_VERSION {
        return Err(IoError::Version(file.version));
    }
    Ok(Parsed {
        dictionary: file.dictionary,
        snippets: file.snippets,
    })
}

fn parse_txt(text: &str) -> Parsed {
    let dictionary = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|to| Replacement {
            from: String::new(),
            to: to.to_owned(),
        })
        .collect();
    Parsed {
        dictionary,
        snippets: Vec::new(),
    }
}

fn parse_csv(text: &str) -> Result<Parsed, IoError> {
    let rows = csv_rows(text)?;
    let mut rows = rows.into_iter().peekable();
    let cell = |row: &[String], i: usize| row.get(i).map_or("", |c| c.trim()).to_owned();

    // Cabeçalho do Ditok (`tipo;de;para`): a primeira coluna diz de que lista é.
    let with_kind = rows
        .peek()
        .is_some_and(|r| matches!(cell(r, 0).to_lowercase().as_str(), "tipo" | "kind" | "type"));
    if with_kind || rows.peek().is_some_and(|r| is_pair_header(r)) {
        rows.next();
    }

    let mut parsed = Parsed::default();
    for row in rows {
        if with_kind {
            let kind = cell(&row, 0).to_lowercase();
            let (a, b) = (cell(&row, 1), cell(&row, 2));
            if kind.starts_with("exp") || kind.starts_with("snip") {
                parsed.snippets.push(Snippet {
                    trigger: a,
                    text: b,
                });
            } else {
                parsed.dictionary.push(Replacement { from: a, to: b });
            }
        } else if row.len() == 1 {
            // Uma coluna só: a palavra, como num TXT.
            parsed.dictionary.push(Replacement {
                from: String::new(),
                to: cell(&row, 0),
            });
        } else {
            parsed.dictionary.push(Replacement {
                from: cell(&row, 0),
                to: cell(&row, 1),
            });
        }
    }
    Ok(parsed)
}

/// Uma primeira linha que só dá nome às colunas ("de;para", "from;to"…).
fn is_pair_header(row: &[String]) -> bool {
    let name = |i: usize| {
        row.get(i)
            .map_or(String::new(), |c| c.trim().to_lowercase())
    };
    row.len() == 2
        && matches!(name(0).as_str(), "de" | "from" | "como sai" | "palavra")
        && matches!(name(1).as_str(), "para" | "to" | "como deve ficar")
}

/// O separador: o que aparecer mais vezes (fora de aspas) na primeira linha;
/// `;` se não houver nenhum ou em caso de empate (é o do Excel português).
fn detect_delimiter(text: &str) -> char {
    let (mut semicolons, mut commas, mut quoted) = (0, 0, false);
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            '\n' if !quoted => break,
            ';' if !quoted => semicolons += 1,
            ',' if !quoted => commas += 1,
            _ => {}
        }
    }
    if commas > semicolons { ',' } else { ';' }
}

/// Um CSV em linhas de células. Uma célula entre aspas pode ter o separador, mudanças
/// de linha e aspas duplas (`""`). As linhas em branco ignoram-se.
fn csv_rows(text: &str) -> Result<Vec<Vec<String>>, IoError> {
    let delimiter = detect_delimiter(text);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' if quoted => {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            }
            // Só abre aspas no princípio de uma célula: `5" ecrã` não é uma citação.
            '"' if field.is_empty() => quoted = true,
            c if c == delimiter && !quoted => row.push(std::mem::take(&mut field)),
            '\r' if !quoted => {}
            '\n' if !quoted => {
                row.push(std::mem::take(&mut field));
                let blank = row.len() == 1 && row[0].trim().is_empty();
                if !blank {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            c => field.push(c),
        }
    }
    if quoted {
        return Err(IoError::Csv);
    }
    row.push(field);
    if !(row.len() == 1 && row[0].trim().is_empty()) {
        rows.push(row);
    }
    Ok(rows)
}

// ---------------------------------------------------------------- escrever

pub fn export(
    dictionary: &[Replacement],
    snippets: &[Snippet],
    format: Format,
    with_snippets: bool,
) -> Vec<u8> {
    match format {
        Format::Json => {
            let file = JsonFile {
                version: JSON_VERSION,
                dictionary: dictionary.to_vec(),
                snippets: if with_snippets {
                    snippets.to_vec()
                } else {
                    Vec::new()
                },
            };
            // Só strings: serializar não falha.
            serde_json::to_vec_pretty(&file).unwrap_or_default()
        }
        Format::Csv => {
            let mut out = String::from(BOM);
            out.push_str("tipo;de;para\r\n");
            let mut line = |kind: &str, a: &str, b: &str| {
                out.push_str(&format!("{kind};{};{}\r\n", csv_cell(a), csv_cell(b)));
            };
            for r in dictionary {
                line("dicionario", &r.from, &r.to);
            }
            if with_snippets {
                for s in snippets {
                    line("expansao", &s.trigger, &s.text);
                }
            }
            out.into_bytes()
        }
        Format::Txt => {
            let mut seen = std::collections::HashSet::new();
            let mut out = String::from("# Vocabulário do Ditok: uma palavra por linha.\n");
            for r in dictionary {
                let word = r.to.trim();
                if !word.is_empty() && seen.insert(word.to_lowercase()) {
                    out.push_str(word);
                    out.push('\n');
                }
            }
            out.into_bytes()
        }
    }
}

/// Uma célula de CSV: entre aspas se tiver o separador, aspas, mudança de linha ou
/// espaços nas pontas (que um leitor poderia cortar).
fn csv_cell(value: &str) -> String {
    let needs_quotes = value.contains([';', ',', '"', '\n', '\r']) || value != value.trim();
    if needs_quotes {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

// ---------------------------------------------------------------- juntar

/// Quando uma entrada do ficheiro e uma tua dizem coisas diferentes para a mesma
/// palavra ("tauri" → "Tauri" contra "tauri" → "TAURI").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportMode {
    /// Fica a tua.
    Keep,
    /// Fica a do ficheiro.
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conflict {
    /// "dictionary" ou "snippet".
    pub list: &'static str,
    /// A palavra ou frase em causa.
    pub key: String,
    pub mine: String,
    pub theirs: String,
}

/// O resultado de juntar um ficheiro às tuas listas, num dos modos.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Merged {
    pub dictionary: Vec<Replacement>,
    pub snippets: Vec<Snippet>,
    /// Entradas que não tinhas.
    pub new: usize,
    /// Entradas que já tinhas, iguais.
    pub duplicate: usize,
    /// Entradas tuas trocadas pelas do ficheiro (só no modo `Replace`).
    pub replaced: usize,
    /// Entradas do ficheiro que não servem (vazias ou compridas demais).
    pub invalid: usize,
    pub conflicts: Vec<Conflict>,
}

/// Junta o que o ficheiro traz às tuas listas, sem apagar nada que tenhas (a não ser
/// que um conflito seja decidido a favor do ficheiro).
pub fn merge(
    mine_dictionary: &[Replacement],
    mine_snippets: &[Snippet],
    incoming: &Parsed,
    mode: ImportMode,
) -> Result<Merged, IoError> {
    let mut merged = Merged::default();

    let (valid_dictionary, bad_d) = partition(&incoming.dictionary, valid_replacement);
    let (valid_snippets, bad_s) = partition(&incoming.snippets, valid_snippet);
    merged.invalid = bad_d + bad_s;

    let dict = merge_list(
        mine_dictionary,
        valid_dictionary,
        mode,
        |r| r.variants().iter().map(|v| v.to_lowercase()).collect(),
        |r| r.to.as_str(),
    );
    let snips = merge_list(
        mine_snippets,
        valid_snippets,
        mode,
        |s| {
            vec![
                s.trigger
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase(),
            ]
        },
        |s| s.text.as_str(),
    );
    if dict.items.len() > MAX_ENTRIES || snips.items.len() > MAX_ENTRIES {
        return Err(IoError::TooMany);
    }

    merged.new = dict.new + snips.new;
    merged.duplicate = dict.duplicate + snips.duplicate;
    merged.replaced = dict.replaced + snips.replaced;
    merged.conflicts.extend(
        dict.conflicts
            .into_iter()
            .map(|(key, mine, theirs)| Conflict {
                list: "dictionary",
                key,
                mine,
                theirs,
            }),
    );
    merged.conflicts.extend(
        snips
            .conflicts
            .into_iter()
            .map(|(key, mine, theirs)| Conflict {
                list: "snippet",
                key,
                mine,
                theirs,
            }),
    );
    merged.dictionary = dict.items;
    merged.snippets = snips.items;
    Ok(merged)
}

fn partition<T: Clone>(items: &[T], valid: impl Fn(&T) -> bool) -> (Vec<T>, usize) {
    let good: Vec<T> = items.iter().filter(|i| valid(i)).cloned().collect();
    let bad = items.len() - good.len();
    (good, bad)
}

fn too_long(s: &str, max: usize) -> bool {
    s.chars().count() > max
}

fn valid_replacement(r: &Replacement) -> bool {
    !r.to.trim().is_empty()
        && !too_long(&r.from, MAX_PHRASE_CHARS)
        && !too_long(&r.to, MAX_PHRASE_CHARS)
}

fn valid_snippet(s: &Snippet) -> bool {
    !s.trigger.trim().is_empty()
        && !s.text.trim().is_empty()
        && !too_long(&s.trigger, MAX_PHRASE_CHARS)
        && !too_long(&s.text, MAX_SNIPPET_CHARS)
}

struct ListMerge<T> {
    items: Vec<T>,
    new: usize,
    duplicate: usize,
    replaced: usize,
    /// (chave, o teu valor, o do ficheiro)
    conflicts: Vec<(String, String, String)>,
}

/// Junta duas listas do mesmo tipo. `keys` dá as formas que identificam uma entrada
/// (as variantes de uma palavra; a frase de uma expansão) e `value` o que ela
/// produz. Duas entradas que partilham uma chave são "a mesma": se produzem o
/// mesmo, é uma repetida; senão, é um conflito.
fn merge_list<T: Clone>(
    mine: &[T],
    incoming: Vec<T>,
    mode: ImportMode,
    keys: impl Fn(&T) -> Vec<String>,
    value: impl for<'a> Fn(&'a T) -> &'a str,
) -> ListMerge<T> {
    let mut items: Vec<Option<T>> = mine.iter().cloned().map(Some).collect();
    // Chave → posição em `items`. Se duas entradas tuas já partilhavam uma chave,
    // fica a primeira.
    let build = |items: &[Option<T>]| {
        let mut index: HashMap<String, usize> = HashMap::new();
        for (i, item) in items.iter().enumerate() {
            if let Some(item) = item {
                for key in keys(item) {
                    index.entry(key).or_insert(i);
                }
            }
        }
        index
    };
    let mut index = build(&items);
    let mut out = ListMerge {
        items: Vec::new(),
        new: 0,
        duplicate: 0,
        replaced: 0,
        conflicts: Vec::new(),
    };

    for entry in incoming {
        let entry_keys = keys(&entry);
        let mut hits: Vec<usize> = entry_keys
            .iter()
            .filter_map(|k| index.get(k).copied())
            .collect();
        hits.sort_unstable();
        hits.dedup();

        let Some(&first) = hits.first() else {
            let at = items.len();
            for key in entry_keys {
                index.entry(key).or_insert(at);
            }
            items.push(Some(entry));
            out.new += 1;
            continue;
        };

        let same = hits
            .iter()
            .all(|&i| items[i].as_ref().is_some_and(|e| value(e) == value(&entry)));
        if same {
            out.duplicate += 1;
            continue;
        }

        let mine_value = items[first]
            .as_ref()
            .map_or_else(String::new, |e| value(e).to_owned());
        out.conflicts.push((
            entry_keys.first().cloned().unwrap_or_default(),
            mine_value,
            value(&entry).to_owned(),
        ));
        if mode == ImportMode::Replace {
            for &i in &hits[1..] {
                items[i] = None;
            }
            items[first] = Some(entry);
            out.replaced += 1;
            index = build(&items);
        }
    }
    out.items = items.into_iter().flatten().collect();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(from: &str, to: &str) -> Replacement {
        Replacement {
            from: from.into(),
            to: to.into(),
        }
    }

    fn s(trigger: &str, text: &str) -> Snippet {
        Snippet {
            trigger: trigger.into(),
            text: text.into(),
        }
    }

    fn parsed(dictionary: Vec<Replacement>, snippets: Vec<Snippet>) -> Parsed {
        Parsed {
            dictionary,
            snippets,
        }
    }

    #[test]
    fn csv_autodetects_semicolon() {
        let p = parse(
            "tipo;de;para\ndicionario;tauri, tori;Tauri\nexpansao;o meu email;eu@x.pt\n".as_bytes(),
            Format::Csv,
        )
        .unwrap();
        assert_eq!(p.dictionary, [r("tauri, tori", "Tauri")]);
        assert_eq!(p.snippets, [s("o meu email", "eu@x.pt")]);
    }

    #[test]
    fn csv_autodetects_comma_and_skips_the_pair_header() {
        let p = parse(b"de,para\ntauri,Tauri\nreact,React\n", Format::Csv).unwrap();
        assert_eq!(p.dictionary, [r("tauri", "Tauri"), r("react", "React")]);
    }

    #[test]
    fn csv_with_bom_and_quotes() {
        let text = "\u{feff}tipo;de;para\r\ndicionario;\"a;b\";\"Diz \"\"olá\"\"\"\r\nexpansao;morada;\"Rua A, 1\n1000-001 Lisboa\"\r\n";
        let p = parse(text.as_bytes(), Format::Csv).unwrap();
        assert_eq!(p.dictionary, [r("a;b", "Diz \"olá\"")]);
        assert_eq!(p.snippets, [s("morada", "Rua A, 1\n1000-001 Lisboa")]);
    }

    #[test]
    fn csv_with_one_column_is_vocabulary() {
        let p = parse(b"Tauri\nManuel Brito\n\n", Format::Csv).unwrap();
        assert_eq!(p.dictionary, [r("", "Tauri"), r("", "Manuel Brito")]);
    }

    #[test]
    fn csv_roundtrip_with_commas_in_from() {
        let dictionary = [
            r("tauri, tori", "Tauri"),
            r("", "Manuel Brito"),
            r("quote\"", "Q"),
        ];
        let snippets = [
            s("morada", "Rua A, 1\n1000-001 Lisboa"),
            s("com espaço", "x"),
        ];
        let bytes = export(&dictionary, &snippets, Format::Csv, true);
        assert!(bytes.starts_with(BOM.as_bytes()), "o Excel precisa do BOM");
        let back = parse(&bytes, Format::Csv).unwrap();
        assert_eq!(back.dictionary, dictionary);
        assert_eq!(back.snippets, snippets);
    }

    #[test]
    fn csv_in_latin1_is_read_by_byte() {
        // "ação" em Windows-1252: a, ç = 0xE7, ã = 0xE3, o
        let bytes = b"a\xE7\xE3o;A\xE7\xE3o\n";
        let p = parse(bytes, Format::Csv).unwrap();
        assert_eq!(p.dictionary, [r("ação", "Ação")]);
        // Já o JSON tem de ser UTF-8.
        assert!(matches!(
            parse(b"{\"version\":1,\"x\":\"\xE7\"}", Format::Json),
            Err(IoError::NotUtf8)
        ));
    }

    #[test]
    fn csv_with_unclosed_quotes_is_rejected() {
        assert!(matches!(parse(b"a;\"b\n", Format::Csv), Err(IoError::Csv)));
    }

    #[test]
    fn txt_makes_vocabulary_entries() {
        let p = parse(b"# comentario\nTauri\n\n  Manuel Brito  \n", Format::Txt).unwrap();
        assert_eq!(p.dictionary, [r("", "Tauri"), r("", "Manuel Brito")]);
        assert!(p.snippets.is_empty());
    }

    #[test]
    fn txt_export_lists_each_word_once() {
        let bytes = export(
            &[r("tauri", "Tauri"), r("tori", "tauri"), r("", "React")],
            &[s("a", "b")],
            Format::Txt,
            true,
        );
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text.lines().skip(1).collect::<Vec<_>>(), ["Tauri", "React"]);
    }

    #[test]
    fn json_roundtrip_and_version_check() {
        let bytes = export(&[r("a", "A")], &[s("x", "y")], Format::Json, true);
        let back = parse(&bytes, Format::Json).unwrap();
        assert_eq!(back, parsed(vec![r("a", "A")], vec![s("x", "y")]));
        assert!(matches!(
            parse(b"{\"version\":2,\"dictionary\":[]}", Format::Json),
            Err(IoError::Version(2))
        ));
        assert!(matches!(
            parse(b"[1,2]", Format::Json),
            Err(IoError::Json(_))
        ));
    }

    #[test]
    fn export_without_snippets_by_default_option() {
        for format in [Format::Json, Format::Csv] {
            let bytes = export(
                &[r("a", "A")],
                &[s("o meu email", "segredo@x.pt")],
                format,
                false,
            );
            let text = String::from_utf8(bytes).unwrap();
            assert!(!text.contains("segredo"), "{format:?}");
            assert!(text.contains('A'));
        }
    }

    #[test]
    fn empty_and_oversized_files_are_rejected() {
        assert!(matches!(parse(b"", Format::Txt), Err(IoError::Empty)));
        assert!(matches!(
            parse(b"# so um comentario\n", Format::Txt),
            Err(IoError::Empty)
        ));
        let big = vec![b'a'; MAX_FILE_BYTES as usize + 1];
        assert!(matches!(parse(&big, Format::Txt), Err(IoError::TooBig)));
    }

    #[test]
    fn merge_adds_new_and_skips_duplicates() {
        let mine = [r("tauri, tori", "Tauri")];
        let incoming = parsed(
            vec![r("", "Tauri"), r("tori", "Tauri"), r("react", "React")],
            vec![],
        );
        let m = merge(&mine, &[], &incoming, ImportMode::Keep).unwrap();
        assert_eq!((m.new, m.duplicate, m.replaced), (1, 2, 0));
        assert!(m.conflicts.is_empty());
        assert_eq!(
            m.dictionary,
            [r("tauri, tori", "Tauri"), r("react", "React")]
        );
    }

    #[test]
    fn merge_keep_skips_conflicts() {
        let mine = [r("tauri", "Tauri")];
        let incoming = parsed(vec![r("tauri", "TAURI")], vec![]);
        let m = merge(&mine, &[], &incoming, ImportMode::Keep).unwrap();
        assert_eq!(m.dictionary, mine);
        assert_eq!(m.replaced, 0);
        assert_eq!(
            m.conflicts,
            [Conflict {
                list: "dictionary",
                key: "tauri".into(),
                mine: "Tauri".into(),
                theirs: "TAURI".into()
            }]
        );
    }

    #[test]
    fn merge_replace_overrides() {
        let mine = [r("tauri", "Tauri"), r("react", "React")];
        let incoming = parsed(vec![r("tauri", "TAURI")], vec![]);
        let m = merge(&mine, &[], &incoming, ImportMode::Replace).unwrap();
        assert_eq!(m.dictionary, [r("tauri", "TAURI"), r("react", "React")]);
        assert_eq!((m.replaced, m.conflicts.len()), (1, 1));
    }

    #[test]
    fn merge_matches_snippets_by_phrase_ignoring_spacing_and_case() {
        let mine = [s("o meu  email", "a@x.pt")];
        let incoming = parsed(
            vec![],
            vec![s("O meu email", "a@x.pt"), s("a minha morada", "Rua A")],
        );
        let m = merge(&[], &mine, &incoming, ImportMode::Keep).unwrap();
        assert_eq!((m.new, m.duplicate), (1, 1));
        assert_eq!(m.snippets.len(), 2);
    }

    #[test]
    fn merge_counts_invalid_entries() {
        let incoming = parsed(
            vec![
                r("a", "  "),
                r(&"x".repeat(MAX_PHRASE_CHARS + 1), "X"),
                r("ok", "Ok"),
            ],
            vec![s("", "texto")],
        );
        let m = merge(&[], &[], &incoming, ImportMode::Keep).unwrap();
        assert_eq!((m.new, m.invalid), (1, 3));
    }

    #[test]
    fn import_respects_the_1000_limit() {
        let mine: Vec<Replacement> = (0..MAX_ENTRIES)
            .map(|i| r("", &format!("Palavra{i}")))
            .collect();
        let incoming = parsed(vec![r("", "Nova")], vec![]);
        assert!(matches!(
            merge(&mine, &[], &incoming, ImportMode::Keep),
            Err(IoError::TooMany)
        ));
        // Repetidas não contam.
        let same = parsed(vec![r("", "Palavra3")], vec![]);
        assert!(merge(&mine, &[], &same, ImportMode::Keep).is_ok());
    }

    #[test]
    fn merge_dedupes_inside_the_file_too() {
        let incoming = parsed(vec![r("", "Tauri"), r("tauri", "Tauri")], vec![]);
        let m = merge(&[], &[], &incoming, ImportMode::Keep).unwrap();
        assert_eq!((m.new, m.duplicate), (1, 1));
    }

    #[test]
    fn extensions_map_to_formats() {
        assert_eq!(Format::from_extension("CSV"), Some(Format::Csv));
        assert_eq!(Format::from_extension("json"), Some(Format::Json));
        assert_eq!(Format::from_extension("xlsx"), None);
    }
}
