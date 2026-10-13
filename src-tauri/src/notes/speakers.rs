//! Quem fala dentro de "Outros": diarização de oradores, neste computador.
//!
//! Corre **depois** da reunião, sobre o áudio da faixa dos outros que se foi guardando
//! num ficheiro temporário (`diar.pcm`, apagado no fim): um modelo de segmentação
//! (pyannote) marca onde fala alguém, um de "impressão da voz" mede cada trecho, e um
//! agrupamento junta os trechos parecidos em "Orador 1, 2…". A segmentação é a mesma
//! para os três modelos de voz (WeSpeaker, TitaNet small e large), que o utilizador
//! escolhe nas definições. É o motor `sherpa-onnx` do STT local, por isso só existe
//! com a feature `local`.
//!
//! A atribuição às linhas já transcritas (`assign`) e a renumeração (`renumber`) são
//! puras e testam-se sem modelos.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{capture::Track, store::Line};
use crate::local_models::{self, DownloadError, Job};

/// Esta compilação sabe separar oradores?
pub const AVAILABLE: bool = cfg!(feature = "local");

/// Mais do que isto (em amostras a 16 kHz) não se processa: o motor trabalha com o
/// áudio todo em memória (~230 MB por hora).
pub const MAX_SAMPLES: usize = 90 * 60 * 16_000;

/// O limiar de clustering: quando dois trechos são o mesmo orador. Afinado com o
/// WeSpeaker; os TitaNet usam o mesmo até serem calibrados com áudio em português.
#[cfg_attr(not(feature = "local"), allow(dead_code))] // só o motor local o usa
const THRESHOLD: f32 = 0.7;

/// O modelo de "impressão da voz" escolhido nas definições (o que se usa nas Notas).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Model {
    /// WeSpeaker ResNet34 (VoxCeleb, inglês): o que a app usava desde o início.
    #[default]
    #[serde(rename = "wespeaker")]
    WeSpeaker,
    /// NVIDIA NeMo TitaNet small: mais leve.
    #[serde(rename = "titanet-small")]
    TitaNetSmall,
    /// NVIDIA NeMo TitaNet large: o maior dos três.
    #[serde(rename = "titanet-large")]
    TitaNetLarge,
}

struct File {
    name: &'static str,
    url: &'static str,
    size: u64,
    sha256: &'static str,
}

const SEGMENTATION: File = File {
    name: "segmentation.int8.onnx",
    url: "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/9403a6902bb58e3d5ae8c7e77c3422de279db2e0/model.int8.onnx",
    size: 1_540_506,
    sha256: "d582f4b4c6b48205de7e0643c57df0df5615a3c176189be3fc461e9d18827b5d",
};

// Os TitaNet vêm da release de modelos de reconhecimento de oradores do sherpa-onnx (o
// nome da tag tem mesmo esse erro de escrita). Os nomes locais são distintos por modelo,
// para os três poderem estar descarregados ao mesmo tempo; o WeSpeaker mantém o nome
// antigo, para quem já o tem.
const WESPEAKER: File = File {
    name: "embedding.onnx",
    url: "https://huggingface.co/csukuangfj/speaker-embedding-models/resolve/0743f301363dec56491a490f6d6cbc9d67f9a3bf/wespeaker_en_voxceleb_resnet34.onnx",
    size: 26_534_365,
    sha256: "5ef208a9da1453335308a6b6f4e6dfbd7e183a38b604de0a57664f45d257fe94",
};

const TITANET_SMALL: File = File {
    name: "embedding-titanet-small.onnx",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/nemo_en_titanet_small.onnx",
    size: 40_257_283,
    sha256: "ad4a1802485d8b34c722d2a9d04249662f2ece5d28a7a039063ca22f515a789e",
};

const TITANET_LARGE: File = File {
    name: "embedding-titanet-large.onnx",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/nemo_en_titanet_large.onnx",
    size: 101_405_493,
    sha256: "d51abcf31717ef28162f26acb9d44dd4127c3d44c9b8624f699f3425daca8e77",
};

impl Model {
    pub const ALL: [Model; 3] = [Model::WeSpeaker, Model::TitaNetSmall, Model::TitaNetLarge];

    pub fn label(self) -> &'static str {
        match self {
            Self::WeSpeaker => "WeSpeaker ResNet34",
            Self::TitaNetSmall => "TitaNet small",
            Self::TitaNetLarge => "TitaNet large",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::WeSpeaker => "O que a app já usava. Treinado sobretudo com inglês.",
            Self::TitaNetSmall => {
                "Mais leve e rápido. Treinado com inglês. Ainda sem testes em português."
            }
            Self::TitaNetLarge => {
                "O maior dos três: mais lento e pesado. Treinado com inglês. Ainda sem testes em português."
            }
        }
    }

    fn embedding(self) -> &'static File {
        match self {
            Self::WeSpeaker => &WESPEAKER,
            Self::TitaNetSmall => &TITANET_SMALL,
            Self::TitaNetLarge => &TITANET_LARGE,
        }
    }

    /// Os ficheiros que este modelo precisa: a segmentação (partilhada) e a sua voz.
    fn files(self) -> [&'static File; 2] {
        [&SEGMENTATION, self.embedding()]
    }

    pub fn total_bytes(self) -> u64 {
        self.files().iter().map(|f| f.size).sum()
    }
}

