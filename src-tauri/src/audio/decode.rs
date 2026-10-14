//! Ler ficheiros de áudio e vídeo (mp3, m4a, wav, flac, ogg, mp4…) e parti-los em
//! pedaços para transcrever, cortando nas pausas.
//!
//! Porquê pedaços: é o caminho dos ficheiros que não convém enviar inteiros (um vídeo de
//! 300 MB por hora, um WAV de estúdio, qualquer coisa acima dos 500 MB que a xAI aceita) e a
//! reserva quando o envio inteiro falha (`files.rs`). Só se envia o áudio: uma aula de 1 h
//! são 115 MB em WAV de 16 kHz, em pedaços de ~2,5 min (~5 MB) que podem ir vários ao mesmo
//! tempo, com progresso a sério e só se perde o pedaço que falhar.
//!
//! Porquê nas pausas: um corte a meio de uma palavra estraga-a dos dois lados.
//! Perto do sítio onde queríamos cortar, procura-se o trecho mais silencioso.
//!
//! Tudo aqui é bloqueante (lê o disco e gasta CPU): corre em `spawn_blocking`.
//! Nunca há mais do que ~3 min de áudio descodificado em memória de cada vez.

use std::{fs::File, io::ErrorKind, path::Path, time::Duration};

use bytes::Bytes;
use symphonia::core::{
    audio::SampleBuffer,
    codecs::{CODEC_TYPE_NULL, Decoder, DecoderOptions},
    errors::Error as SymphoniaError,
    formats::{FormatOptions, FormatReader},
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
};

use super::{
    resample::{self, TARGET_RATE, To16k},
    vad, wav,
};

/// Duração que se procura para cada pedaço…
const CHUNK: Duration = Duration::from_secs(150);
/// …com esta folga para cada lado, para encontrar uma pausa.
const SEARCH: Duration = Duration::from_secs(15);
/// Janela usada para medir o silêncio: uma pausa entre frases, não entre sílabas.
const PAUSE: Duration = Duration::from_millis(300);
/// Um resto mais curto do que isto no fim do ficheiro não vale um pedido.
const MIN_TAIL: Duration = Duration::from_millis(300);
/// Um pedaço só é saltado se não tiver voz E for mesmo silencioso. Numa aula,
/// um professor longe do microfone pode ficar abaixo do limiar da deteção de
/// voz (afinada para quem fala para o microfone): perder texto é pior do que
/// pagar uns segundos a mais.
const SILENT_PEAK_DB: f32 = -45.0;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("não foi possível ler o ficheiro: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "formato não suportado. Funciona com mp3, m4a, aac, wav, flac, ogg, opus, mp4, mov e mkv"
    )]
    Unsupported,
    #[error("o ficheiro não tem áudio")]
    NoAudio,
    #[error("o áudio do ficheiro está danificado ({0})")]
    Corrupt(String),
    #[error(transparent)]
    Resample(#[from] resample::ResampleError),
}

impl From<SymphoniaError> for DecodeError {
    fn from(e: SymphoniaError) -> Self {
        match e {
            SymphoniaError::IoError(e) => Self::Io(e),
            SymphoniaError::Unsupported(_) => Self::Unsupported,
            other => Self::Corrupt(other.to_string()),
        }
    }
}

/// O que se sabe de um ficheiro antes de o descodificar.
#[derive(Debug, Clone, Copy)]
pub struct FileInfo {
    /// `None` quando o ficheiro não o diz (alguns mp3).
    pub duration: Option<Duration>,
    pub sample_rate: u32,
}

/// Um pedaço pronto a enviar.
#[derive(Debug)]
pub struct Chunk {
    pub index: usize,
    /// Onde começa no ficheiro.
    pub start: Duration,
    /// Quanto dura no ficheiro.
    pub length: Duration,
    /// WAV 16 kHz mono. `None` se o pedaço só tem silêncio (um intervalo da
    /// aula): não é enviado.
    pub wav: Option<Bytes>,
}

struct Opened {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    info: FileInfo,
}

