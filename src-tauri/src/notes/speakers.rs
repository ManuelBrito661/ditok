//! Quem fala dentro de "Outros": diarização de oradores, neste computador.
//!
//! Corre **depois** da reunião, sobre o áudio da faixa dos outros que se foi guardando
//! num ficheiro temporário (`diar.pcm`, apagado no fim): um modelo de segmentação
//! (pyannote) marca onde fala alguém, um de "impressão da voz" (wespeaker) mede cada
//! trecho, e um agrupamento junta os trechos parecidos em "Orador 1, 2…". É o motor
//! `sherpa-onnx` do STT local, por isso só existe com a feature `local`.
//!
//! A atribuição às linhas já transcritas (`assign`) e a renumeração (`renumber`) são
//! puras e testam-se sem modelos.

use std::path::{Path, PathBuf};

use super::{capture::Track, store::Line};
use crate::local_models::{self, DownloadError, Job};

/// Esta compilação sabe separar oradores?
pub const AVAILABLE: bool = cfg!(feature = "local");

/// Mais do que isto (em amostras a 16 kHz) não se processa: o motor trabalha com o
/// áudio todo em memória (~230 MB por hora).
pub const MAX_SAMPLES: usize = 90 * 60 * 16_000;

struct File {
    name: &'static str,
    repo: &'static str,
    commit: &'static str,
    size: u64,
    sha256: &'static str,
}

const SEGMENTATION: File = File {
    name: "segmentation.int8.onnx",
    repo: "csukuangfj/sherpa-onnx-pyannote-segmentation-3-0",
    commit: "9403a6902bb58e3d5ae8c7e77c3422de279db2e0",
    size: 1_540_506,
    sha256: "d582f4b4c6b48205de7e0643c57df0df5615a3c176189be3fc461e9d18827b5d",
};

const EMBEDDING: File = File {
    name: "embedding.onnx",
    repo: "csukuangfj/speaker-embedding-models",
    commit: "0743f301363dec56491a490f6d6cbc9d67f9a3bf",
    size: 26_534_365,
    sha256: "5ef208a9da1453335308a6b6f4e6dfbd7e183a38b604de0a57664f45d257fe94",
};

/// No Hugging Face os dois modelos têm outro nome: guardamo-los com nomes nossos.
fn remote_name(f: &File) -> &'static str {
    if f.name == SEGMENTATION.name {
        "model.int8.onnx"
    } else {
        "wespeaker_en_voxceleb_resnet34.onnx"
    }
}

const FILES: [&File; 2] = [&SEGMENTATION, &EMBEDDING];

pub fn dir(models_dir: &Path) -> PathBuf {
    models_dir.join("speakers")
}

pub fn total_bytes() -> u64 {
    FILES.iter().map(|f| f.size).sum()
}

pub fn is_installed(models_dir: &Path) -> bool {
    let dir = dir(models_dir);
    FILES
        .iter()
        .all(|f| std::fs::metadata(dir.join(f.name)).is_ok_and(|m| m.len() == f.size))
}

