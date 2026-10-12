//! O que se faz ao texto neste computador, entre a transcrição e a colagem:
//!
//! 1. **Comandos falados**: "nova linha" e "novo parágrafo" viram mudanças de linha e
//!    "vírgula", "ponto de interrogação", "abre aspas"… viram a pontuação (e as
//!    mesmas frases em inglês).
//! 2. **Expansões**: uma frase ("o meu email") dá lugar a um texto (o endereço).
//! 3. **Dicionário**: palavras escritas à tua maneira ("tauri" → "Tauri").
//!
//! Não há rede nem modelos: são expressões regulares compiladas uma vez, quando
//! guardas as definições, e aplicadas em microssegundos.
//!
//! Com a limpeza com IA ligada, o modelo recebe o texto já com os comandos e o
//! dicionário aplicados, mas cada expansão vai trocada por um marcador (`{{1}}`):
//! o conteúdo dela (o teu email, a tua morada) nunca sai do computador, e o
//! modelo não o pode "corrigir". Depois da limpeza, os marcadores voltam a ser
//! trocados pelo texto (`Processed::merge_cleaned`).

use std::{ops::Range, sync::LazyLock};

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::config::Style;

/// Uma entrada do dicionário. `from` pode ter várias formas separadas por
/// vírgulas ("tauri, tori"); vazio quer dizer "a própria palavra", o que serve
/// para acertar maiúsculas e para a ensinar ao modelo de limpeza.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replacement {
    pub from: String,
    pub to: String,
}

impl Replacement {
    /// As formas a procurar no texto.
    fn variants(&self) -> Vec<&str> {
        let variants: Vec<&str> = self
            .from
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .collect();
        if variants.is_empty() {
            vec![self.to.trim()]
        } else {
            variants
        }
    }
}

/// Uma expansão: dizes `trigger` e é colado `text`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    pub trigger: String,
    pub text: String,
}

/// As regras prontas a usar, compiladas a partir da configuração. Vivem num
/// `Arc` no `AppState` e só são recriadas quando guardas as definições.
#[derive(Debug, Default)]
pub struct TextRules {
    /// "nova linha" e "novo parágrafo" (`Config::voice_commands`).
    lines: bool,
    /// "vírgula", "ponto de interrogação"… (`Config::spoken_punctuation`).
    punctuation: bool,
    dictionary: Option<PhraseSet>,
    /// O texto final de cada entrada do dicionário (indexado como na config).
    dictionary_to: Vec<String>,
    snippets: Option<PhraseSet>,
    snippet_text: Vec<String>,
    vocabulary: Vec<String>,
}

/// Quantas vezes cada regra foi aplicada num ditado. Vai para os logs (o texto
/// não vai).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    pub commands: usize,
    pub replaced: usize,
    pub expanded: usize,
}

/// O resultado de `TextRules::apply`.
#[derive(Debug)]
pub struct Processed {
    /// O texto pronto a colar (sem limpeza com IA).
    pub text: String,
    /// O mesmo texto com as expansões trocadas por `{{1}}`, `{{2}}`…: é isto que
    /// pode ir para o modelo de limpeza.
    for_cleanup: String,
    /// O texto de cada marcador, pela ordem em que aparecem.
    expansions: Vec<String>,
    pub applied: Applied,
}

impl TextRules {
    pub fn new(config: &crate::config::Config) -> Self {
        let dictionary = PhraseSet::new(
            config
                .dictionary
                .iter()
                .enumerate()
                .flat_map(|(i, r)| r.variants().into_iter().map(move |v| (v, i))),
        );
        let snippets = PhraseSet::new(
            config
                .snippets
                .iter()
                .enumerate()
                .map(|(i, s)| (s.trigger.trim(), i)),
        );
        let mut vocabulary: Vec<String> = Vec::new();
        for r in &config.dictionary {
            let word = r.to.trim();
            if !word.is_empty() && !vocabulary.iter().any(|w| w == word) {
                vocabulary.push(word.to_owned());
            }
        }
        Self {
            lines: config.voice_commands,
            punctuation: config.spoken_punctuation,
            dictionary,
            dictionary_to: config
                .dictionary
                .iter()
                .map(|r| r.to.trim().to_owned())
                .collect(),
            snippets,
            snippet_text: config.snippets.iter().map(|s| s.text.clone()).collect(),
            vocabulary,
        }
    }

    /// As palavras do dicionário, para o modelo de limpeza as escrever bem.
    pub fn vocabulary(&self) -> &[String] {
        &self.vocabulary
    }

    /// `apply_styled` com o estilo Predefinido (os testes não precisam de mais).
    #[cfg(test)]
    pub fn apply(&self, raw: &str) -> Processed {
        self.apply_styled(raw, Style::Default)
    }

