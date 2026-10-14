//! Pedidos de transcrição que se repetem quando falham por pouco: o que os ficheiros
//! (e, mais tarde, as notas de reuniões) fazem a cada pedaço de áudio, qualquer que
//! seja o provider. Aqui só há lógica de repetição e a classificação dos erros;
//! nada sabe de OpenRouter, por isso testa-se com um provider falso.

use std::time::Duration;

use bytes::Bytes;

use super::{SttError, SttProvider, SttRequest, Transcription};

/// Repetições de um pedaço quando o erro é passageiro (rede, servidor ocupado).
pub const RETRIES: u32 = 2;
/// Espera antes da repetição `n`: `n` × isto (2 s, 4 s).
const BACKOFF: Duration = Duration::from_secs(2);

/// Erros que se repetiriam em todos os pedaços: não vale a pena continuar.
/// Sem um ramo `_`: acrescentar uma variante a `SttError` obriga a decidir aqui.
pub fn is_fatal(e: &SttError) -> bool {
    match e {
        SttError::Unauthorized(_)
        | SttError::Forbidden(_)
        | SttError::InsufficientCredits(_)
        | SttError::ModelNotFound { .. }
        | SttError::ModelMissing(_) => true,
        SttError::RateLimited(_)
        | SttError::ProviderTimeout
        | SttError::PayloadTooLarge
        | SttError::BadRequest(_)
        | SttError::Http { .. }
        | SttError::ClientTimeout
        | SttError::Network(_)
        | SttError::Inference(_)
        | SttError::Decode(_) => false,
    }
}

/// Erros passageiros: repetir daqui a uns segundos costuma resolver.
pub fn is_transient(e: &SttError) -> bool {
    match e {
        SttError::RateLimited(_)
        | SttError::ProviderTimeout
        | SttError::ClientTimeout
        | SttError::Network(_) => true,
        SttError::Http { status, .. } => *status >= 500,
        SttError::Unauthorized(_)
        | SttError::Forbidden(_)
        | SttError::InsufficientCredits(_)
        | SttError::ModelNotFound { .. }
        | SttError::ModelMissing(_)
        | SttError::Inference(_)
        | SttError::PayloadTooLarge
        | SttError::BadRequest(_)
        | SttError::Decode(_) => false,
    }
}

/// Transcreve um pedaço, repetindo até `RETRIES` vezes os erros passageiros.
pub async fn transcribe(
    provider: &dyn SttProvider,
    wav: Bytes,
    model: &str,
    language: Option<&str>,
) -> Result<Transcription, SttError> {
    transcribe_with_backoff(provider, wav, model, language, BACKOFF).await
}

/// Como `transcribe`, com a espera base à escolha (os testes não esperam segundos).
async fn transcribe_with_backoff(
    provider: &dyn SttProvider,
    wav: Bytes,
    model: &str,
    language: Option<&str>,
    backoff: Duration,
) -> Result<Transcription, SttError> {
    let req = SttRequest {
        model,
        language,
        keyterms: &[],
    };
    let mut attempt = 0;
    loop {
        // `wav.clone()` só copia o ponteiro (é um `Bytes`), não o áudio.
        match provider.transcribe(wav.clone(), &req).await {
            Err(e) if attempt < RETRIES && is_transient(&e) => {
                attempt += 1;
                tracing::debug!(error = %e, attempt, "a repetir o pedaço");
                tokio::time::sleep(backoff * attempt).await;
            }
            result => return result,
        }
    }
}

#[cfg(test)]
pub mod fake {
    //! Um provider falso: devolve, pela ordem, o que lhe derem, e conta os pedidos.

