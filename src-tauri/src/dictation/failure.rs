//! Motivo curto de um ditado falhado, para a pílula e para a tooltip do tray.
//!
//! As mensagens completas continuam nas notificações e no log; aqui só cabe uma
//! etiqueta de duas palavras que se lê num relance ("Sem rede", "Sem chave"…).
//! A tradução a partir dos erros reais é pura (sem I/O), por isso tem testes.

use crate::{audio::AudioError, stt::SttError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailReason {
    /// Sem ligação ao servidor (DNS, TLS, cabo, Wi-Fi…).
    NoNetwork,
    /// A conta do OpenRouter ficou sem saldo.
    NoCredits,
    /// A chave tem um limite de gastos e chegou-lhe.
    KeyLimit,
    /// Não há chave guardada.
    NoKey,
    /// Há chave, mas o servidor recusou-a (401).
    KeyRejected,
    /// O pedido foi recusado por outro motivo (403 sem ser o limite da chave,
    /// ex.: moderação).
    Refused,
    /// Demasiados pedidos seguidos (429).
    TooManyRequests,
    /// O microfone não abriu, não existe ou não temos permissão.
    NoMicrophone,
    /// O servidor (ou o provider) não respondeu a tempo.
    Timeout,
    /// Tudo o resto: a notificação e o log explicam.
    Generic,
}

impl FailReason {
    /// Texto curto para a pílula (cabe ao lado da mascote).
    pub fn label(self) -> &'static str {
        match self {
            Self::NoNetwork => "Sem rede",
            Self::NoCredits => "Sem créditos",
            Self::KeyLimit => "Limite da chave",
            Self::NoKey => "Sem chave",
            Self::KeyRejected => "Chave inválida",
            Self::Refused => "Recusado",
            Self::TooManyRequests => "Muitos pedidos",
            Self::NoMicrophone => "Sem microfone",
            Self::Timeout => "Tempo esgotado",
            Self::Generic => "Falhou",
        }
    }

    pub fn from_stt(e: &SttError) -> Self {
        match e {
            SttError::Network(_) => Self::NoNetwork,
            SttError::ClientTimeout | SttError::ProviderTimeout => Self::Timeout,
            SttError::RateLimited(_) => Self::TooManyRequests,
            // O OpenRouter usa 402/403 tanto para "sem saldo" como para "a chave
            // chegou ao limite que lhe deste"; só a mensagem os distingue.
            SttError::InsufficientCredits(m) | SttError::Forbidden(m)
                if m.to_lowercase().contains("limit") =>
            {
                Self::KeyLimit
            }
            SttError::InsufficientCredits(_) => Self::NoCredits,
            SttError::Unauthorized(_) => Self::KeyRejected,
            SttError::Forbidden(_) => Self::Refused,
            _ => Self::Generic,
        }
    }

    /// Vale a pena oferecer "Tentar de novo"? Só para erros passageiros (rede,
    /// tempo, servidor sobrecarregado) ou que se resolvem fora do Ditok sem
    /// gravar outra vez (trocar a chave, carregar créditos). Um áudio grande demais
    /// ou um modelo errado falhariam outra vez da mesma maneira.
    pub fn retryable(e: &SttError) -> bool {
        match e {
            SttError::Network(_)
            | SttError::ClientTimeout
            | SttError::ProviderTimeout
            | SttError::RateLimited(_)
            | SttError::Unauthorized(_)
            | SttError::Forbidden(_)
            | SttError::InsufficientCredits(_) => true,
            SttError::Http { status, .. } => *status >= 500,
            SttError::ModelNotFound { .. }
            | SttError::PayloadTooLarge
            | SttError::BadRequest(_)
            | SttError::Decode(_) => false,
        }
    }

    pub fn from_audio(e: &AudioError) -> Self {
        match e {
            AudioError::NoDevice
            | AudioError::PermissionDenied
            | AudioError::UnsupportedFormat(_)
            | AudioError::Device(_)
            | AudioError::ThreadGone
            // O microfone nem chegou a abrir (o motivo já veio em `AudioFailed`).
            | AudioError::NotRecording => Self::NoMicrophone,
            AudioError::Resample(_) => Self::Generic,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::error_from_response;

    #[test]
    fn maps_stt_errors_to_short_reasons() {
        let reason =
            |status, body: &[u8]| FailReason::from_stt(&error_from_response(status, body, "m"));
        assert_eq!(reason(401, b"{}"), FailReason::KeyRejected);
        assert_eq!(reason(402, b"{}"), FailReason::NoCredits);
        assert_eq!(
            reason(403, br#"{"error":{"message":"Key limit exceeded"}}"#),
            FailReason::KeyLimit
        );
        assert_eq!(
            reason(403, br#"{"error":{"message":"flagged"}}"#),
            FailReason::Refused
        );
        assert_eq!(reason(429, b"slow down"), FailReason::TooManyRequests);
        assert_eq!(reason(524, b""), FailReason::Timeout);
        assert_eq!(reason(500, b"boom"), FailReason::Generic);
        assert_eq!(
            FailReason::from_stt(&SttError::Network("dns error".into())),
            FailReason::NoNetwork
        );
        assert_eq!(
            FailReason::from_stt(&SttError::ClientTimeout),
            FailReason::Timeout
        );
    }

    #[test]
    fn decode_errors_are_generic_not_no_network() {
        assert_eq!(
            FailReason::from_stt(&SttError::Decode("eof".into())),
            FailReason::Generic
        );
    }

    #[test]
    fn only_transient_errors_offer_retry() {
        let retry =
            |status, body: &[u8]| FailReason::retryable(&error_from_response(status, body, "m"));
        assert!(retry(401, b"{}"));
        assert!(retry(402, b"{}"));
        assert!(retry(429, b""));
        assert!(retry(503, b""));
        assert!(retry(524, b""));
        assert!(!retry(404, b""));
        assert!(!retry(413, b""));
        assert!(!retry(400, b"bad"));
        assert!(FailReason::retryable(&SttError::Network("dns".into())));
        assert!(FailReason::retryable(&SttError::ClientTimeout));
        assert!(!FailReason::retryable(&SttError::Decode("x".into())));
    }

    #[test]
    fn audio_errors_mean_no_microphone() {
        assert_eq!(
            FailReason::from_audio(&AudioError::NotRecording),
            FailReason::NoMicrophone
        );
        assert_eq!(
            FailReason::from_audio(&AudioError::PermissionDenied),
            FailReason::NoMicrophone
        );
    }

    #[test]
    fn labels_are_short() {
        use FailReason::*;
        for r in [
            NoNetwork,
            NoCredits,
            KeyLimit,
            NoKey,
            KeyRejected,
            Refused,
            TooManyRequests,
            NoMicrophone,
            Timeout,
            Generic,
        ] {
            assert!(r.label().chars().count() <= 15, "{r:?}");
        }
    }
}
