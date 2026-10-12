//! As duas faixas de uma reunião: o microfone ("Eu") e o que o sistema está a tocar
//! ("Outros", por loopback). Cada faixa tem a sua thread, dona do seu `cpal::Stream`
//! (que não é `Send`), que esvazia o ring buffer a cada 50 ms, passa o áudio ao
//! `Segmenter` e entrega os enunciados já em 16 kHz mono.
//!
//! O loopback do Windows (WASAPI) **não chama o callback enquanto nada toca**: sem
//! cuidado, "o tempo" da faixa só avançava quando houvesse som e as horas dos
//! enunciados ficavam todas erradas. Cada faixa conta por isso o relógio de parede
//! desde que abriu e, se faltar áudio, preenche com zeros (ver `Timeline`).

use std::{
    sync::mpsc::{self, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use cpal::traits::HostTrait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use super::segmenter::{Cut, Segmenter};
use crate::audio::{
    AudioError, InputDevice, RawInput,
    resample::{self, To16k},
};

const TICK: Duration = Duration::from_millis(50);
/// Falta de áudio acima disto é preenchida com silêncio. Abaixo, é só a irregularidade
/// normal da entrega dos blocos.
const GAP_FILL: Duration = Duration::from_millis(300);
/// Com que frequência se vê se o dispositivo mudou ou morreu.
const DEVICE_CHECK: Duration = Duration::from_secs(2);
/// Quando o nível é enviado à interface.
const LEVEL_EVERY: Duration = Duration::from_millis(100);
/// Se o sistema só entregar silêncio *exato* durante este tempo, avisa-se: no macOS é
/// o que acontece sem a permissão de captura (falha em silêncio), e com conteúdo
/// protegido (DRM) em qualquer sistema.
const MUTE_AFTER: Duration = Duration::from_secs(15);

/// Qual das duas faixas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Track {
    /// O microfone: quem está à frente do computador.
    Mic,
    /// O áudio do sistema: o resto da reunião.
    System,
}

impl Track {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mic => "microfone",
            Self::System => "sistema",
        }
    }
}

/// O que se grava. Reunião = os dois; só sistema = um vídeo ou uma aula online;
/// presencial = só o microfone.
#[derive(Debug, Clone, Default)]
pub struct CaptureConfig {
    pub mic: bool,
    pub system: bool,
    /// Microfone escolhido nas definições (`None` = o predefinido).
    pub mic_device: Option<String>,
    /// Dispositivo de saída a captar (`None` = o predefinido, e acompanha-o se mudar).
    pub output_device: Option<String>,
}

/// Um enunciado pronto a transcrever.
#[derive(Debug, Clone)]
pub struct Utterance {
    pub track: Track,
    /// Milissegundos desde o início da sessão.
    pub start_ms: u64,
    pub end_ms: u64,
    /// 16 kHz mono.
    pub samples: Vec<f32>,
}

#[derive(Debug)]
pub enum CaptureEvent {
    Utterance(Utterance),
    /// Nível 0–1 de uma faixa, ~10 vezes por segundo.
    Level(Track, f32),
    /// A faixa abriu (ou reabriu depois de uma mudança) este dispositivo.
    Device(Track, String),
    /// A faixa deixou de funcionar e não se conseguiu reabrir.
    Lost(Track, String),
    /// O sistema entrega áudio, mas só zeros.
    SystemMute,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("faixa {track}: {source}")]
    Open {
        track: &'static str,
        #[source]
        source: AudioError,
    },
    #[error("não há nada para gravar (nem microfone nem áudio do sistema)")]
    NothingToRecord,
    #[error("a thread de áudio não arrancou: {0}")]
    Thread(std::io::Error),
}

/// Quando se grava. Largar o `Capture` sem `stop()` também pára tudo, mas sem
/// entregar o que estava a meio.
pub struct Capture {
    stops: Vec<mpsc::Sender<()>>,
    threads: Vec<JoinHandle<()>>,
}

