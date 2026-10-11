//! Definições em JSON em `app_config_dir` (Windows:
//! %APPDATA%\com.manuelbrito.ditok\config.json). Até à janela de definições (fase 5)
//! edita-se à mão; as alterações aplicam-se no arranque seguinte.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{hotkey::DEFAULT_SHORTCUT, stt::openrouter::Transport};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    OpenRouter,
}

/// `#[serde(default)]` faz com que campos em falta no ficheiro usem o valor de
/// `Default`: um config.json antigo continua válido quando acrescentamos campos.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub shortcut: String,
    pub provider: Provider,
    /// Slug completo do modelo no OpenRouter.
    pub openrouter_model: String,
    pub openrouter_transport: Transport,
    /// ISO-639-1 (ex.: "pt"); `null` para deteção automática.
    pub language: Option<String>,
    /// Guarda a última gravação em `debug/last.wav` (útil para o benchmark).
    pub save_last_recording: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shortcut: DEFAULT_SHORTCUT.to_owned(),
            provider: Provider::OpenRouter,
            openrouter_model: "openai/whisper-large-v3-turbo".to_owned(),
            openrouter_transport: Transport::Multipart,
            language: Some("pt".to_owned()),
            save_last_recording: true,
        }
    }
}

impl Config {
    pub fn path(config_dir: &Path) -> PathBuf {
        config_dir.join("config.json")
    }

    /// Lê o ficheiro; se não existir, cria-o com os valores por omissão. Se estiver
    /// inválido, usa os valores por omissão sem o reescrever (para não perderes o que
    /// escreveste) e regista o erro.
    pub fn load_or_create(config_dir: &Path) -> Self {
        let path = Self::path(config_dir);
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
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
    fn missing_fields_fall_back_to_defaults() {
        let c: Config =
            serde_json::from_str(r#"{"language": null, "openrouter_model": "google/chirp-3"}"#)
                .unwrap();
        assert_eq!(c.language, None);
        assert_eq!(c.openrouter_model, "google/chirp-3");
        assert_eq!(c.shortcut, DEFAULT_SHORTCUT);
        assert_eq!(c.openrouter_transport, Transport::Multipart);
    }

    #[test]
    fn roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("ditok-config-test-{}", std::process::id()));
        let c = Config {
            openrouter_transport: Transport::Json,
            ..Config::default()
        };
        c.save(&dir).unwrap();
        let loaded = Config::load_or_create(&dir);
        assert_eq!(loaded.openrouter_transport, Transport::Json);
        assert_eq!(loaded.provider, Provider::OpenRouter);
        let _ = std::fs::remove_dir_all(dir);
    }
}
