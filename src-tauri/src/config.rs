//! Definições em JSON em `app_config_dir` (Windows:
//! %APPDATA%\com.manuelbrito.ditok\config.json). Editadas pela janela de
//! definições; também podem ser editadas à mão (aplicam-se no arranque seguinte).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    hotkey::DEFAULT_SHORTCUT,
    stt::openrouter::Transport,
    text::{Replacement, Snippet},
};

pub const DEFAULT_STT_MODEL: &str = "openai/whisper-large-v3-turbo";
pub const DEFAULT_CLEANUP_MODEL: &str = "openai/gpt-4.1-nano";

/// `#[serde(default)]` faz com que campos em falta no ficheiro usem o valor de
/// `Default`: um config.json antigo continua válido quando acrescentamos campos.
/// Campos desconhecidos (ex.: o antigo "provider") são ignorados, e `alias` aceita
/// os nomes antigos dos campos renomeados.
///
/// O mesmo struct é enviado à UI (`Serialize`) e recebido dela (`Deserialize`): o
/// TypeScript tem uma interface com os mesmos campos em `src/api.ts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub shortcut: String,
    /// Slug completo do modelo no OpenRouter.
    #[serde(alias = "openrouter_model")]
    pub stt_model: String,
    #[serde(alias = "openrouter_transport")]
    pub transport: Transport,
    /// ISO-639-1 (ex.: "pt"); `None` para deteção automática.
    pub language: Option<String>,
    /// Passo opcional que corrige pontuação e remove hesitações com um LLM.
    pub cleanup_enabled: bool,
    pub cleanup_model: String,
    /// Depois de colar, deixa o ditado no clipboard (pronto para colar outra vez)
    /// em vez de repor o que lá estava.
    pub keep_in_clipboard: bool,
    /// Guarda os últimos ditados (texto incluído) em disco, só neste computador.
    pub history_enabled: bool,
    pub history_limit: usize,
    /// Guarda a última gravação em `debug/last.wav`.
    pub save_last_recording: bool,
    /// Barra flutuante com a mascote. Desligá-la liberta a RAM do WebView dela.
    pub show_overlay: bool,
    /// "nova linha" e "novo parágrafo" viram mudanças de linha (ver `text.rs`).
    pub voice_commands: bool,
    /// Palavras escritas à tua maneira depois da transcrição ("tauri" → "Tauri").
    pub dictionary: Vec<Replacement>,
    /// Frases que dão lugar a um texto ("o meu email" → o endereço).
    pub snippets: Vec<Snippet>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shortcut: DEFAULT_SHORTCUT.to_owned(),
            stt_model: DEFAULT_STT_MODEL.to_owned(),
            transport: Transport::Multipart,
            language: Some("pt".to_owned()),
            cleanup_enabled: false,
            cleanup_model: DEFAULT_CLEANUP_MODEL.to_owned(),
            keep_in_clipboard: true,
            history_enabled: true,
            history_limit: 50,
            save_last_recording: false,
            show_overlay: true,
            voice_commands: true,
            dictionary: Vec::new(),
            snippets: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("o modelo tem de ser o slug completo, com \"/\" (ex.: {DEFAULT_STT_MODEL})")]
    Model,
    #[error(
        "idioma inválido: usa um código de 2 letras (ex.: pt) ou deixa vazio para deteção automática"
    )]
    Language,
    #[error("o histórico tem de guardar entre 1 e 1000 ditados")]
    HistoryLimit,
    #[error("cada palavra do dicionário precisa de saber como fica escrita")]
    DictionaryEntry,
    #[error("cada expansão precisa de uma frase e de um texto")]
    SnippetEntry,
    #[error("o dicionário e as expansões têm um limite de {MAX_ENTRIES} entradas cada")]
    TooManyEntries,
    #[error("uma entrada do dicionário ou das expansões é demasiado longa")]
    EntryTooLong,
}

/// Limites do dicionário e das expansões: muito acima do que se usa, só para um
/// config.json editado à mão não pôr o regex a compilar durante segundos.
const MAX_ENTRIES: usize = 1000;
const MAX_PHRASE_CHARS: usize = 200;
const MAX_SNIPPET_CHARS: usize = 5000;

impl Config {
    pub fn path(config_dir: &Path) -> PathBuf {
        config_dir.join("config.json")
    }