pub fn delete(models_dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(dir(models_dir)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Descarrega os dois modelos (com retoma e verificação do SHA-256, como os do STT).
pub async fn download(
    client: &reqwest::Client,
    models_dir: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    on_progress: impl FnMut(local_models::Progress),
) -> Result<(), DownloadError> {
    let jobs: Vec<Job> = FILES
        .iter()
        .map(|f| Job {
            url: format!(
                "https://huggingface.co/{}/resolve/{}/{}",
                f.repo,
                f.commit,
                remote_name(f)
            ),
            name: f.name,
            size: f.size,
            sha256: f.sha256,
        })
        .collect();
    local_models::download_files(
        client,
        "speakers",
        &dir(models_dir),
        &jobs,
        cancel,
        on_progress,
    )
    .await
}

// ---------------------------------------------------------------- puro

/// Um trecho em que fala um orador (1, 2, 3…), em milissegundos desde o início da sessão.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: u32,
}

/// Numera os oradores pela ordem em que começam a falar: "Orador 1" é quem abriu a
/// conversa, o que é mais fácil de reconhecer do que um número de grupo arbitrário.
pub fn renumber(segments: &mut [Segment]) {
    segments.sort_by_key(|s| (s.start_ms, s.end_ms));
    let mut seen: Vec<u32> = Vec::new();
    for s in segments.iter_mut() {
        let n = match seen.iter().position(|&x| x == s.speaker) {
            Some(i) => i,
            None => {
                seen.push(s.speaker);
                seen.len() - 1
            }
        };
        s.speaker = n as u32 + 1;
    }
}

/// Dá a cada linha da `track` o orador que mais se sobrepõe a ela no tempo. Sem
/// sobreposição nenhuma, a linha fica sem orador (continua a ser "Outros").
pub fn assign(lines: &mut [Line], track: Track, segments: &[Segment]) {
    for line in lines.iter_mut().filter(|l| l.track == track) {
        let mut best: Option<(u64, u32)> = None;
        // Soma por orador: dois segmentos curtos do mesmo orador contam juntos.
        let mut totals: Vec<(u32, u64)> = Vec::new();
        for s in segments {
            let overlap = line
                .end_ms
                .min(s.end_ms)
                .saturating_sub(line.start_ms.max(s.start_ms));
            if overlap == 0 {
                continue;
            }
            match totals.iter_mut().find(|(sp, _)| *sp == s.speaker) {
                Some((_, t)) => *t += overlap,
                None => totals.push((s.speaker, overlap)),
            }
        }
        for (speaker, total) in totals {
            if best.is_none_or(|(b, _)| total > b) {
                best = Some((total, speaker));
            }
        }
        line.speaker = best.map(|(_, sp)| sp);
    }
}

// ---------------------------------------------------------------- motor

#[derive(Debug, thiserror::Error)]
pub enum SpeakersError {
    #[error("esta versão do Ditok não inclui a separação de oradores")]
    Unavailable,
    #[error("faltam os modelos de oradores: descarrega-os nas Notas")]
    ModelMissing,
    #[error("a reunião é demasiado longa para separar os oradores (o limite são 90 minutos)")]
    TooLong,
    #[error("o motor não conseguiu processar o áudio")]
    Failed,
}

/// Separa os oradores de `samples` (16 kHz mono). Bloqueante e pesado: `spawn_blocking`.
#[cfg(feature = "local")]
pub fn diarize(models_dir: &Path, samples: &[f32]) -> Result<Vec<Segment>, SpeakersError> {
    use sherpa_onnx::{
        FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
        OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
        SpeakerEmbeddingExtractorConfig,
    };
    if !is_installed(models_dir) {
        return Err(SpeakersError::ModelMissing);
    }
    if samples.len() > MAX_SAMPLES {
        return Err(SpeakersError::TooLong);
    }
    let dir = dir(models_dir);
    let threads = std::thread::available_parallelism()
        .map_or(2, |n| n.get().min(8))
        .try_into()
        .unwrap_or(2);
    let config = OfflineSpeakerDiarizationConfig {
        segmentation: OfflineSpeakerSegmentationModelConfig {
            pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                model: Some(dir.join(SEGMENTATION.name).display().to_string()),
                ..Default::default()
            },
            num_threads: threads,
            ..Default::default()
        },
        embedding: SpeakerEmbeddingExtractorConfig {
            model: Some(dir.join(EMBEDDING.name).display().to_string()),
            num_threads: threads,
            ..Default::default()
        },
        // Número de oradores desconhecido: o limiar decide quando dois são o mesmo.
        clustering: FastClusteringConfig {
            num_clusters: -1,
            threshold: 0.5,
            ..Default::default()
        },
        ..Default::default()
    };
    let engine = OfflineSpeakerDiarization::create(&config).ok_or(SpeakersError::Failed)?;
    if engine.sample_rate() != 16_000 {
        return Err(SpeakersError::Failed);
    }
    let result = engine.process(samples).ok_or(SpeakersError::Failed)?;
    let mut segments: Vec<Segment> = result
        .sort_by_start_time()
        .into_iter()
        .map(|s| Segment {
            start_ms: (s.start.max(0.0) * 1000.0) as u64,
            end_ms: (s.end.max(0.0) * 1000.0) as u64,
            speaker: s.speaker.max(0) as u32,
        })
        .collect();
    renumber(&mut segments);
    Ok(segments)
}

