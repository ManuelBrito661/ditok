//! Pedidos que se repetem quando falham por pouco: o que os ficheiros e as notas de
//! reuniões fazem a cada pedaço de áudio. Aqui só há lógica de repetição e a
//! classificação dos erros; a repetição em si recebe o pedido como um closure, por isso
//! testa-se sem rede.

use std::time::Duration;

use bytes::Bytes;

use super::{
    ApiError, Xai,
    stt::{SttRequest, Transcription},
};

/// Repetições de um pedaço quando o erro é passageiro (rede, servidor ocupado).
pub const RETRIES: u32 = 2;
/// Espera antes da repetição `n`: `n` × isto (2 s, 4 s).
const BACKOFF: Duration = Duration::from_secs(2);

/// Erros que se repetiriam em todos os pedaços: não vale a pena continuar.
/// Sem um ramo `_`: acrescentar uma variante a `ApiError` obriga a decidir aqui.
pub fn is_fatal(e: &ApiError) -> bool {
    match e {
        ApiError::Unauthorized(_)
        | ApiError::Forbidden(_)
        | ApiError::NoPermission(_)
        | ApiError::InsufficientCredits(_)
        | ApiError::ModelNotFound { .. } => true,
        ApiError::RateLimited(_)
        | ApiError::ProviderTimeout
        | ApiError::PayloadTooLarge
        | ApiError::BadRequest(_)
        | ApiError::Http { .. }
        | ApiError::ClientTimeout
        | ApiError::Network(_)
        | ApiError::Decode(_) => false,
    }
}

/// Erros passageiros: repetir daqui a uns segundos costuma resolver. A xAI pede
/// espera exponencial no 429 e repetir no 503.
pub fn is_transient(e: &ApiError) -> bool {
    match e {
        ApiError::RateLimited(_)
        | ApiError::ProviderTimeout
        | ApiError::ClientTimeout
        | ApiError::Network(_) => true,
        ApiError::Http { status, .. } => *status >= 500,
        ApiError::Unauthorized(_)
        | ApiError::Forbidden(_)
        | ApiError::NoPermission(_)
        | ApiError::InsufficientCredits(_)
        | ApiError::ModelNotFound { .. }
        | ApiError::PayloadTooLarge
        | ApiError::BadRequest(_)
        | ApiError::Decode(_) => false,
    }
}

/// Corre `op`, repetindo até `RETRIES` vezes os erros passageiros, com `backoff` × n de espera.
async fn retrying<T, Fut>(mut op: impl FnMut() -> Fut, backoff: Duration) -> Result<T, ApiError>
where
    Fut: Future<Output = Result<T, ApiError>>,
{
    let mut attempt = 0;
    loop {
        match op().await {
            Err(e) if attempt < RETRIES && is_transient(&e) => {
                attempt += 1;
                tracing::debug!(error = %e, attempt, "a repetir o pedido");
                tokio::time::sleep(backoff * attempt).await;
            }
            result => return result,
        }
    }
}

impl Xai {
    /// Transcreve um pedaço de áudio, repetindo os erros passageiros.
    pub async fn transcribe_retrying(
        &self,
        wav: Bytes,
        req: &SttRequest<'_>,
    ) -> Result<Transcription, ApiError> {
        // `wav.clone()` só copia o ponteiro (é um `Bytes`), não o áudio.
        retrying(|| self.transcribe(wav.clone(), req), BACKOFF).await
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Mutex, PoisonError,
            atomic::{AtomicU32, Ordering},
        },
    };

    use super::*;

    /// Devolve, pela ordem, o que lhe derem, e conta os pedidos.
    struct Script {
        answers: Mutex<VecDeque<Result<String, ApiError>>>,
        calls: AtomicU32,
    }

    impl Script {
        fn new(answers: Vec<Result<String, ApiError>>) -> Self {
            Self {
                answers: Mutex::new(answers.into()),
                calls: AtomicU32::new(0),
            }
        }

        async fn call(&self) -> Result<String, ApiError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.answers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
                .unwrap_or_else(|| Err(ApiError::Decode("sem respostas".into())))
        }

        fn calls(&self) -> u32 {
            self.calls.load(Ordering::Relaxed)
        }
    }

    fn network() -> ApiError {
        ApiError::Network("sem rede".into())
    }

    async fn run(script: &Script) -> Result<String, ApiError> {
        retrying(|| script.call(), Duration::ZERO).await
    }

    #[tokio::test]
    async fn returns_the_first_good_answer() {
        let s = Script::new(vec![Ok("olá mundo".into())]);
        assert_eq!(run(&s).await.unwrap(), "olá mundo");
        assert_eq!(s.calls(), 1);
    }

    #[tokio::test]
    async fn transient_error_retried_twice() {
        // Duas falhas passageiras e uma resposta: à terceira tentativa dá certo.
        let s = Script::new(vec![Err(network()), Err(network()), Ok("ok".into())]);
        assert_eq!(run(&s).await.unwrap(), "ok");
        assert_eq!(s.calls(), 3);

        // Três falhas: desiste (1 pedido + 2 repetições) e devolve o erro.
        let s = Script::new(vec![
            Err(network()),
            Err(network()),
            Err(network()),
            Ok("ok".into()),
        ]);
        assert!(matches!(run(&s).await, Err(ApiError::Network(_))));
        assert_eq!(s.calls(), 3);
    }

    #[tokio::test]
    async fn fatal_error_not_retried() {
        let s = Script::new(vec![
            Err(ApiError::Unauthorized("chave".into())),
            Ok("ok".into()),
        ]);
        assert!(matches!(run(&s).await, Err(ApiError::Unauthorized(_))));
        assert_eq!(s.calls(), 1, "um erro de chave não se repete");
    }

    #[tokio::test]
    async fn a_5xx_is_retried_but_a_4xx_is_not() {
        let http = |status| ApiError::Http {
            status,
            message: "x".into(),
        };
        let s = Script::new(vec![Err(http(503)), Ok("ok".into())]);
        assert!(run(&s).await.is_ok());
        assert_eq!(s.calls(), 2);
        let s = Script::new(vec![Err(http(418)), Ok("ok".into())]);
        assert!(run(&s).await.is_err());
        assert_eq!(s.calls(), 1);
    }

    #[test]
    fn classification_is_consistent() {
        // Nada é fatal e passageiro ao mesmo tempo.
        for e in [
            ApiError::Unauthorized(String::new()),
            ApiError::Forbidden(String::new()),
            ApiError::NoPermission(String::new()),
            ApiError::InsufficientCredits(String::new()),
            ApiError::ModelNotFound { model: "m".into() },
            ApiError::RateLimited(String::new()),
            ApiError::ProviderTimeout,
            ApiError::PayloadTooLarge,
            ApiError::BadRequest(String::new()),
            ApiError::Http {
                status: 500,
                message: String::new(),
            },
            ApiError::ClientTimeout,
            ApiError::Network(String::new()),
            ApiError::Decode(String::new()),
        ] {
            assert!(!(is_fatal(&e) && is_transient(&e)), "{e:?}");
        }
        assert!(is_fatal(&ApiError::ModelNotFound { model: "m".into() }));
        assert!(is_fatal(&ApiError::InsufficientCredits("x".into())));
        assert!(!is_fatal(&ApiError::ProviderTimeout));
        assert!(is_transient(&ApiError::RateLimited(String::new())));
        assert!(is_transient(&ApiError::Http {
            status: 502,
            message: String::new()
        }));
        assert!(!is_transient(&ApiError::Http {
            status: 400,
            message: String::new()
        }));
        assert!(!is_transient(&ApiError::Unauthorized("x".into())));
    }
}
