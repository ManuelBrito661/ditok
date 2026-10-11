//! Chave da API do OpenRouter no cofre do sistema (Windows: Gestor de Credenciais;
//! macOS: Keychain). Nunca em ficheiros nem no repositório, e nunca devolvida à UI:
//! a janela de definições só sabe se está ou não configurada.

const SERVICE: &str = "ditok";
const ACCOUNT: &str = "openrouter-api-key";

#[derive(Debug, thiserror::Error)]
#[error("cofre do sistema: {0}")]
pub struct SecretError(#[from] keyring::Error);

fn entry() -> Result<keyring::Entry, SecretError> {
    Ok(keyring::Entry::new(SERVICE, ACCOUNT)?)
}

/// `Ok(None)` quando a chave simplesmente não está guardada (não é um erro).
pub fn get() -> Result<Option<String>, SecretError> {
    match entry()?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn set(secret: &str) -> Result<(), SecretError> {
    Ok(entry()?.set_password(secret)?)
}

pub fn delete() -> Result<(), SecretError> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