fn open(path: &Path) -> Result<Opened, DecodeError> {
    let source = MediaSourceStream::new(Box::new(File::open(path)?), Default::default());
    // A extensão ajuda a escolher o formato; o conteúdo é que decide.
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe().format(
        &hint,
        source,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let reader = probed.format;

    // Num vídeo também há uma faixa de imagem: queremos a primeira de áudio.
    let track = reader
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL && t.codec_params.sample_rate.is_some())
        .ok_or(DecodeError::NoAudio)?;
    let track_id = track.id;
    // Copiamos os parâmetros: `track` é um empréstimo de `reader`, que vai ser
    // movido para dentro do `Opened` já a seguir.
    let params = track.codec_params.clone();
    let sample_rate = params.sample_rate.ok_or(DecodeError::NoAudio)?;
    let duration = match (params.n_frames, params.time_base) {
        (Some(n), Some(base)) => {
            let t = base.calc_time(n);
            Some(Duration::from_secs(t.seconds) + Duration::from_secs_f64(t.frac))
        }
        (Some(n), None) => Some(Duration::from_secs_f64(n as f64 / f64::from(sample_rate))),
        _ => None,
    };
    let decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions::default())?;
    Ok(Opened {
        reader,
        decoder,
        track_id,
        info: FileInfo {
            duration,
            sample_rate,
        },
    })
}

/// Lê só o cabeçalho: serve para confirmar que o formato é suportado e mostrar a
/// duração antes de começar.
pub fn probe(path: &Path) -> Result<FileInfo, DecodeError> {
    open(path).map(|o| o.info)
}