impl Capture {
    /// Abre as faixas pedidas. Só devolve `Ok` se **todas** abriram: uma reunião sem a
    /// metade dos participantes não é o que foi pedido.
    pub fn start(
        config: &CaptureConfig,
        events: UnboundedSender<CaptureEvent>,
    ) -> Result<Self, CaptureError> {
        let mut wanted = Vec::new();
        if config.mic {
            wanted.push((Track::Mic, config.mic_device.clone()));
        }
        if config.system {
            wanted.push((Track::System, config.output_device.clone()));
        }
        if wanted.is_empty() {
            return Err(CaptureError::NothingToRecord);
        }

        let t0 = Instant::now();
        let mut capture = Self {
            stops: Vec::new(),
            threads: Vec::new(),
        };
        for (track, device) in wanted {
            let (stop_tx, stop_rx) = mpsc::channel();
            // A thread diz por aqui se conseguiu abrir (o stream tem de nascer nela).
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let events = events.clone();
            let handle = thread::Builder::new()
                .name(format!("ditok-notes-{}", track.label()))
                .spawn(move || track_thread(track, device, t0, stop_rx, ready_tx, events))
                .map_err(CaptureError::Thread)?;
            capture.stops.push(stop_tx);
            capture.threads.push(handle);
            match ready_rx.recv() {
                Ok(Ok(())) => {}
                // `capture` é largado ao devolver o erro: o `Drop` pára as faixas que já abriram.
                Ok(Err(source)) => {
                    return Err(CaptureError::Open {
                        track: track.label(),
                        source,
                    });
                }
                Err(_) => {
                    return Err(CaptureError::Open {
                        track: track.label(),
                        source: AudioError::ThreadGone,
                    });
                }
            }
        }
        Ok(capture)
    }

    /// Pára, entrega o que estava a meio e espera pelas threads.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        for stop in self.stops.drain(..) {
            let _ = stop.send(());
        }
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Quantos zeros faltam para a faixa acompanhar o relógio de parede. Puro, para
/// se testar sem áudio: `fed` amostras já entregues, `incoming` a entregar agora.
fn silence_to_fill(rate: u32, elapsed: Duration, fed: u64, incoming: usize) -> usize {
    let expected = (elapsed.as_secs_f64() * f64::from(rate)) as u64;
    let have = fed + incoming as u64;
    let deficit = expected.saturating_sub(have);
    let threshold = (GAP_FILL.as_secs_f64() * f64::from(rate)) as u64;
    if deficit > threshold {
        deficit as usize
    } else {
        0
    }
}

/// A faixa aberta e o que é preciso para a cortar.
struct Lane {
    input: RawInput,
    segmenter: Segmenter,
    opened_at: Instant,
    /// Onde esta abertura começa na linha do tempo da sessão (ms). Diferente de zero
    /// quando o dispositivo foi reaberto a meio.
    base_ms: u64,
    /// Amostras já entregues ao segmentador (incluindo os zeros de preenchimento).
    fed: u64,
    raw_total: u64,
    raw_peak: f32,
    muted_reported: bool,
}

impl Lane {
    fn new(input: RawInput, t0: Instant) -> Self {
        let now = Instant::now();
        Self {
            segmenter: Segmenter::new(input.rate()),
            input,
            opened_at: now,
            base_ms: now.duration_since(t0).as_millis() as u64,
            fed: 0,
            raw_total: 0,
            raw_peak: 0.0,
            muted_reported: false,
        }
    }

    fn rate(&self) -> u32 {
        self.input.rate()
    }

    /// Converte um corte em enunciado da sessão.
    fn utterance(&self, track: Track, cut: Cut, to16k: &mut To16k) -> Option<Utterance> {
        let rate = u64::from(self.rate());
        let start_ms = self.base_ms + cut.start * 1000 / rate;
        let end_ms = start_ms + cut.samples.len() as u64 * 1000 / rate;
        match to16k.convert(&cut.samples, self.rate()) {
            Ok(samples) => Some(Utterance {
                track,
                start_ms,
                end_ms,
                samples,
            }),
            Err(e) => {
                tracing::warn!(error = %e, "enunciado descartado: não foi possível passar para 16 kHz");
                None
            }
        }
    }
}

/// Como se abre cada faixa.
fn open_input(track: Track, wanted: Option<&str>) -> Result<RawInput, AudioError> {
    let host = cpal::default_host();
    match track {
        Track::Mic => {
            // Um microfone que já não existe cai no predefinido, como nos ditados.
            RawInput::open(Instant::now(), false, None, wanted)
        }
        Track::System => {
            let device = wanted
                .and_then(|name| {
                    host.output_devices()
                        .ok()?
                        .find(|d| crate::audio::device_name(d).as_deref() == Some(name))
                })
                .or_else(|| host.default_output_device())
                .ok_or(AudioError::NoDevice)?;
            RawInput::open_loopback(&device)
        }
    }
}

