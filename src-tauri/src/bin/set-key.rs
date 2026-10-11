//! Guarda (ou apaga) a chave do OpenRouter no cofre do sistema, sem a escrever em
//! nenhum ficheiro nem a mostrar no ecrã. Alternativa à janela de definições.
//!
//!     cargo run --bin set-key
//!     cargo run --bin set-key -- --delete

use std::process::ExitCode;

use ditok_lib::secrets;

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
    if std::env::args().skip(1).any(|a| a == "--delete") {
        secrets::delete()?;
        return Ok("Chave do OpenRouter apagada do cofre do sistema.".to_owned());
    }

    // Uma linha vazia logo no início é quase sempre um Enter que ficou no buffer do
    // terminal, não uma resposta: voltamos a perguntar algumas vezes.
    let mut key = String::new();
    for _ in 0..3 {
        // O prompt vai pelo `eprint!` (o Rust converte para UTF-16 na consola do
        // Windows; o `rpassword` escreveria bytes UTF-8 e os acentos sairiam mal).
        eprint!("Cola a chave do OpenRouter (não aparece enquanto escreves) e carrega Enter: ");
        key = rpassword::read_password()?.trim().to_owned();
        if !key.is_empty() {
            break;
        }
    }
    if key.is_empty() {
        return Err("chave vazia; nada foi guardado".into());
    }
    secrets::set(&key)?;
    // Confirma lendo de volta, sem mostrar a chave.
    if secrets::get()?.as_deref() != Some(key.as_str()) {
        return Err("a chave foi escrita mas não foi possível lê-la de volta".into());
    }
    Ok(format!(
        "Chave do OpenRouter guardada no cofre do sistema ({} caracteres). A app usa-a no próximo ditado.",
        key.chars().count()
    ))
}