    /// Como `apply`, sabendo o estilo do ditado. No Literal (terminal, IDE) a
    /// pontuação falada fica desligada: lá, um "ponto" ou uma "vírgula" ditados
    /// quase sempre são palavras de um comando, não pontuação.
    pub fn apply_styled(&self, raw: &str, style: Style) -> Processed {
        let mut applied = Applied::default();
        let punctuation = self.punctuation && style != Style::Literal;
        let text = if self.lines || punctuation {
            apply_spoken(raw, self.lines, punctuation, &mut applied.commands)
        } else {
            raw.to_owned()
        };

        // As expansões partem o texto em pedaços; o dicionário só mexe nos pedaços
        // de texto, nunca dentro de uma expansão (senão "manuel" → "Manuel"
        // estragava o email manuel@…).
        let mut segments = Vec::new();
        let mut last = 0;
        if let Some(set) = &self.snippets {
            for (i, range) in set.find_iter(&text) {
                segments.push(Segment::Text(&text[last..range.start]));
                segments.push(Segment::Snippet(i));
                last = range.end;
            }
        }
        segments.push(Segment::Text(&text[last..]));

        let mut out = Processed {
            text: String::with_capacity(text.len()),
            for_cleanup: String::with_capacity(text.len()),
            expansions: Vec::new(),
            applied,
        };
        for segment in segments {
            match segment {
                Segment::Text(t) => {
                    let replaced = self.replace_words(t, &mut out.applied.replaced);
                    out.text.push_str(&replaced);
                    out.for_cleanup.push_str(&replaced);
                }
                Segment::Snippet(i) => {
                    let expansion = &self.snippet_text[i];
                    out.text.push_str(expansion);
                    out.expansions.push(expansion.clone());
                    out.for_cleanup.push_str(&placeholder(out.expansions.len()));
                    out.applied.expanded += 1;
                }
            }
        }
        out
    }

    /// Só as palavras do dicionário, sem comandos nem expansões: é o que se aplica
    /// a um ficheiro transcrito (uma aula onde se diga "nova linha" ou "o meu
    /// email" não é um ditado teu).
    pub fn apply_dictionary(&self, text: &str) -> String {
        self.replace_words(text, &mut 0)
    }

    fn replace_words(&self, text: &str, count: &mut usize) -> String {
        let Some(set) = &self.dictionary else {
            return text.to_owned();
        };
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for (i, range) in set.find_iter(text) {
            out.push_str(&text[last..range.start]);
            out.push_str(&self.dictionary_to[i]);
            last = range.end;
            *count += 1;
        }
        out.push_str(&text[last..]);
        out
    }
}

enum Segment<'a> {
    Text(&'a str),
    Snippet(usize),
}

fn placeholder(n: usize) -> String {
    format!("{{{{{n}}}}}") // `{{` num `format!` é uma chaveta literal: dá "{{n}}"
}

/// Porque é que a resposta do modelo de limpeza não foi usada.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MergeError {
    #[error("o modelo mudou as mudanças de linha")]
    Lines,
    #[error("o modelo estragou o marcador de uma expansão")]
    Placeholder,
}

impl Processed {
    /// O que enviar ao modelo de limpeza: o texto sem os espaços e mudanças de
    /// linha das pontas, que o modelo apagaria (um "nova linha" no fim tem de
    /// chegar ao destino). `None` se não houver palavras para limpar (ex.: o
    /// ditado foi só "nova linha" ou só uma expansão).
    pub fn cleanup_input(&self) -> Option<&str> {
        let core = self.for_cleanup.trim();
        let text_only = (1..=self.expansions.len())
            .fold(core.to_owned(), |t, n| t.replace(&placeholder(n), ""));
        text_only.chars().any(char::is_alphabetic).then_some(core)
    }

    /// Junta a resposta do modelo de limpeza com o que ficou de fora: as pontas e
    /// o conteúdo das expansões. Se o modelo mexeu nas linhas ou num marcador,
    /// devolve erro e o pipeline cola o texto sem limpeza.
    pub fn merge_cleaned(&self, cleaned: &str) -> Result<String, MergeError> {
        let cleaned = cleaned.replace("\r\n", "\n");
        let full = self.for_cleanup.as_str();
        let core = full.trim();
        if cleaned.matches('\n').count() != core.matches('\n').count() {
            return Err(MergeError::Lines);
        }
        let mut merged = cleaned;
        for (i, expansion) in self.expansions.iter().enumerate() {
            let marker = placeholder(i + 1);
            if merged.matches(&marker).count() != 1 {
                return Err(MergeError::Placeholder);
            }
            merged = merged.replacen(&marker, expansion, 1);
        }
        // `trim` só tira das pontas: o que tirou está antes e depois do `core`.
        let start = full.len() - full.trim_start().len();
        let end = full.trim_end().len();
        Ok(format!(
            "{}{merged}{}",
            &full[..start],
            &full[end.max(start)..]
        ))
    }
}

