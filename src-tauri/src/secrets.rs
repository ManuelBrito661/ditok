//! Chaves de API no cofre do sistema (Windows: Gestor de Credenciais; macOS:
//! Keychain). Nunca em ficheiros nem no repositório, e nunca devolvidas à UI.

use std::{fmt, str::FromStr};

const SERVICE: &str = "ditok";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKey {
    OpenRouter,
    Groq,
}

impl ApiKey {
    fn account(self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter-api-key",
            Self::Groq => "groq-api-key",
        }
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OpenRouter => "OpenRouter",
            Self::Groq => "Groq",
        })
    }
}

impl FromStr for ApiKey {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "openrouter" => Ok(Self::OpenRouter),
            "groq" => Ok(Self::Groq),
            other => Err(format!(
                "provider desconhecido \"{other}\" (usa openrouter ou groq)"
            )),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("cofre do sistema: {0}")]
pub struct SecretError(#[from] keyring::Error);

fn entry(key: ApiKey) -> Result<keyring::Entry, SecretError> {
    Ok(keyring::Entry::new(SERVICE, key.account())?)
}

/// `Ok(None)` quando a chave simplesmente não está guardada (não é um erro).
pub fn get(key: ApiKey) -> Result<Option<String>, SecretError> {
    match entry(key)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn set(key: ApiKey, secret: &str) -> Result<(), SecretError> {
    Ok(entry(key)?.set_password(secret)?)
}

pub fn delete(key: ApiKey) -> Result<(), SecretError> {
    match entry(key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
