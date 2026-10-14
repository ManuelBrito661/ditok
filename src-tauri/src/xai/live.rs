//! Testes contra a xAI a sério, com a chave do cofre do sistema. Todos `#[ignore]`: custam
//! uma fração de cêntimo e precisam de rede.
//!
//!     cargo test --lib live_ -- --ignored --nocapture --test-threads=1
//!
//! Os que usam áudio querem um WAV (16 kHz, mono) em `DITOK_WAV`, por exemplo o
//! `debug\last.wav` de um ditado (ligar "Guardar a última gravação" nas Definições).

use std::time::Instant;

use bytes::Bytes;

use super::{
    Xai,
    chat::Chat,
    http_client,
    stt::{SttRequest, keyterms},
};

fn client() -> Xai {
    let key = crate::secrets::get()
        .expect("cofre")
        .expect("sem chave da xAI no cofre: guarda-a nas Definições");
    Xai::new(http_client().unwrap(), key)
}

/// O idioma do áudio de teste (`DITOK_LANG`, por omissão `pt`).
fn lang() -> String {
    std::env::var("DITOK_LANG").unwrap_or_else(|_| "pt".into())
}

fn wav() -> Bytes {
    let path = std::env::var("DITOK_WAV").expect("define DITOK_WAV=<ficheiro.wav>");
    Bytes::from(std::fs::read(path).expect("não li o WAV"))
}

#[tokio::test]
#[ignore]
async fn live_key_and_models() {
    let xai = client();
    let started = Instant::now();
    xai.check_key().await.expect("chave recusada");
    println!("chave aceite em {:?}", started.elapsed());
    let models = xai.list_models().await.expect("lista de modelos");
    for m in &models {
        println!(
            "{:<34} {:>8.3} / {:>8.3} US$ por M tokens  raciocina: {}",
            m.id,
            m.prompt_price.unwrap_or(0.0) * 1e6,
            m.completion_price.unwrap_or(0.0) * 1e6,
            m.reasoning
        );
    }
    assert!(
        models.iter().any(|m| m.id == super::DEFAULT_CHAT_MODEL),
        "o modelo por omissão ({}) não está na lista",
        super::DEFAULT_CHAT_MODEL
    );
}

/// O pedido que a limpeza faz, com o modelo por omissão (ou `DITOK_CLEANUP_MODEL`).
#[tokio::test]
#[ignore]
async fn live_chat() {
    let xai = client();
    let model =
        std::env::var("DITOK_CLEANUP_MODEL").unwrap_or_else(|_| super::DEFAULT_CHAT_MODEL.into());
    for round in 1..=2 {
        let started = Instant::now();
        let answer = xai
            .chat(&Chat {
                model: &model,
                system: "Devolve o texto com pontuação e maiúsculas, sem hesitações. Responde só com o texto.",
                user: "<transcricao>ééé olá tudo bem hmm hoje vou vou testar o ditok</transcricao>",
                temperature: 0.0,
                max_tokens: 128,
                timeout: std::time::Duration::from_secs(20),
                purpose: "ditok-live-test",
            })
            .await
            .expect("chat");
        println!(
            "[{round}] {model}: {:?} em {:?}, custo {:?}",
            answer.text,
            started.elapsed(),
            answer.cost
        );
        assert!(
            answer.cost.is_some_and(|c| c > 0.0),
            "sem custo na resposta"
        );
    }
}

#[tokio::test]
#[ignore]
async fn live_transcribe_batch() {
    let xai = client();
    let words = keyterms(&[]);
    let language = lang();
    let req = SttRequest {
        language: Some(language.as_str()),
        keyterms: &words,
    };
    let started = Instant::now();
    let t = xai.transcribe(wav(), &req).await.expect("transcrição");
    println!(
        "{:?} ({:?} no pedido, {:?} no total, {:?} s de áudio, custo {:?})",
        t.text,
        t.elapsed,
        started.elapsed(),
        t.usage.seconds,
        t.usage.cost
    );
    assert!(!t.text.trim().is_empty());
}

/// Envia o WAV em blocos de 50 ms, ao ritmo da fala, como o microfone.
#[tokio::test]
#[ignore]
async fn live_transcribe_stream() {
    let xai = client();
    let wav = wav();
    // Sem o cabeçalho de 44 bytes: o stream recebe PCM cru.
    let pcm = wav.slice(44..);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    let session = xai.stream(Some(lang()), Vec::new(), rx);
    let seconds = pcm.len() as f64 / 32_000.0;
    let started = Instant::now();
    for chunk in pcm.chunks(1_600) {
        tx.send(Bytes::copy_from_slice(chunk)).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let spoken = started.elapsed();
    drop(tx);
    let released = Instant::now();
    let t = session
        .finish(std::time::Duration::from_secs(10))
        .await
        .expect("stream");
    println!(
        "{:?}\nfala {:?} ({seconds:.1} s de áudio); do largar ao texto final: {:?}; custo {:?}",
        t.text,
        spoken,
        released.elapsed(),
        t.usage.cost
    );
    assert!(!t.text.trim().is_empty());
}

#[tokio::test]
#[ignore]
async fn live_diarize() {
    let xai = client();
    let pcm = wav().slice(44..);
    let started = Instant::now();
    let segments = crate::notes::speakers::diarize(&xai, pcm, Some(lang().as_str()))
        .await
        .expect("diarização");
    println!("{} trechos em {:?}", segments.len(), started.elapsed());
    for s in &segments {
        println!(
            "  orador {}: {} ms – {} ms",
            s.speaker, s.start_ms, s.end_ms
        );
    }
}

/// O que a chave deixa fazer (a mesma verificação do botão "Testar" das definições).
#[tokio::test]
#[ignore]
async fn live_probe() {
    let started = Instant::now();
    let access = client().probe().await;
    println!("{access:#?} em {:?}", started.elapsed());
}