// ---------------------------------------------------------------- frases

/// Várias frases procuradas de uma só vez: um único regex com uma alternativa (e
/// um grupo de captura) por frase. O regex diz onde está a ocorrência; o grupo
/// que apanhou diz qual foi a frase.
#[derive(Debug)]
struct PhraseSet {
    regex: Regex,
    /// Para cada grupo do regex, a entrada da config a que pertence.
    entries: Vec<usize>,
}

impl PhraseSet {
    /// `phrases`: (frase, entrada da config). `None` se não houver frases.
    fn new<'a>(phrases: impl Iterator<Item = (&'a str, usize)>) -> Option<Self> {
        let mut alternatives: Vec<(String, usize, usize)> = phrases
            .filter_map(|(phrase, entry)| {
                phrase_pattern(phrase).map(|p| (p, entry, phrase.chars().count()))
            })
            .collect();
        if alternatives.is_empty() {
            return None;
        }
        // Num mesmo sítio, o regex fica com a primeira alternativa que serve: as
        // frases mais longas vêm primeiro para "manuel brito" ganhar a "manuel".
        // O `sort_by` é estável, por isso em caso de empate ganha a primeira entrada.
        alternatives.sort_by(|a, b| b.2.cmp(&a.2));
        let pattern = alternatives
            .iter()
            .map(|(p, ..)| format!("({p})"))
            .collect::<Vec<_>>()
            .join("|");
        match RegexBuilder::new(&pattern).case_insensitive(true).build() {
            Ok(regex) => Some(Self {
                regex,
                entries: alternatives
                    .into_iter()
                    .map(|(_, entry, _)| entry)
                    .collect(),
            }),
            Err(e) => {
                tracing::error!(error = %e, "não foi possível preparar o dicionário; fica desligado");
                None
            }
        }
    }

    /// As ocorrências, da esquerda para a direita: (entrada da config, posição).
    fn find_iter<'t>(&'t self, text: &'t str) -> impl Iterator<Item = (usize, Range<usize>)> + 't {
        self.regex.captures_iter(text).filter_map(|caps| {
            let whole = caps.get(0)?;
            // O grupo 0 é a ocorrência inteira; os nossos começam no 1.
            let group = caps.iter().skip(1).position(|g| g.is_some())?;
            Some((self.entries[group], whole.range()))
        })
    }
}

/// "o meu  email" → `\bo\s+meu\s+email\b`: ignora quantos espaços há entre as
/// palavras e só apanha palavras inteiras ("tauri" não apanha "tauris"). O `\b`
/// só se põe do lado onde a frase acaba numa letra ou número: "c++" não tem uma
/// fronteira de palavra depois do "+".
fn phrase_pattern(phrase: &str) -> Option<String> {
    let words: Vec<String> = phrase.split_whitespace().map(regex::escape).collect();
    let first = phrase.trim().chars().next()?;
    let last = phrase.trim().chars().next_back()?;
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    Some(format!(
        "{}{}{}",
        if is_word(first) { r"\b" } else { "" },
        words.join(r"\s+"),
        if is_word(last) { r"\b" } else { "" },
    ))
}

// ---------------------------------------------------------------- comandos

/// Onde fica o espaço à volta de uma marca de pontuação.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spacing {
    /// Cola-se à palavra de trás e dá espaço à de a seguir: `,` `?` `)` `”`.
    Close,
    /// Tem espaço antes e cola-se à palavra de a seguir: `(` `“`.
    Open,
}

#[derive(Debug, Clone, Copy)]
enum Action {
    Line,
    Paragraph,
    Mark {
        symbol: &'static str,
        spacing: Spacing,
        /// A palavra a seguir começa com maiúscula (depois de `.` `?` `!`).
        capitalize: bool,
    },
}

/// Um comando falado: a frase (em expressão regular, PT e EN) e o que faz.
struct Spoken {
    pattern: &'static str,
    action: Action,
    /// Frases que quase nunca aparecem por acaso ("ponto de interrogação") valem em
    /// qualquer sítio. As outras ("vírgula", "ponto final", "nova linha") só contam
    /// isoladas por pontuação, como no "nova linha" de sempre.
    firm: bool,
    /// Também conta quando é a última coisa do texto, mesmo sem pausa escrita antes
    /// ("…eu sou o Pedro ponto final"): é como se acaba um ditado.
    at_end: bool,
}