#[cfg(not(feature = "local"))]
pub fn diarize(_models_dir: &Path, _samples: &[f32]) -> Result<Vec<Segment>, SpeakersError> {
    Err(SpeakersError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(track: Track, start: u64, end: u64) -> Line {
        Line {
            track,
            start_ms: start,
            end_ms: end,
            text: "x".into(),
            failed: false,
            speaker: None,
        }
    }

    fn seg(start: u64, end: u64, speaker: u32) -> Segment {
        Segment {
            start_ms: start,
            end_ms: end,
            speaker,
        }
    }

    #[test]
    fn assign_speaker_by_max_overlap() {
        let segs = [seg(0, 4_000, 1), seg(4_000, 10_000, 2)];
        // 1 s com o orador 1 e 5 s com o 2: ganha o 2.
        let mut lines = [line(Track::System, 3_000, 9_000)];
        assign(&mut lines, Track::System, &segs);
        assert_eq!(lines[0].speaker, Some(2));
    }

    #[test]
    fn overlaps_of_the_same_speaker_add_up() {
        // O orador 1 aparece em dois trechos (2 s + 2 s) e o 2 num de 3 s.
        let segs = [seg(0, 2_000, 1), seg(2_000, 5_000, 2), seg(5_000, 7_000, 1)];
        let mut lines = [line(Track::System, 0, 7_000)];
        assign(&mut lines, Track::System, &segs);
        assert_eq!(lines[0].speaker, Some(1));
    }

    #[test]
    fn no_overlap_stays_unknown() {
        let segs = [seg(0, 1_000, 1)];
        let mut lines = [line(Track::System, 5_000, 6_000)];
        assign(&mut lines, Track::System, &segs);
        assert_eq!(lines[0].speaker, None);
        // Encostado mas sem sobreposição também não conta.
        let mut touching = [line(Track::System, 1_000, 2_000)];
        assign(&mut touching, Track::System, &segs);
        assert_eq!(touching[0].speaker, None);
    }

    #[test]
    fn only_the_diarized_track_is_touched() {
        let segs = [seg(0, 10_000, 1)];
        let mut lines = [
            line(Track::Mic, 1_000, 2_000),
            line(Track::System, 1_000, 2_000),
        ];
        assign(&mut lines, Track::System, &segs);
        assert_eq!(lines[0].speaker, None, "o microfone continua a ser 'Eu'");
        assert_eq!(lines[1].speaker, Some(1));
    }

    #[test]
    fn speakers_are_numbered_by_first_appearance() {
        // O motor chamou "7" a quem falou primeiro e "3" ao segundo.
        let mut segs = [seg(5_000, 6_000, 3), seg(0, 2_000, 7), seg(8_000, 9_000, 7)];
        renumber(&mut segs);
        let by_start: Vec<(u64, u32)> = segs.iter().map(|s| (s.start_ms, s.speaker)).collect();
        assert_eq!(by_start, [(0, 1), (5_000, 2), (8_000, 1)]);
    }

    #[test]
    fn catalog_hashes_are_64_hex() {
        for f in FILES {
            assert_eq!(f.sha256.len(), 64, "{}", f.name);
            assert!(f.sha256.chars().all(|c| c.is_ascii_hexdigit()));
            assert_eq!(f.commit.len(), 40);
        }
    }

    #[test]
    fn not_installed_without_files() {
        let dir = std::env::temp_dir().join(format!("ditok-speakers-{}", std::process::id()));
        assert!(!is_installed(&dir));
        assert!(delete(&dir).is_ok(), "apagar o que não existe não falha");
    }

    /// Com os modelos e um WAV reais:
    /// `DITOK_SPEAKERS=<pasta dos modelos> DITOK_WAV=<wav> cargo test --features local live_ -- --ignored --nocapture`
    #[cfg(feature = "local")]
    #[test]
    #[ignore]
    fn live_diarization() {
        let models = PathBuf::from(std::env::var("DITOK_SPEAKERS").expect("DITOK_SPEAKERS"));
        let wav = std::fs::read(std::env::var("DITOK_WAV").expect("DITOK_WAV")).unwrap();
        let (samples, rate) = crate::audio::wav::decode_pcm16_mono(&wav).unwrap();
        assert_eq!(rate, 16_000);
        let started = std::time::Instant::now();
        let segs = diarize(&models, &samples).unwrap();
        println!(
            "{} segmentos, {} oradores, em {:?} para {:.0} s de áudio",
            segs.len(),
            segs.iter().map(|s| s.speaker).max().unwrap_or(0),
            started.elapsed(),
            samples.len() as f32 / 16_000.0
        );
        for s in &segs {
            println!(
                "  {:>6} → {:>6} ms  orador {}",
                s.start_ms, s.end_ms, s.speaker
            );
        }
        assert!(!segs.is_empty());
    }
}
