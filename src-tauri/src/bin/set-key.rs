//! Guarda (ou apaga) uma chave de API no cofre do sistema, sem a escrever em
//! nenhum ficheiro nem a mostrar no ecrã.
//!
//!     cargo run --bin set-key -- openrouter
//!     cargo run --bin set-key -- groq
//!     cargo run --bin set-key -- openrouter --delete

use std::process::ExitCode;

use ditok_lib::secrets::{self, ApiKey};

fn main() -> ExitCode {
    match run() {
        Ok(msg) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("erro: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let provider: ApiKey = args
        .next()
        .unwrap_or_else(|| "openrouter".to_owned())
        .parse()?;

    if args.next().as_deref() == Some("--delete") {
        secrets::delete(provider)?;
        return Ok(format!("Chave do {provider} apagada do cofre do sistema."));
    }

    let key = rpassword::prompt_password(format!(
        "Cola a chave do {provider} (não aparece enquanto escreves) e carrega Enter: "
    ))?;
    let key = key.trim();
    if key.is_empty() {
        return Err("chave vazia; nada foi guardado".into());
    }
    secrets::set(provider, key)?;
    // Confirma lendo de volta, sem mostrar a chave.
    let stored = secrets::get(provider)?.is_some_and(|k| k == key);
    if !stored {
        return Err("a chave foi escrita mas não foi possível lê-la de volta".into());
    }
    Ok(format!(
        "Chave do {provider} guardada no cofre do sistema ({} caracteres). A app usa-a no próximo ditado.",
        key.chars().count()
    ))
}