const fn mark(symbol: &'static str, spacing: Spacing, capitalize: bool) -> Action {
    Action::Mark {
        symbol,
        spacing,
        capitalize,
    }
}

/// A tabela. A ordem não importa: o regex escolhe a frase que começa mais cedo, e
/// "ponto e vírgula" começa antes da "vírgula" que tem lá dentro.
const SPOKEN: &[Spoken] = &[
    Spoken {
        pattern: r"ponto\s+de\s+interroga[çc][ãa]o|question\s+mark",
        action: mark("?", Spacing::Close, true),
        firm: true,
        at_end: false,
    },
    Spoken {
        pattern: r"ponto\s+de\s+exclama[çc][ãa]o|exclamation\s+(?:mark|point)",
        action: mark("!", Spacing::Close, true),
        firm: true,
        at_end: false,
    },
    Spoken {
        pattern: r"ponto\s+e\s+v[íi]rgula",
        action: mark(";", Spacing::Close, false),
        firm: true,
        at_end: false,
    },
    Spoken {
        pattern: r"abr(?:e|ir)\s+aspas|open\s+quotes?",
        action: mark("“", Spacing::Open, false),
        firm: true,
        at_end: false,
    },
    Spoken {
        pattern: r"fech(?:a|ar)\s+aspas|close\s+quotes?",
        action: mark("”", Spacing::Close, false),
        firm: true,
        at_end: false,
    },
    Spoken {
        pattern: r"abr(?:e|ir)\s+par[êe]nte(?:ses|sis)|open\s+paren(?:thesis|theses)?",
        action: mark("(", Spacing::Open, false),
        firm: true,
        at_end: false,
    },
    Spoken {
        pattern: r"fech(?:a|ar)\s+par[êe]nte(?:ses|sis)|close\s+paren(?:thesis|theses)?",
        action: mark(")", Spacing::Close, false),
        firm: true,
        at_end: false,
    },
    Spoken {
        pattern: r"v[íi]rgula|comma",
        action: mark(",", Spacing::Close, false),
        firm: false,
        at_end: false,
    },
    Spoken {
        pattern: r"ponto\s+final|full\s+stop",
        action: mark(".", Spacing::Close, true),
        firm: false,
        at_end: true,
    },
    Spoken {
        pattern: r"dois\s+pontos",
        action: mark(":", Spacing::Close, false),
        firm: false,
        at_end: false,
    },
    Spoken {
        pattern: r"retic[êe]ncias",
        action: mark("…", Spacing::Close, false),
        firm: false,
        at_end: true,
    },
    Spoken {
        pattern: r"nova\s+linha|new\s*line",
        action: Action::Line,
        firm: false,
        at_end: false,
    },
    Spoken {
        pattern: r"novo\s+par[áa]grafo|new\s+paragraph",
        action: Action::Paragraph,
        firm: false,
        at_end: false,
    },
];

/// Um regex só, com um grupo de captura por frase (como o `PhraseSet`): o grupo
/// que apanhou diz qual foi a entrada de `SPOKEN`. `LazyLock`: compilado na
/// primeira vez que é usado e partilhado daí em diante.
static SPOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    let alternatives: Vec<String> = SPOKEN.iter().map(|s| format!("({})", s.pattern)).collect();
    RegexBuilder::new(&format!(r"\b(?:{})\b", alternatives.join("|")))
        .case_insensitive(true)
        .build()
        .expect("regex dos comandos falados")
});

fn is_punct(c: char) -> bool {
    matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '…')
}

/// Onde uma pausa deixa a sua marca: pontuação, mudança de linha ou o fecho de
/// uns parênteses ou aspas ("(ver abaixo) ponto final").
fn is_boundary(c: char) -> bool {
    c == '\n' || is_punct(c) || matches!(c, ')' | '”' | '"')
}

fn is_space(c: char) -> bool {
    c.is_whitespace() && c != '\n'
}