/// Descodifica o ficheiro todo e entrega cada pedaço a `on_chunk`, pela ordem.
/// Se `on_chunk` devolver `false` (cancelado), pára.
pub fn decode(path: &Path, mut on_chunk: impl FnMut(Chunk) -> bool) -> Result<(), DecodeError> {
    let Opened {
        mut reader,
        mut decoder,
        track_id,
        info,
    } = open(path)?;
    let rate = info.sample_rate;
    let mut chunker = Chunker::new(rate, CHUNK, SEARCH, PAUSE);
    let mut to16k = To16k::default();
    let mut buffer: Option<SampleBuffer<f32>> = None;
    let mut mono = Vec::new();
    let mut pieces = Vec::new();

    loop {
        let packet = match reader.next_packet() {
            Ok(packet) => packet,
            // O fim do ficheiro chega como um erro de leitura "UnexpectedEof".
            Err(SymphoniaError::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => break,
            // Outro ficheiro encadeado no mesmo (raro): ficamos pelo primeiro.
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // Um pacote estragado são uns milissegundos de áudio: não vale a pena
            // desistir da aula toda por causa dele.
            Err(SymphoniaError::DecodeError(e)) => {
                tracing::debug!(error = e, "pacote de áudio ignorado");
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count();
        // O buffer de conversão para f32 é reaproveitado entre pacotes; só é
        // recriado se um pacote não couber.
        let needed = decoded.capacity() * channels;
        if buffer.as_ref().is_none_or(|b| b.capacity() < needed) {
            buffer = Some(SampleBuffer::new(decoded.capacity() as u64, spec));
        }
        let Some(buf) = buffer.as_mut() else { continue };
        buf.copy_interleaved_ref(decoded);

        mono.clear();
        resample::downmix(buf.samples(), channels, |s| s, |s| mono.push(s));
        chunker.push(&mono, &mut pieces);
        for piece in pieces.drain(..) {
            if !on_chunk(prepare(piece, rate, &mut to16k)?) {
                return Ok(());
            }
        }
    }
    if let Some(piece) = chunker.finish() {
        on_chunk(prepare(piece, rate, &mut to16k)?);
    }
    Ok(())
}

/// Passa um pedaço para 16 kHz, decide se tem voz e codifica-o em WAV.
fn prepare(piece: Piece, rate: u32, to16k: &mut To16k) -> Result<Chunk, DecodeError> {
    let mut samples = to16k.convert(&piece.samples, rate)?;
    let voice = vad::detect(&samples, TARGET_RATE, false);
    let silent = !voice.has_speech() && voice.peak_db < SILENT_PEAK_DB;
    if !silent {
        resample::condition_speech(&mut samples, TARGET_RATE);
    }
    Ok(Chunk {
        index: piece.index,
        start: seconds(piece.start, rate),
        length: seconds(piece.samples.len(), rate),
        wav: (!silent).then(|| wav::encode_pcm16_mono(&samples, TARGET_RATE)),
    })
}

fn seconds(samples: usize, rate: u32) -> Duration {
    Duration::from_secs_f64(samples as f64 / f64::from(rate))
}

// ---------------------------------------------------------------- cortes

/// Um pedaço ainda à taxa original do ficheiro.
#[derive(Debug)]
pub struct Piece {
    pub index: usize,
    /// Primeira amostra, contada desde o início do ficheiro.
    pub start: usize,
    pub samples: Vec<f32>,
}

/// Junta as amostras que vão chegando e, sempre que há o suficiente, corta um
/// pedaço na pausa mais silenciosa perto do tamanho pretendido. Não sabe nada de
/// ficheiros: recebe amostras e devolve pedaços, por isso testa-se sem disco.
pub struct Chunker {
    rate: usize,
    target: usize,
    search: usize,
    pause: usize,
    pending: Vec<f32>,
    /// Amostras já entregues em pedaços anteriores.
    consumed: usize,
    next_index: usize,
}

impl Chunker {
    pub fn new(rate: u32, target: Duration, search: Duration, pause: Duration) -> Self {
        let samples = |d: Duration| (d.as_secs_f64() * f64::from(rate)) as usize;
        Self {
            rate: rate as usize,
            target: samples(target),
            search: samples(search),
            pause: samples(pause).max(1),
            pending: Vec::new(),
            consumed: 0,
            next_index: 0,
        }
    }

    /// Acrescenta amostras e põe em `out` os pedaços que ficaram completos.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<Piece>) {
        self.pending.extend_from_slice(samples);
        // Só se corta quando a zona de procura inteira já chegou.
        while self.pending.len() >= self.target + self.search {
            let from = self.target.saturating_sub(self.search).max(self.pause);
            let cut = quietest(
                &self.pending,
                from,
                self.target + self.search,
                self.pause,
                self.rate,
            );
            // `split_off` deixa em `pending` o pedaço e devolve o resto; o
            // `mem::replace` troca-os sem copiar as amostras do pedaço.
            let rest = self.pending.split_off(cut);
            let samples = std::mem::replace(&mut self.pending, rest);
            out.push(self.piece(samples));
        }
    }

    /// O que sobrou no fim do ficheiro, se não for só um resto minúsculo.
    pub fn finish(mut self) -> Option<Piece> {
        let min = (MIN_TAIL.as_secs_f64() * self.rate as f64) as usize;
        let samples = std::mem::take(&mut self.pending);
        (samples.len() >= min).then(|| self.piece(samples))
    }

    fn piece(&mut self, samples: Vec<f32>) -> Piece {
        let piece = Piece {
            index: self.next_index,
            start: self.consumed,
            samples,
        };
        self.consumed += piece.samples.len();
        self.next_index += 1;
        piece
    }
}

/// Onde cortar entre `from` e `to`: o meio da janela de `window` amostras com
/// menos energia. Em caso de empate (silêncio digital), a mais perto do centro,
/// para os pedaços ficarem com tamanhos parecidos.
fn quietest(samples: &[f32], from: usize, to: usize, window: usize, rate: usize) -> usize {
    // Energia em blocos de 20 ms e soma móvel de blocos (mais barato do que somar
    // a janela inteira em cada posição).
    let step = (rate / 50).max(1);
    let energies: Vec<f64> = samples[from..to]
        .chunks(step)
        .map(|c| c.iter().map(|&s| f64::from(s * s)).sum())
        .collect();
    let k = (window / step).max(1);
    if energies.len() <= k {
        return (from + to) / 2;
    }
    let center = (energies.len() - k) / 2;
    let mut sum: f64 = energies[..k].iter().sum();
    let mut best: (f64, usize) = (sum, 0);
    for i in k..energies.len() {
        sum += energies[i] - energies[i - k];
        let pos = i + 1 - k;
        let tie = (sum - best.0).abs() <= 1e-9;
        if (sum < best.0 && !tie) || (tie && pos.abs_diff(center) < best.1.abs_diff(center)) {
            best = (sum, pos);
        }
    }
    from + best.1 * step + window / 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const RATE: u32 = 16_000;

    /// "Fala" de `secs` segundos (um seno) seguida de `pause` segundos de silêncio.
    fn speech_with_pauses(blocks: &[(f32, f32)]) -> (Vec<f32>, Vec<std::ops::Range<usize>>) {
        let mut audio = Vec::new();
        let mut silences = Vec::new();
        for &(secs, pause) in blocks {
            let n = (secs * RATE as f32) as usize;
            audio.extend((0..n).map(|i| (TAU * 220.0 * i as f32 / RATE as f32).sin() * 0.3));
            let start = audio.len();
            audio.resize(start + (pause * RATE as f32) as usize, 0.0);
            silences.push(start..audio.len());
        }
        (audio, silences)
    }

    #[test]
    fn cuts_fall_in_pauses_and_nothing_is_lost() {
        // 10 min de frases de 6,5 s com pausas de 0,8 s; pedaços de ~40 s ± 5 s.
        let (audio, silences) = speech_with_pauses(&[(6.5, 0.8); 82]);
        let mut chunker = Chunker::new(
            RATE,
            Duration::from_secs(40),
            Duration::from_secs(5),
            Duration::from_millis(300),
        );
        let mut pieces = Vec::new();
        for block in audio.chunks(4096) {
            chunker.push(block, &mut pieces);
        }
        pieces.extend(chunker.finish());

        let total: usize = pieces.iter().map(|p| p.samples.len()).sum();
        assert_eq!(total, audio.len(), "as amostras somam o ficheiro inteiro");
        for (i, p) in pieces.iter().enumerate() {
            assert_eq!(p.index, i);
            let end = p.start + p.samples.len();
            if i + 1 < pieces.len() {
                assert_eq!(pieces[i + 1].start, end, "pedaços seguidos, sem buracos");
                let secs = p.samples.len() as f32 / RATE as f32;
                assert!((35.0..=45.0).contains(&secs), "pedaço {i}: {secs} s");
                assert!(
                    silences.iter().any(|s| s.contains(&end)),
                    "o corte do pedaço {i} ({end}) não caiu numa pausa"
                );
            }
        }
        assert!(pieces.len() >= 12, "{} pedaços", pieces.len());
    }

    #[test]
    fn short_audio_is_one_piece_and_a_tiny_tail_is_dropped() {
        let mut chunker =
            Chunker::new(RATE, Duration::from_secs(40), Duration::from_secs(5), PAUSE);
        let mut pieces = Vec::new();
        chunker.push(&vec![0.1; RATE as usize * 3], &mut pieces);
        assert!(pieces.is_empty());
        assert_eq!(
            chunker.finish().map(|p| p.samples.len()),
            Some(RATE as usize * 3)
        );

        let mut chunker =
            Chunker::new(RATE, Duration::from_secs(40), Duration::from_secs(5), PAUSE);
        chunker.push(&vec![0.1; 100], &mut pieces);
        assert!(chunker.finish().is_none(), "6 ms não valem um pedido");
    }

    #[test]
    fn digital_silence_is_cut_near_the_target() {
        let audio = vec![0.0; RATE as usize * 50];
        let mut chunker =
            Chunker::new(RATE, Duration::from_secs(20), Duration::from_secs(5), PAUSE);
        let mut pieces = Vec::new();
        chunker.push(&audio, &mut pieces);
        let first = pieces[0].samples.len() as f32 / RATE as f32;
        assert!((first - 20.0).abs() < 0.5, "{first} s");
    }

    /// Um WAV escrito por nós, lido de volta pelo symphonia, pedaço a pedaço.
    #[test]
    fn decodes_a_wav_file_into_chunks() {
        let (audio, _) = speech_with_pauses(&[(6.5, 0.8); 50]); // ~6 min
        let silence = vec![0.0; RATE as usize * 400]; // um intervalo da aula
        let all: Vec<f32> = [audio.as_slice(), &silence, &audio[..RATE as usize * 20]].concat();
        let dir = std::env::temp_dir().join(format!("ditok-decode-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aula.wav");
        std::fs::write(&path, wav::encode_pcm16_mono(&all, RATE)).unwrap();

        let info = probe(&path).unwrap();
        let expected = all.len() as f64 / f64::from(RATE);
        assert!((info.duration.unwrap().as_secs_f64() - expected).abs() < 0.01);

        let mut chunks = Vec::new();
        decode(&path, |c| {
            chunks.push(c);
            true
        })
        .unwrap();
        let total: f64 = chunks.iter().map(|c| c.length.as_secs_f64()).sum();
        assert!((total - expected).abs() < 0.01, "{total} vs {expected}");
        assert!(
            chunks.iter().any(|c| c.wav.is_none()),
            "o intervalo em silêncio é saltado"
        );
        assert!(chunks[0].wav.is_some());
        // Cancelar a meio: pára logo.
        let mut seen = 0;
        decode(&path, |_| {
            seen += 1;
            false
        })
        .unwrap();
        assert_eq!(seen, 1);

        assert!(matches!(
            probe(&dir.join("não-existe.mp3")),
            Err(DecodeError::Io(_))
        ));
        std::fs::write(dir.join("texto.mp3"), b"isto nao e audio").unwrap();
        assert!(probe(&dir.join("texto.mp3")).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
