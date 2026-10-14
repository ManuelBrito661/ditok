//! Motivo curto de um ditado falhado, para a pílula e para a tooltip do tray.
//!
//! As mensagens completas continuam nas notificações e no log; aqui só cabe uma
//! etiqueta de duas palavras que se lê num relance ("Sem rede", "Sem chave"…).
//! A tradução a partir dos erros reais é pura (sem I/O), por isso tem testes.

use crate::{audio::AudioError, xai::ApiError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailReason {
    /// Sem ligação ao servidor (DNS, TLS, cabo, Wi-Fi…).
    NoNetwork,
    /// A conta da xAI ficou sem créditos, ou chegou ao limite de gastos do mês.
    NoCredits,
    /// Não há chave guardada.
    NoKey,
    /// Há chave, mas o servidor recusou-a (401).
    KeyRejected,
    /// A chave é válida mas foi criada sem acesso a este serviço (403).
    KeyNoAccess,
    /// O pedido foi recusado por outro motivo (403 sem ser falta de créditos,
    /// ex.: moderação).
    Refused,
    /// Demasiados pedidos seguidos (429).
    TooManyRequests,
    /// O microfone não abriu, não existe ou não temos permissão.
    NoMicrophone,
    /// O servidor não respondeu a tempo.
    Timeout,
    /// Modo comando: o texto selecionado passa do limite (ver
    /// `command::MAX_SELECTION`).
    SelectionTooLarge,
    /// Modo comando sem modelo de texto configurado.
    NoModel,
    /// Modo comando: o modelo respondeu, mas sem texto que se possa colar.
    BadAnswer,
    /// Tudo o resto: a notificação e o log explicam.
    Generic,
}

impl FailReason {
    /// Texto curto para a pílula (cabe ao lado da mascote).
    pub fn label(self) -> &'static str {
        match self {
            Self::NoNetwork => "Sem rede",
            Self::NoCredits => "Sem créditos",
            Self::NoKey => "Sem chave",
            Self::KeyRejected => "Chave inválida",
            Self::KeyNoAccess => "Chave sem permissão",
            Self::Refused => "Recusado",
            Self::TooManyRequests => "Muitos pedidos",
            Self::NoMicrophone => "Sem microfone",
            Self::Timeout => "Tempo esgotado",
            Self::SelectionTooLarge => "Seleção grande demais",
            Self::NoModel => "Sem modelo",
            Self::BadAnswer => "Resposta vazia",
            Self::Generic => "Falhou",
        }
    }

    pub fn from_api(e: &ApiError) -> Self {
        match e {
            ApiError::Network(_) => Self::NoNetwork,
            ApiError::ClientTimeout | ApiError::ProviderTimeout => Self::Timeout,
            ApiError::RateLimited(_) => Self::TooManyRequests,
            ApiError::InsufficientCredits(_) => Self::NoCredits,
            ApiError::Unauthorized(_) => Self::KeyRejected,
            ApiError::NoPermission(_) => Self::KeyNoAccess,
            ApiError::Forbidden(_) => Self::Refused,
            _ => Self::Generic,
        }
    }

    /// Vale a pena oferecer "Tentar de novo"? Só para erros passageiros (rede,
    /// tempo, servidor sobrecarregado) ou que se resolvem fora do Ditok sem
    /// gravar outra vez (trocar a chave, carregar créditos). Um áudio grande demais
    /// ou um modelo errado falhariam outra vez da mesma maneira.
    pub fn retryable(e: &ApiError) -> bool {
        match e {
            ApiError::Network(_)
            | ApiError::ClientTimeout
            | ApiError::ProviderTimeout
            | ApiError::RateLimited(_)
            | ApiError::Unauthorized(_)
            | ApiError::Forbidden(_)
            | ApiError::NoPermission(_)
            | ApiError::InsufficientCredits(_) => true,
            ApiError::Http { status, .. } => *status >= 500,
            ApiError::ModelNotFound { .. }
            | ApiError::PayloadTooLarge
            | ApiError::BadRequest(_)
            | ApiError::Decode(_) => false,
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
    use crate::xai::error_from_response;

    #[test]
    fn maps_api_errors_to_short_reasons() {
        let reason =
            |status, body: &[u8]| FailReason::from_api(&error_from_response(status, body, "m"));
        assert_eq!(reason(401, b"{}"), FailReason::KeyRejected);
        assert_eq!(reason(402, b"{}"), FailReason::NoCredits);
        assert_eq!(
            reason(
                403,
                br#"{"error":"Your team has either used all available credits or reached its monthly spending limit."}"#
            ),
            FailReason::NoCredits
        );
        assert_eq!(reason(403, br#"{"error":"flagged"}"#), FailReason::Refused);
        assert_eq!(
            reason(
                403,
                br#"{"error":"Access to the chat endpoint is denied."}"#
            ),
            FailReason::KeyNoAccess
        );
        assert_eq!(reason(429, b"slow down"), FailReason::TooManyRequests);
        assert_eq!(reason(504, b""), FailReason::Timeout);
        assert_eq!(reason(500, b"boom"), FailReason::Generic);
        assert_eq!(
            FailReason::from_api(&ApiError::Network("dns error".into())),
            FailReason::NoNetwork
        );
        assert_eq!(
            FailReason::from_api(&ApiError::ClientTimeout),
            FailReason::Timeout
        );
    }

    #[test]
    fn decode_errors_are_generic_not_no_network() {
        assert_eq!(
            FailReason::from_api(&ApiError::Decode("eof".into())),
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
        assert!(retry(504, b""));
        assert!(!retry(404, b""));
        assert!(!retry(413, b""));
        assert!(!retry(400, b"bad"));
        assert!(FailReason::retryable(&ApiError::Network("dns".into())));
        assert!(FailReason::retryable(&ApiError::ClientTimeout));
        assert!(!FailReason::retryable(&ApiError::Decode("x".into())));
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
            NoKey,
            KeyRejected,
            KeyNoAccess,
            Refused,
            TooManyRequests,
            NoMicrophone,
            Timeout,
            SelectionTooLarge,
            NoModel,
            BadAnswer,
            Generic,
        ] {
            // A pílula de falha alarga-se para os motivos mais compridos
            // (classe "wide" em overlay.ts).
            assert!(r.label().chars().count() <= 22, "{r:?}");
        }
    }
}