/// O dispositivo predefinido de agora: serve para reabrir quando o utilizador troca
/// de auscultadores a meio da reunião.
fn default_name(track: Track) -> Option<String> {
    let host = cpal::default_host();
    let device = match track {
        Track::Mic => host.default_input_device(),
        Track::System => host.default_output_device(),
    }?;
    crate::audio::device_name(&device)
}

fn track_thread(
    track: Track,
    wanted: Option<String>,
    t0: Instant,
    stop: mpsc::Receiver<()>,
    ready: mpsc::SyncSender<Result<(), AudioError>>,
    events: UnboundedSender<CaptureEvent>,
) {
    let mut lane = match open_input(track, wanted.as_deref()) {
        Ok(input) => {
            let _ = events.send(CaptureEvent::Device(track, input.device().to_owned()));
            tracing::info!(
                track = track.label(),
                device = input.device(),
                rate = input.rate(),
                "faixa aberta"
            );
            let _ = ready.send(Ok(()));
            Lane::new(input, t0)
        }
        Err(e) => {
            tracing::error!(track = track.label(), error = %e, "não foi possível abrir a faixa");
            let _ = ready.send(Err(e));
            return;
        }
    };

    let mut to16k = To16k::default();
    let mut raw: Vec<f32> = Vec::new();
    let mut cuts: Vec<Cut> = Vec::new();
    let mut last_check = Instant::now();
    let mut last_level = Instant::now();
    let mut level_acc: Vec<f32> = Vec::new();
    // `None` = a faixa morreu e não se conseguiu reabrir: continua a tentar.
    let mut lost = false;

    loop {
        match stop.recv_timeout(TICK) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }

        // Dispositivo morto, ou o predefinido mudou (sem escolha explícita): reabrir.
        if last_check.elapsed() >= DEVICE_CHECK {
            last_check = Instant::now();
            let changed = wanted.is_none()
                && default_name(track).is_some_and(|name| name != lane.input.device());
            if lane.input.has_failed() || changed || lost {
                reopen(
                    track,
                    wanted.as_deref(),
                    t0,
                    &mut lane,
                    &mut to16k,
                    &mut lost,
                    &events,
                );
            }
        }
        if lost {
            continue;
        }

        raw.clear();
        lane.input.drain_into(&mut raw);
        lane.raw_total += raw.len() as u64;
        lane.raw_peak = raw.iter().fold(lane.raw_peak, |p, s| p.max(s.abs()));

        let zeros = silence_to_fill(lane.rate(), lane.opened_at.elapsed(), lane.fed, raw.len());
        if zeros > 0 {
            lane.segmenter.push(&vec![0.0; zeros], &mut cuts);
            lane.fed += zeros as u64;
        }
        lane.segmenter.push(&raw, &mut cuts);
        lane.fed += raw.len() as u64;

        for cut in cuts.drain(..) {
            if let Some(u) = lane.utterance(track, cut, &mut to16k) {
                let _ = events.send(CaptureEvent::Utterance(u));
            }
        }

        level_acc.extend_from_slice(&raw);
        if last_level.elapsed() >= LEVEL_EVERY {
            last_level = Instant::now();
            let _ = events.send(CaptureEvent::Level(track, resample::level(&level_acc)));
            level_acc.clear();
        }

        if track == Track::System
            && !lane.muted_reported
            && lane.opened_at.elapsed() >= MUTE_AFTER
            && lane.raw_total > 0
            && lane.raw_peak == 0.0
        {
            lane.muted_reported = true;
            let _ = events.send(CaptureEvent::SystemMute);
        }
    }

    // Fim: o que estava a meio é um enunciado também.
    if !lost {
        raw.clear();
        lane.input.drain_into(&mut raw);
        lane.segmenter.push(&raw, &mut cuts);
        lane.segmenter.finish(&mut cuts);
        for cut in cuts.drain(..) {
            if let Some(u) = lane.utterance(track, cut, &mut to16k) {
                let _ = events.send(CaptureEvent::Utterance(u));
            }
        }
    }
    if lane.input.dropped() > 0 {
        tracing::warn!(
            track = track.label(),
            amostras = lane.input.dropped(),
            "amostras perdidas"
        );
    }
    tracing::debug!(track = track.label(), "faixa terminada");
}