pub fn dir(models_dir: &Path) -> PathBuf {
    models_dir.join("speakers")
}

fn file_ok(path: &Path, size: u64) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() == size)
}

/// O modelo escolhido está todo no disco (segmentação e voz, com o tamanho certo).
pub fn is_installed(models_dir: &Path, model: Model) -> bool {
    let dir = dir(models_dir);
    model
        .files()
        .iter()
        .all(|f| file_ok(&dir.join(f.name), f.size))
}

/// Apaga a voz do modelo. A segmentação só sai quando nenhum outro modelo a usa.
pub fn delete(models_dir: &Path, model: Model) -> std::io::Result<()> {
    let dir = dir(models_dir);
    let mut names = vec![model.embedding().name.to_owned()];
    if !Model::ALL
        .iter()
        .any(|&m| m != model && is_installed(models_dir, m))
    {
        names.push(SEGMENTATION.name.to_owned());
    }
    for name in names {
        for path in [dir.join(&name), dir.join(format!("{name}.part"))] {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
    }
    Ok(())
}

/// Descarrega o modelo (com retoma e verificação do SHA-256, como os do STT). A
/// segmentação só volta a descarregar se ainda não estiver no disco.
pub async fn download(
    client: &reqwest::Client,
    models_dir: &Path,
    model: Model,
    cancel: &std::sync::atomic::AtomicBool,
    on_progress: impl FnMut(local_models::Progress),
) -> Result<(), DownloadError> {
    let jobs: Vec<Job> = model
        .files()
        .iter()
        .map(|f| Job {
            url: f.url.to_owned(),
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
    #[error("falta o modelo de oradores escolhido: descarrega-o nas definições")]
    ModelMissing,
    #[error("a reunião é demasiado longa para separar os oradores (o limite são 90 minutos)")]
    TooLong,
    #[error("o motor não conseguiu processar o áudio")]
    Failed,
}

/// Separa os oradores de `samples` (16 kHz mono) com o `model` escolhido. Bloqueante e
/// pesado: `spawn_blocking`.
#[cfg(feature = "local")]
pub fn diarize(
    models_dir: &Path,
    model: Model,
    samples: &[f32],
) -> Result<Vec<Segment>, SpeakersError> {
    use sherpa_onnx::{
        FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
        OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
        SpeakerEmbeddingExtractorConfig,
    };
    if !is_installed(models_dir, model) {
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
            model: Some(dir.join(model.embedding().name).display().to_string()),
            num_threads: threads,
            ..Default::default()
        },
        // Número de oradores desconhecido: o limiar decide quando dois são o mesmo.
        // Mais alto junta mais vozes (menos oradores). 0.5 deu 15 oradores em 5 min.
        clustering: FastClusteringConfig {
            num_clusters: -1,
            threshold: THRESHOLD,
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
pub fn diarize(
    _models_dir: &Path,
    _model: Model,
    _samples: &[f32],
) -> Result<Vec<Segment>, SpeakersError> {
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
        for model in Model::ALL {
            for f in model.files() {
                assert_eq!(f.sha256.len(), 64, "{}", f.name);
                assert!(f.sha256.chars().all(|c| c.is_ascii_hexdigit()));
                assert!(f.url.starts_with("https://"), "{}", f.name);
            }
        }
    }

    #[test]
    fn each_model_has_its_own_voice_file() {
        let names: Vec<&str> = Model::ALL.iter().map(|m| m.embedding().name).collect();
        assert_eq!(
            names.len(),
            names.iter().collect::<std::collections::HashSet<_>>().len()
        );
    }

    #[test]
    fn not_installed_without_files() {
        let dir = std::env::temp_dir().join(format!("ditok-speakers-{}", std::process::id()));
        for model in Model::ALL {
            assert!(!is_installed(&dir, model));
            assert!(
                delete(&dir, model).is_ok(),
                "apagar o que não existe não falha"
            );
        }
    }

    /// Com os modelos e um WAV reais:
    /// `DITOK_SPEAKERS=<pasta dos modelos> DITOK_WAV=<wav> cargo test --features local live_ -- --ignored --nocapture`
    /// (`DITOK_MODEL` escolhe o modelo: `wespeaker`, `titanet-small` ou `titanet-large`.)
    #[cfg(feature = "local")]
    #[test]
    #[ignore]
    fn live_diarization() {
        let models = PathBuf::from(std::env::var("DITOK_SPEAKERS").expect("DITOK_SPEAKERS"));
        let model: Model = serde_json::from_value(serde_json::Value::String(
            std::env::var("DITOK_MODEL").unwrap_or_else(|_| "wespeaker".into()),
        ))
        .expect("DITOK_MODEL");
        let wav = std::fs::read(std::env::var("DITOK_WAV").expect("DITOK_WAV")).unwrap();
        let (samples, rate) = crate::audio::wav::decode_pcm16_mono(&wav).unwrap();
        assert_eq!(rate, 16_000);
        let started = std::time::Instant::now();
        let segs = diarize(&models, model, &samples).unwrap();
        println!(
            "{}: {} segmentos, {} oradores, em {:?} para {:.0} s de áudio",
            model.label(),
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