    use std::{
        collections::VecDeque,
        sync::{
            Mutex, PoisonError,
            atomic::{AtomicU32, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use bytes::Bytes;

    use super::super::{SttError, SttProvider, SttRequest, Transcription, Usage};

    pub struct FakeStt {
        answers: Mutex<VecDeque<Result<String, SttError>>>,
        calls: AtomicU32,
    }

    impl FakeStt {
        pub fn new(answers: Vec<Result<String, SttError>>) -> Self {
            Self {
                answers: Mutex::new(answers.into()),
                calls: AtomicU32::new(0),
            }
        }

        pub fn calls(&self) -> u32 {
            self.calls.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl SttProvider for FakeStt {
        fn name(&self) -> &'static str {
            "fake"
        }

        async fn prewarm(&self) {}

        async fn transcribe(
            &self,
            _audio: Bytes,
            _req: &SttRequest<'_>,
        ) -> Result<Transcription, SttError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let next = self
                .answers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front();
            match next {
                Some(Ok(text)) => Ok(Transcription {
                    text,
                    usage: Usage::default(),
                    generation_id: None,
                    elapsed: Duration::ZERO,
                    transport: "fake",
                }),
                Some(Err(e)) => Err(e),
                None => Err(SttError::Decode(
                    "o provider falso ficou sem respostas".into(),
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fake::FakeStt, *};

    fn network() -> SttError {
        SttError::Network("sem rede".into())
    }

    async fn run(fake: &FakeStt) -> Result<Transcription, SttError> {
        transcribe_with_backoff(
            fake,
            Bytes::from_static(b"RIFF"),
            "m/m",
            Some("pt"),
            Duration::ZERO,
        )
        .await
    }

    #[tokio::test]
    async fn transcribe_returns_the_provider_text() {
        let fake = FakeStt::new(vec![Ok("olá mundo".into())]);
        let t = run(&fake).await.unwrap();
        assert_eq!(t.text, "olá mundo");
        assert_eq!(fake.calls(), 1);
    }

    #[tokio::test]
    async fn transient_error_retried_twice() {
        // Duas falhas passageiras e uma resposta: à terceira tentativa dá certo.
        let fake = FakeStt::new(vec![Err(network()), Err(network()), Ok("ok".into())]);
        assert_eq!(run(&fake).await.unwrap().text, "ok");
        assert_eq!(fake.calls(), 3);

        // Três falhas: desiste (1 pedido + 2 repetições) e devolve o erro.
        let fake = FakeStt::new(vec![
            Err(network()),
            Err(network()),
            Err(network()),
            Ok("ok".into()),
        ]);
        assert!(matches!(run(&fake).await, Err(SttError::Network(_))));
        assert_eq!(fake.calls(), 3);
    }

    #[tokio::test]
    async fn fatal_error_not_retried() {
        let fake = FakeStt::new(vec![
            Err(SttError::Unauthorized("chave".into())),
            Ok("ok".into()),
        ]);
        assert!(matches!(run(&fake).await, Err(SttError::Unauthorized(_))));
        assert_eq!(fake.calls(), 1, "um erro de chave não se repete");
    }

    #[tokio::test]
    async fn a_5xx_is_retried_but_a_4xx_is_not() {
        let http = |status| SttError::Http {
            status,
            message: "x".into(),
        };
        let fake = FakeStt::new(vec![Err(http(503)), Ok("ok".into())]);
        assert!(run(&fake).await.is_ok());
        assert_eq!(fake.calls(), 2);
        let fake = FakeStt::new(vec![Err(http(418)), Ok("ok".into())]);
        assert!(run(&fake).await.is_err());
        assert_eq!(fake.calls(), 1);
    }

    #[test]
    fn classification_is_consistent() {
        // Nada é fatal e passageiro ao mesmo tempo.
        for e in [
            SttError::Unauthorized(String::new()),
            SttError::Forbidden(String::new()),
            SttError::InsufficientCredits(String::new()),
            SttError::ModelNotFound { model: "m".into() },
            SttError::RateLimited(String::new()),
            SttError::ProviderTimeout,
            SttError::PayloadTooLarge,
            SttError::BadRequest(String::new()),
            SttError::Http {
                status: 500,
                message: String::new(),
            },
            SttError::ClientTimeout,
            SttError::Network(String::new()),
            SttError::Decode(String::new()),
        ] {
            assert!(!(is_fatal(&e) && is_transient(&e)), "{e:?}");
        }
        assert!(is_fatal(&SttError::ModelNotFound { model: "m".into() }));
        assert!(is_fatal(&SttError::InsufficientCredits("x".into())));
        assert!(!is_fatal(&SttError::ProviderTimeout));
        assert!(is_transient(&SttError::RateLimited(String::new())));
        assert!(is_transient(&SttError::Http {
            status: 502,
            message: String::new()
        }));
        assert!(!is_transient(&SttError::Http {
            status: 400,
            message: String::new()
        }));
        assert!(!is_transient(&SttError::Unauthorized("x".into())));
    }
}
