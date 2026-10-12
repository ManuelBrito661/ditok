//! O que o sistema tem de ter para captar o áudio do computador.
//!
//! No Windows o loopback do WASAPI existe desde sempre. No macOS o cpal usa uma "tap"
//! do Core Audio, que só existe a partir do macOS 14.6; numa versão anterior o
//! pedido falharia com um erro críptico, por isso diz-se o motivo antes de começar.

/// Versão mínima do macOS para o loopback (major, minor).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const MACOS_MIN: (u32, u32) = (14, 6);

/// "14.6.1" → (14, 6). Aceita "15", "14.6" e lixo à frente ("macOS 14.6").
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_version(v: &str) -> Option<(u32, u32)> {
    let digits = v.trim_start_matches(|c: char| !c.is_ascii_digit());
    let mut parts = digits.split('.').map(|p| {
        p.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u32>()
    });
    let major = parts.next()?.ok()?;
    let minor = parts.next().and_then(Result::ok).unwrap_or(0);
    Some((major, minor))
}

/// Esta versão do macOS consegue captar o áudio do sistema? Versão desconhecida:
/// tenta-se (o erro do sistema aparece se não der).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn macos_supports_loopback(version: Option<&str>) -> bool {
    match version.and_then(parse_version) {
        Some(v) => v >= MACOS_MIN,
        None => true,
    }
}

/// `Err` com a explicação, pronta a mostrar, se o áudio do sistema não puder ser captado.
pub fn check_system_audio() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let version = sysinfo::System::os_version();
        if !macos_supports_loopback(version.as_deref()) {
            return Err(format!(
                "Captar o áudio do computador pede o macOS {}.{} ou mais recente (tens o {}). \
                 Podes gravar só o microfone (conversa presencial).",
                MACOS_MIN.0,
                MACOS_MIN.1,
                version.unwrap_or_default()
            ));
        }
    }
    Ok(())
}

/// Onde se dá a permissão de captura do áudio do sistema, para a mensagem de "só silêncio".
pub const SYSTEM_AUDIO_HELP: &str = if cfg!(target_os = "macos") {
    "Definições do Sistema → Privacidade e Segurança → Gravação de Ecrã e Áudio do Sistema, e liga o Ditok (na secção só de áudio)."
} else {
    "Alguns conteúdos protegidos (DRM) chegam mudos."
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(parse_version("14.6.1"), Some((14, 6)));
        assert_eq!(parse_version("15"), Some((15, 0)));
        assert_eq!(parse_version("macOS 14.7"), Some((14, 7)));
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("abc"), None);
    }

    #[test]
    fn loopback_needs_14_6() {
        for ok in ["14.6", "14.6.1", "14.7", "15.0", "26.1"] {
            assert!(macos_supports_loopback(Some(ok)), "{ok}");
        }
        for bad in ["14.5", "14.5.9", "13.6", "12.7.6"] {
            assert!(!macos_supports_loopback(Some(bad)), "{bad}");
        }
        assert!(macos_supports_loopback(None), "desconhecida: tenta-se");
        assert!(macos_supports_loopback(Some("???")));
    }
}