/// Troca os comandos falados pelo que eles pedem, num só passe pelo texto:
/// - `lines`: "nova linha" / "novo parágrafo" → mudanças de linha;
/// - `punctuation`: "ponto de interrogação", "vírgula", "abre aspas"… → `?` `,` `“`.
///
/// Uma frase que não é "firme" (ver `Spoken::firm`) só conta quando está isolada:
/// entre pontuação, mudanças de linha ou as pontas do texto. Quem dita um comando
/// faz uma pausa, e os modelos marcam-na com uma vírgula ou um ponto ("Olá Pedro,
/// nova linha, tudo bem?"). Assim "abriu uma nova linha de metro" e "o ponto final
/// do autocarro" ficam escritos tal e qual.
///
/// À volta de um comando: a pontuação que o modelo pôs de antes (é só a pausa) e
/// de depois é tirada, e a frase seguinte começa com maiúscula quando o comando
/// acaba uma frase.
fn apply_spoken(text: &str, lines: bool, punctuation: bool, count: &mut usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0; // até onde o texto original já foi copiado
    let mut capitalize = false;
    // Depois de uma marca que fecha (`,` `?` `)`), a próxima palavra leva um espaço.
    let mut space_next = false;

    for caps in SPOKEN_RE.captures_iter(text) {
        let Some(m) = caps.get(0) else { continue };
        // O grupo 0 é a ocorrência inteira; os das frases começam no 1.
        let Some(entry) = caps.iter().skip(1).position(|g| g.is_some()) else {
            continue;
        };
        let spoken = &SPOKEN[entry];
        let enabled = match spoken.action {
            Action::Line | Action::Paragraph => lines,
            Action::Mark { .. } => punctuation,
        };
        if !enabled || m.start() < last {
            continue;
        }
        if !spoken.firm {
            let before = text[..m.start()].trim_end_matches(is_space);
            let after = text[m.end()..].trim_start_matches(is_space);
            let isolated_before = before.is_empty() || before.ends_with(is_boundary);
            let isolated_after = after.is_empty() || after.starts_with(is_boundary);
            // Um ditado acaba muitas vezes em "ponto final", sem pausa antes.
            let ends_the_text = spoken.at_end && after.is_empty();
            if !(isolated_before || ends_the_text) || !isolated_after {
                continue;
            }
        }

        push_text(
            &mut out,
            &text[last..m.start()],
            &mut capitalize,
            &mut space_next,
        );
        trim_end_spaces(&mut out);
        match spoken.action {
            Action::Line | Action::Paragraph => {
                if out.ends_with(',') {
                    out.pop();
                    trim_end_spaces(&mut out);
                }
                out.push_str(if matches!(spoken.action, Action::Line) {
                    "\n"
                } else {
                    "\n\n"
                });
                capitalize = true;
                space_next = false;
            }
            Action::Mark {
                symbol,
                spacing: Spacing::Close,
                capitalize: capital,
            } => {
                // A pausa que o modelo marcou antes ("tudo bem, ponto de interrogação")
                // é só isso: a marca falada substitui-a.
                while out.ends_with([',', ';', ':', '.']) {
                    out.pop();
                    trim_end_spaces(&mut out);
                }
                // Se o modelo já escreveu o "?" não se repete; os parênteses e as
                // aspas, sim, podem vir seguidos.
                let repeats = matches!(symbol, "?" | "!" | "…") && out.ends_with(symbol);
                if !repeats {
                    out.push_str(symbol);
                }
                capitalize |= capital;
                space_next = true;
            }
            Action::Mark {
                symbol,
                spacing: Spacing::Open,
                ..
            } => {
                if !out.is_empty() && !out.ends_with(['\n', '(', '“']) {
                    out.push(' ');
                }
                out.push_str(symbol);
                space_next = false;
            }
        }
        *count += 1;

        let rest = &text[m.end()..];
        let skipped = rest.len()
            - rest
                .trim_start_matches(|c| is_space(c) || is_punct(c))
                .len();
        last = m.end() + skipped;
    }
    push_text(&mut out, &text[last..], &mut capitalize, &mut space_next);
    out
}

/// Acrescenta um bocado de texto original a `out`, com o espaço que uma marca
/// anterior pede (`space_next`) e a maiúscula que um comando pediu (`capitalize`).
fn push_text(out: &mut String, chunk: &str, capitalize: &mut bool, space_next: &mut bool) {
    if !*space_next {
        push_capitalized(out, chunk, capitalize);
        return;
    }
    // O espaço que lá estava é substituído por um só (ou nenhum).
    let chunk = chunk.trim_start_matches(is_space);
    // Se só havia espaços, o pedido de espaço fica para o próximo bocado.
    if chunk.is_empty() {
        return;
    }
    if !chunk.starts_with('\n') {
        out.push(' ');
    }
    *space_next = false;
    push_capitalized(out, chunk, capitalize);
}

fn trim_end_spaces(s: &mut String) {
    s.truncate(s.trim_end_matches(is_space).len());
}

