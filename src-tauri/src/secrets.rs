//! Chaves das APIs no cofre do sistema (Windows: Gestor de Credenciais; macOS:
//! Keychain). Nunca em ficheiros nem no repositório, e nunca devolvidas à UI: a janela
//! de definições só sabe se estão ou não configuradas.
//!
//! Há duas, independentes: a do OpenRouter (transcrição na nuvem, limpeza, comandos,
//! resumos) e a da xAI (transcrição em streaming, ver `stt/xai.rs`).

const SERVICE: &str = "ditok";
const OPENROUTER: &str = "openrouter-api-key";
const XAI: &str = "xai-api-key";

#[derive(Debug, thiserror::Error)]
#[error("cofre do sistema: {0}")]
pub struct SecretError(#[from] keyring::Error);

fn entry(account: &str) -> Result<keyring::Entry, SecretError> {
    Ok(keyring::Entry::new(SERVICE, account)?)
}

/// `Ok(None)` quando a chave simplesmente não está guardada (não é um erro).
fn get_in(account: &str) -> Result<Option<String>, SecretError> {
    match entry(account)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn set_in(account: &str, secret: &str) -> Result<(), SecretError> {
    Ok(entry(account)?.set_password(secret)?)
}

fn delete_in(account: &str) -> Result<(), SecretError> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// A chave do OpenRouter.
pub fn get() -> Result<Option<String>, SecretError> {
    get_in(OPENROUTER)
}

pub fn set(secret: &str) -> Result<(), SecretError> {
    set_in(OPENROUTER, secret)
}

pub fn delete() -> Result<(), SecretError> {
    delete_in(OPENROUTER)
}

/// A chave da xAI.
pub fn get_xai() -> Result<Option<String>, SecretError> {
    get_in(XAI)
}

pub fn set_xai(secret: &str) -> Result<(), SecretError> {
    set_in(XAI, secret)
}

pub fn delete_xai() -> Result<(), SecretError> {
    delete_in(XAI)
}