    /// Verifica o que vem da UI antes de o guardar.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let slug_ok = |s: &str| s.contains('/') && !s.contains(char::is_whitespace);
        if !slug_ok(&self.stt_model) || !slug_ok(&self.cleanup_model) {
            return Err(ConfigError::Model);
        }
        if let Some(lang) = &self.language
            && !(lang.len() == 2 && lang.chars().all(|c| c.is_ascii_lowercase()))
        {
            return Err(ConfigError::Language);
        }
        if !(1..=1000).contains(&self.history_limit) {
            return Err(ConfigError::HistoryLimit);
        }
        if self.dictionary.len() > MAX_ENTRIES || self.snippets.len() > MAX_ENTRIES {
            return Err(ConfigError::TooManyEntries);
        }
        let too_long = |s: &str, max: usize| s.chars().count() > max;
        for r in &self.dictionary {
            if r.to.trim().is_empty() {
                return Err(ConfigError::DictionaryEntry);
            }
            if too_long(&r.from, MAX_PHRASE_CHARS) || too_long(&r.to, MAX_PHRASE_CHARS) {
                return Err(ConfigError::EntryTooLong);
            }
        }
        for s in &self.snippets {
            if s.trigger.trim().is_empty() || s.text.trim().is_empty() {
                return Err(ConfigError::SnippetEntry);
            }
            if too_long(&s.trigger, MAX_PHRASE_CHARS) || too_long(&s.text, MAX_SNIPPET_CHARS) {
                return Err(ConfigError::EntryTooLong);
            }
        }
        Ok(())
    }

    /// Lê o ficheiro; se não existir, cria-o com os valores por omissão. Se estiver
    /// inválido, usa os valores por omissão sem o reescrever (para não perderes o que
    /// escreveste) e regista o erro.
    pub fn load_or_create(config_dir: &Path) -> Self {
        let path = Self::path(config_dir);
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Self>(&bytes) {
                Ok(config) => config,
                Err(e) => {
                    tracing::error!(path = %path.display(), error = %e, "config.json inválido; a usar valores por omissão");
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let config = Self::default();
                if let Err(e) = config.save(config_dir) {
                    tracing::warn!(error = %e, "não foi possível criar config.json");
                }
                config
            }
            Err(e) => {
                tracing::error!(path = %path.display(), error = %e, "não foi possível ler config.json");
                Self::default()
            }
        }
    }

    /// Escreve num ficheiro temporário e renomeia: se a app morrer a meio, o
    /// config.json antigo fica intacto em vez de meio escrito.
    pub fn save(&self, config_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(config_dir)?;
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = config_dir.join("config.json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, Self::path(config_dir))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_names_missing_and_unknown_fields_are_tolerated() {
        let c: Config = serde_json::from_str(
            r#"{"language": null, "openrouter_model": "google/chirp-3", "provider": "openrouter"}"#,
        )
        .unwrap();
        assert_eq!(c.language, None);
        assert_eq!(c.stt_model, "google/chirp-3");
        assert_eq!(c.shortcut, DEFAULT_SHORTCUT);
        assert!(!c.cleanup_enabled, "limpeza desligada por omissão");
        assert!(
            c.history_enabled,
            "histórico ligado por omissão (o Início mostra os ditados)"
        );
        assert!(
            c.keep_in_clipboard,
            "um config.json antigo passa a deixar o ditado no clipboard"
        );
        assert!(c.voice_commands, "comandos falados ligados por omissão");
        assert!(c.dictionary.is_empty() && c.snippets.is_empty());
    }

    #[test]
    fn roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("ditok-config-test-{}", std::process::id()));
        let c = Config {
            transport: Transport::Json,
            ..Config::default()
        };
        c.save(&dir).unwrap();
        assert_eq!(Config::load_or_create(&dir), c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn validation_rejects_bad_input() {
        assert!(Config::default().validate().is_ok());
        let bad_model = Config {
            stt_model: "chirp-3".into(),
            ..Config::default()
        };
        assert!(matches!(bad_model.validate(), Err(ConfigError::Model)));
        let bad_lang = Config {
            language: Some("PT-pt".into()),
            ..Config::default()
        };
        assert!(matches!(bad_lang.validate(), Err(ConfigError::Language)));
        let auto_lang = Config {
            language: None,
            ..Config::default()
        };
        assert!(auto_lang.validate().is_ok());
        let bad_limit = Config {
            history_limit: 0,
            ..Config::default()
        };
        assert!(matches!(
            bad_limit.validate(),
            Err(ConfigError::HistoryLimit)
        ));
        let no_target = Config {
            dictionary: vec![Replacement {
                from: "tauri".into(),
                to: "  ".into(),
            }],
            ..Config::default()
        };
        assert!(matches!(
            no_target.validate(),
            Err(ConfigError::DictionaryEntry)
        ));
        let empty_snippet = Config {
            snippets: vec![Snippet {
                trigger: "o meu email".into(),
                text: String::new(),
            }],
            ..Config::default()
        };
        assert!(matches!(
            empty_snippet.validate(),
            Err(ConfigError::SnippetEntry)
        ));
    }
}