/// Acrescenta `s`, pondo em maiúscula a primeira letra se `capitalize` estiver
/// ligado. Fica ligado enquanto só aparecerem espaços (ex.: dois comandos seguidos).
fn push_capitalized(out: &mut String, s: &str, capitalize: &mut bool) {
    if !*capitalize {
        out.push_str(s);
        return;
    }
    let mut chars = s.char_indices();
    for (i, c) in chars.by_ref() {
        if c.is_whitespace() {
            out.push(c);
            continue;
        }
        *capitalize = false;
        out.extend(c.to_uppercase());
        out.push_str(&s[i + c.len_utf8()..]);
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn rules(dictionary: &[(&str, &str)], snippets: &[(&str, &str)]) -> TextRules {
        TextRules::new(&Config {
            dictionary: dictionary
                .iter()
                .map(|(from, to)| Replacement {
                    from: (*from).into(),
                    to: (*to).into(),
                })
                .collect(),
            snippets: snippets
                .iter()
                .map(|(trigger, text)| Snippet {
                    trigger: (*trigger).into(),
                    text: (*text).into(),
                })
                .collect(),
            ..Config::default()
        })
    }

    /// Só "nova linha" e "novo parágrafo" (os testes de sempre).
    fn commands(text: &str) -> String {
        apply_spoken(text, true, false, &mut 0)
    }

    /// Tudo ligado, como por omissão.
    fn spoken(text: &str) -> String {
        apply_spoken(text, true, true, &mut 0)
    }

    #[test]
    fn commands_between_pauses_become_line_breaks() {
        assert_eq!(
            commands("Olá Pedro, nova linha, tudo bem?"),
            "Olá Pedro\nTudo bem?"
        );
        assert_eq!(
            commands("Olá Pedro. Nova linha. Tudo bem?"),
            "Olá Pedro.\nTudo bem?"
        );
        assert_eq!(
            commands("Caro Pedro. Novo parágrafo. Envio o relatório."),
            "Caro Pedro.\n\nEnvio o relatório."
        );
        assert_eq!(
            commands("Compras: nova linha, leite, nova linha, pão."),
            "Compras:\nLeite\nPão."
        );
        assert_eq!(
            commands("Hello. New line. How are you?"),
            "Hello.\nHow are you?"
        );
        assert_eq!(commands("NOVO PARAGRAFO"), "\n\n");
    }

    #[test]
    fn commands_at_the_ends_are_kept() {
        // O "nova linha" no fim tem de chegar à app: o cursor fica na linha seguinte.
        assert_eq!(commands("Obrigado. Nova linha."), "Obrigado.\n");
        assert_eq!(commands("Nova linha. Olá"), "\nOlá");
        assert_eq!(commands("Nova linha, nova linha."), "\n\n");
    }

    #[test]
    fn the_phrase_inside_a_sentence_is_not_a_command() {
        for text in [
            "A empresa abriu uma nova linha de produção.",
            "Abriram uma nova linha.",
            "Nova linha de metro em Lisboa.",
            "Começa um novo parágrafo aqui",
        ] {
            assert_eq!(commands(text), text);
        }
    }

    #[test]
    fn dictionary_fixes_whole_words_ignoring_case() {
        let r = rules(&[("tauri, tori", "Tauri"), ("", "Manuel Brito")], &[]);
        let p = r.apply("Uso o tori e o TAURI com o manuel brito. Os tauris não.");
        assert_eq!(
            p.text,
            "Uso o Tauri e o Tauri com o Manuel Brito. Os tauris não."
        );
        assert_eq!(p.applied.replaced, 3);
        assert_eq!(r.vocabulary(), ["Tauri", "Manuel Brito"]);
    }

    #[test]
    fn longer_phrases_win_and_spacing_does_not_matter() {
        let r = rules(
            &[("manuel", "Manel"), ("manuel   brito", "Manuel Brito")],
            &[],
        );
        assert_eq!(
            r.apply("o manuel brito e o manuel").text,
            "o Manuel Brito e o Manel"
        );
        let r = rules(&[("c++", "C++")], &[]);
        assert_eq!(r.apply("gosto de c++ e de c").text, "gosto de C++ e de c");
    }

    #[test]
    fn snippets_expand_and_the_dictionary_does_not_touch_them() {
        let r = rules(
            &[("manuel", "Manuel")],
            &[("o meu email", "manuel@exemplo.pt")],
        );
        let p = r.apply("Sou o manuel, escreve para o meu email.");
        assert_eq!(p.text, "Sou o Manuel, escreve para manuel@exemplo.pt.");
        assert_eq!(
            p.applied,
            Applied {
                commands: 0,
                replaced: 1,
                expanded: 1
            }
        );
        // O modelo de limpeza recebe um marcador em vez do endereço.
        assert_eq!(p.cleanup_input(), Some("Sou o Manuel, escreve para {{1}}."));
    }

    #[test]
    fn cleaned_text_gets_the_expansions_and_the_edges_back() {
        let r = rules(&[], &[("a minha morada", "Rua A, 1\n1000-001 Lisboa")]);
        let p = r.apply("Nova linha. envia para a minha morada nova linha");
        assert_eq!(p.text, "\nEnvia para Rua A, 1\n1000-001 Lisboa nova linha");
        let input = p.cleanup_input().unwrap();
        assert_eq!(input, "Envia para {{1}} nova linha");
        let merged = p.merge_cleaned("Envia para {{1}} nova linha.").unwrap();
        assert_eq!(merged, "\nEnvia para Rua A, 1\n1000-001 Lisboa nova linha.");
    }

    #[test]
    fn a_broken_answer_is_rejected() {
        let r = rules(&[], &[("o meu email", "eu@exemplo.pt")]);
        let p = r.apply("Olá, nova linha, manda para o meu email");
        assert_eq!(p.cleanup_input(), Some("Olá\nManda para {{1}}"));
        assert_eq!(
            p.merge_cleaned("Olá. Manda para {{1}}."),
            Err(MergeError::Lines)
        );
        assert_eq!(
            p.merge_cleaned("Olá,\nmanda para o teu email."),
            Err(MergeError::Placeholder)
        );
        assert_eq!(
            p.merge_cleaned("Olá,\nmanda para {{1}} {{1}}"),
            Err(MergeError::Placeholder)
        );
        assert_eq!(
            p.merge_cleaned("Olá,\nManda para {{1}}.").unwrap(),
            "Olá,\nManda para eu@exemplo.pt."
        );
    }

    #[test]
    fn nothing_to_clean_without_words() {
        let r = rules(&[], &[("o meu email", "eu@exemplo.pt")]);
        assert_eq!(r.apply("Nova linha.").cleanup_input(), None);
        assert_eq!(r.apply("O meu email.").cleanup_input(), None);
    }

    #[test]
    fn commands_can_be_turned_off() {
        let r = TextRules::new(&Config {
            voice_commands: false,
            ..Config::default()
        });
        assert_eq!(
            r.apply("Olá, nova linha, tudo bem").text,
            "Olá, nova linha, tudo bem"
        );
    }

    #[test]
    fn firm_phrases_apply_anywhere() {
        // Sem pausas escritas à volta: "ponto de interrogação" nunca é uma frase normal.
        assert_eq!(
            spoken("Olá Pedro ponto de exclamação como estás ponto de interrogação"),
            "Olá Pedro! Como estás?"
        );
        assert_eq!(
            spoken("compra leite ponto e vírgula pão ponto e vírgula natas"),
            "compra leite; pão; natas"
        );
        assert_eq!(
            spoken("Are you sure question mark Yes exclamation mark"),
            "Are you sure? Yes!"
        );
    }

    #[test]
    fn ambiguous_phrases_need_isolation() {
        // Com as pausas que o STT escreve, valem.
        assert_eq!(
            spoken("Olá, Pedro, vírgula, tudo bem, ponto final, obrigado"),
            "Olá, Pedro, tudo bem. Obrigado"
        );
        assert_eq!(
            spoken("Atenção, dois pontos, fecha a porta"),
            "Atenção: fecha a porta"
        );
        assert_eq!(
            spoken("E depois, reticências, não sei"),
            "E depois… não sei"
        );
        // No meio de uma frase, são palavras.
        for text in [
            "A vírgula é um sinal de pontuação.",
            "Fica no ponto final do autocarro.",
            "Marcou dois pontos no jogo.",
            "Ele usa muitas reticências nos textos.",
        ] {
            assert_eq!(spoken(text), text);
        }
    }

    #[test]
    fn ponto_de_vista_untouched() {
        for text in [
            "Do meu ponto de vista, está bem.",
            "O ponto de partida é este.",
            "Chegámos a este ponto.",
            "Falta um ponto e a vírgula.",
        ] {
            assert_eq!(spoken(text), text);
        }
    }

    #[test]
    fn no_double_punctuation() {
        // O modelo já tinha escrito a pausa e/ou a própria marca.
        assert_eq!(spoken("Tudo bem, ponto de interrogação."), "Tudo bem?");
        assert_eq!(spoken("Tudo bem? Ponto de interrogação."), "Tudo bem?");
        assert_eq!(spoken("Olá, vírgula, mundo"), "Olá, mundo");
        assert_eq!(spoken("Fim. Ponto final. Obrigado"), "Fim. Obrigado");
    }

    #[test]
    fn question_mark_capitalizes_next() {
        assert_eq!(
            spoken("quem és ponto de interrogação eu sou o pedro ponto final"),
            "quem és? Eu sou o pedro."
        );
        // A vírgula e o ponto e vírgula não mudam a maiúscula.
        assert_eq!(spoken("sim, vírgula, claro"), "sim, claro");
    }

    #[test]
    fn quotes_pair_by_parity() {
        assert_eq!(
            spoken("Ele disse abre aspas olá fecha aspas e saiu"),
            "Ele disse “olá” e saiu"
        );
        assert_eq!(spoken("abre aspas olá fecha aspas ponto final"), "“olá”.");
        assert_eq!(spoken("open quote hi close quote"), "“hi”");
    }

    #[test]
    fn parentheses_spacing() {
        assert_eq!(
            spoken("vê a figura abre parênteses página dois fecha parênteses ponto final"),
            "vê a figura (página dois)."
        );
        assert_eq!(
            spoken("abre parêntesis a fecha parêntesis fecha parêntesis"),
            "(a))"
        );
    }

    #[test]
    fn marks_and_lines_work_together() {
        assert_eq!(
            spoken("Olá ponto de exclamação, nova linha, tudo bem ponto de interrogação"),
            "Olá!\nTudo bem?"
        );
        assert_eq!(
            spoken("Olá Pedro. Nova linha. Obrigado, vírgula, até já"),
            "Olá Pedro.\nObrigado, até já"
        );
    }

    #[test]
    fn new_line_still_works_with_both_toggles() {
        let only_lines = |text| apply_spoken(text, true, false, &mut 0);
        let only_marks = |text| apply_spoken(text, false, true, &mut 0);
        let text = "Olá, nova linha, tudo bem ponto de interrogação";
        assert_eq!(only_lines(text), "Olá\nTudo bem ponto de interrogação");
        assert_eq!(only_marks(text), "Olá, nova linha, tudo bem?");
        assert_eq!(apply_spoken(text, false, false, &mut 0), text);
    }

    #[test]
    fn disabled_toggle_leaves_text() {
        let text = "Olá, vírgula, tudo bem ponto de interrogação";
        let off = TextRules::new(&Config {
            spoken_punctuation: false,
            ..Config::default()
        });
        assert_eq!(off.apply(text).text, text);
        // O Literal (terminal, IDE) também as desliga, com a opção ligada.
        let on = TextRules::new(&Config::default());
        assert_eq!(on.apply_styled(text, Style::Literal).text, text);
        assert_eq!(on.apply_styled(text, Style::Casual).text, "Olá, tudo bem?");
        // O contador conta os dois tipos de comando.
        let applied = on.apply("Olá ponto de exclamação nova linha").applied;
        assert_eq!(
            applied.commands, 1,
            "o 'nova linha' no fim, sem pausa, não conta"
        );
    }

    #[test]
    fn spoken_punctuation_is_not_applied_to_files() {
        // Uma aula onde se diz "ponto de interrogação" não é um ditado teu.
        let r = TextRules::new(&Config::default());
        let text = "Pergunta com ponto de interrogação, vírgula, fim.";
        assert_eq!(r.apply_dictionary(text), text);
    }

    #[test]
    fn many_entries_compile_and_run_fast() {
        let words: Vec<(String, String)> = (0..500)
            .map(|i| (format!("palavra{i}"), format!("Palavra{i}")))
            .collect();
        let pairs: Vec<(&str, &str)> = words
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let started = std::time::Instant::now();
        let r = rules(&pairs, &[]);
        let text = "uma frase com a palavra499 e a palavra7 lá no meio. ".repeat(20);
        let p = r.apply(&text);
        assert_eq!(p.applied.replaced, 40);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    /// Mede quanto custam as regras num ditado de ~1 min com um dicionário grande.
    /// `cargo test --release text_rules_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn text_rules_cost() {
        let words: Vec<(String, String)> = (0..300)
            .map(|i| (format!("nome{i}, outro nome{i}"), format!("Nome{i}")))
            .collect();
        let snippets: Vec<(String, String)> = (0..50)
            .map(|i| (format!("a minha frase {i}"), format!("Texto {i}")))
            .collect();
        let pairs: Vec<(&str, &str)> = words
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let snips: Vec<(&str, &str)> = snippets
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let t = std::time::Instant::now();
        let r = rules(&pairs, &snips);
        let compile = t.elapsed();
        let text = "Olá João, nova linha, então amanhã às três falamos com o nome42 sobre a \
                    minha frase 7 e a aplicação. "
            .repeat(10);
        let t = std::time::Instant::now();
        let p = r.apply(&text);
        println!(
            "compilar: {compile:?}; aplicar a {} caracteres: {:?} ({:?})",
            text.chars().count(),
            t.elapsed(),
            p.applied
        );
    }
}
