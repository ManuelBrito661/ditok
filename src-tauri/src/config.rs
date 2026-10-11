//! Definições em JSON em `app_config_dir` (Windows:
//! %APPDATA%\com.manuelbrito.ditok\config.json). Editadas pela janela de
//! definições; também podem ser editadas à mão (aplicam-se no arranque seguinte).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{hotkey::DEFAULT_SHORTCUT, stt::openrouter::Transport};

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
}

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
        assert!(c.history_enabled, "histórico ligado por omissão (o Início mostra os ditados)");
        assert!(
            c.keep_in_clipboard,
            "um config.json antigo passa a deixar o ditado no clipboard"
        );
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
    }
}
