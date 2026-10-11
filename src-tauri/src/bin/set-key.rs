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

    // Uma linha vazia logo no início é quase sempre um Enter que ficou no buffer do
    // terminal, não uma resposta: voltamos a perguntar algumas vezes.
    let mut key = String::new();
    for _ in 0..3 {
        // O prompt vai pelo `eprint!` (o Rust converte para UTF-16 na consola do
        // Windows; o `rpassword` escreveria bytes UTF-8 e os acentos sairiam mal).
        eprint!("Cola a chave do {provider} (não aparece enquanto escreves) e carrega Enter: ");
        key = rpassword::read_password()?.trim().to_owned();
        if !key.is_empty() {
            break;
        }
    }
    if key.is_empty() {
        return Err("chave vazia; nada foi guardado".into());
    }
    let key = key.as_str();
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