/// Fecha a faixa atual (entregando o que tinha) e abre outra. Se falhar, `lost`
/// fica ligado e a thread volta a tentar daqui a `DEVICE_CHECK`.
fn reopen(
    track: Track,
    wanted: Option<&str>,
    t0: Instant,
    lane: &mut Lane,
    to16k: &mut To16k,
    lost: &mut bool,
    events: &UnboundedSender<CaptureEvent>,
) {
    let mut cuts = Vec::new();
    if !*lost {
        let mut raw = Vec::new();
        lane.input.drain_into(&mut raw);
        lane.segmenter.push(&raw, &mut cuts);
        lane.segmenter.finish(&mut cuts);
        for cut in cuts.drain(..) {
            if let Some(u) = lane.utterance(track, cut, to16k) {
                let _ = events.send(CaptureEvent::Utterance(u));
            }
        }
    }
    match open_input(track, wanted) {
        Ok(input) => {
            tracing::info!(
                track = track.label(),
                device = input.device(),
                "faixa reaberta"
            );
            let _ = events.send(CaptureEvent::Device(track, input.device().to_owned()));
            *lane = Lane::new(input, t0);
            *lost = false;
        }
        Err(e) => {
            tracing::warn!(track = track.label(), error = %e, "não foi possível reabrir a faixa");
            if !*lost {
                let _ = events.send(CaptureEvent::Lost(track, e.to_string()));
            }
            *lost = true;
        }
    }
}

/// Os dispositivos de saída que se podem captar, para o seletor. O predefinido vem primeiro.
pub fn output_devices() -> Result<Vec<InputDevice>, AudioError> {
    let host = cpal::default_host();
    let default = host
        .default_output_device()
        .as_ref()
        .and_then(crate::audio::device_name);
    let mut list: Vec<InputDevice> = Vec::new();
    for device in host.output_devices()? {
        let Some(name) = crate::audio::device_name(&device) else {
            continue;
        };
        if list.iter().any(|d| d.name == name) {
            continue;
        }
        list.push(InputDevice {
            is_default: default.as_deref() == Some(name.as_str()),
            name,
        });
    }
    list.sort_by_key(|d| !d.is_default);
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_gap_fills_zeros() {
        // 5 s de relógio, nada recebido (loopback sem som): faltam 5 s.
        assert_eq!(
            silence_to_fill(16_000, Duration::from_secs(5), 0, 0),
            80_000
        );
        // Já se tinha entregue 1 s e chegam 0,5 s: faltam 3,5 s.
        assert_eq!(
            silence_to_fill(16_000, Duration::from_secs(5), 16_000, 8_000),
            56_000
        );
    }

    #[test]
    fn normal_jitter_is_not_filled() {
        // 1 s de relógio, 0,94 s entregues: 60 ms de atraso é a entrega aos blocos.
        assert_eq!(
            silence_to_fill(48_000, Duration::from_secs(1), 0, 45_000),
            0
        );
        // Mais áudio do que o relógio (deriva do hardware): nunca é negativo.
        assert_eq!(
            silence_to_fill(48_000, Duration::from_secs(1), 0, 48_100),
            0
        );
    }

    #[test]
    fn gap_just_over_threshold_is_filled() {
        // 301 ms de falta a 16 kHz = 4816 amostras.
        let n = silence_to_fill(16_000, Duration::from_millis(1000), 0, 16_000 - 4_816);
        assert_eq!(n, 4_816);
        let n = silence_to_fill(16_000, Duration::from_millis(1000), 0, 16_000 - 4_700);
        assert_eq!(n, 0);
    }

    #[test]
    fn nothing_to_record_is_an_error() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let err = Capture::start(&CaptureConfig::default(), tx).err();
        assert!(matches!(err, Some(CaptureError::NothingToRecord)));
    }

    #[test]
    fn track_labels_serialize_kebab() {
        assert_eq!(serde_json::to_string(&Track::System).unwrap(), "\"system\"");
        assert_eq!(serde_json::to_string(&Track::Mic).unwrap(), "\"mic\"");
    }
}
