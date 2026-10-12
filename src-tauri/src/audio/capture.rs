//! Captura do microfone numa thread dedicada.
//!
//! Há três "atores" envolvidos:
//! 1. **Callback do cpal** (thread de tempo real do sistema de áudio): converte para
//!    f32 mono e escreve num ring buffer lock-free. Não pode bloquear nem alocar, ou
//!    perdemos áudio, daí o `rtrb` em vez de um `Mutex<Vec>`.
//! 2. **Thread `ditok-audio`**: dona do `cpal::Stream` (que não é `Send` em todas as
//!    plataformas, por isso não pode viver numa tarefa tokio que muda de thread).
//!    Recebe comandos por um canal; enquanto grava, acorda a cada 50 ms para passar
//!    o conteúdo do ring buffer para um `Vec`. Quando não grava, fica bloqueada no
//!    `recv()` sem gastar CPU.
//! 3. **`Recorder`**: a "pega" usada pelo orquestrador. Só tem um `Sender`, por isso é
//!    `Send` e barata de guardar.

use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use bytes::Bytes;
use cpal::{
    FromSample, SampleFormat, SizedSample, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use tokio::sync::{mpsc::UnboundedSender, oneshot};

use super::{
    resample::{self, TARGET_RATE, To16k},
    vad::{self, Voice},
    wav,
};
use crate::dictation::DictationEvent;

/// De quanto em quanto tempo a thread esvazia o ring buffer durante a gravação.
const DRAIN_EVERY: Duration = Duration::from_millis(50);
/// Capacidade do ring buffer: muito acima dos 50 ms entre esvaziamentos, para
/// aguentar atrasos pontuais da thread sem perder amostras.
const RING_SECONDS: usize = 2;
/// Reserva inicial do buffer da gravação (cresce se for preciso).
const INITIAL_SECONDS: usize = 10;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("não foi encontrado nenhum microfone")]
    NoDevice,
    #[error("sem permissão para usar o microfone. {hint}", hint = MIC_PERMISSION_HELP)]
    PermissionDenied,
    #[error("o microfone usa um formato não suportado ({0:?})")]
    UnsupportedFormat(SampleFormat),
    #[error("erro do microfone: {0}")]
    Device(cpal::Error),
    #[error(transparent)]
    Resample(#[from] resample::ResampleError),
    #[error("não havia nenhuma gravação em curso")]
    NotRecording,
    #[error("a thread de áudio terminou inesperadamente")]
    ThreadGone,
}

/// Onde se dá a permissão, em cada sistema.
#[cfg(target_os = "windows")]
const MIC_PERMISSION_HELP: &str = "No Windows: Definições → Privacidade e segurança → Microfone; liga \"Acesso ao microfone\" e \"Permitir que as aplicações de ambiente de trabalho acedam ao microfone\".";
#[cfg(target_os = "macos")]
const MIC_PERMISSION_HELP: &str =
    "No macOS: Definições do Sistema → Privacidade e Segurança → Microfone, e liga o Ditok.";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const MIC_PERMISSION_HELP: &str = "Confirma as permissões de áudio do sistema.";

impl From<cpal::Error> for AudioError {
    fn from(e: cpal::Error) -> Self {
        if e.kind() == cpal::ErrorKind::PermissionDenied || is_access_denied(&e.to_string()) {
            Self::PermissionDenied
        } else {
            Self::Device(e)
        }
    }
}

/// No Windows, o cpal não traduz o "acesso negado" do WASAPI (E_ACCESSDENIED,
/// 0x80070005, que é o que acontece com o microfone bloqueado nas definições de
/// privacidade): chega como erro genérico do backend. Reconhecemo-lo pelo texto,
/// que pode vir com o código em hexadecimal, em decimal ou traduzido.
fn is_access_denied(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("0x80070005")
        || lower.contains("-2147024891")
        || lower.contains("access is denied")
        || lower.contains("acesso negado")
}

/// Resultado de uma gravação, já pronto a enviar.
/// `Clone` é barato: o WAV é `Bytes` (contagem de referências, não copia o áudio).
/// É o que permite guardar a gravação de um ditado falhado para o "Tentar de novo".
#[derive(Debug, Clone)]
pub struct Recording {
    /// WAV 16 kHz mono PCM16, sem o silêncio do início e do fim. Sem fala, fica a
    /// gravação inteira: não vai ser enviada, mas o WAV de diagnóstico mostra-a.
    pub wav: Bytes,
    /// Duração do áudio no WAV.
    pub audio: Duration,
    /// Silêncio cortado nas pontas.
    pub trimmed: Duration,
    /// Se há fala, e os níveis usados para o decidir.
    pub voice: Voice,
    pub device: String,
    pub device_rate: u32,
    pub device_channels: u16,
    /// Do premir do atalho até ao primeiro bloco de áudio chegar.
    pub mic_start: Option<Duration>,
    /// Do pedido de paragem até ao WAV estar pronto (esvaziar + resample + codificar).
    pub finalize: Duration,
    /// Amostras perdidas por o ring buffer encher (devia ser sempre 0).
    pub dropped_samples: usize,
}

/// Quanto tempo o medidor fica ligado sem ser renovado. A janela renova-o a cada
/// 2 s enquanto o passo do microfone está à vista; se a janela fechar sem o
/// desligar, o microfone fecha sozinho ao fim disto.
const MONITOR_KEEPALIVE: Duration = Duration::from_secs(6);

enum Command {
    Start {
        pressed_at: Instant,
        /// O som de início vai tocar: a deteção de voz ignora-o (ver `vad.rs`).
        start_cue: bool,
        /// O microfone escolhido nas definições (`None` = o predefinido).
        device: Option<String>,
    },
    Stop {
        reply: oneshot::Sender<Result<Recording, AudioError>>,
    },
    Cancel,
    /// Liga (ou renova) o medidor de nível fora de um ditado.
    MonitorStart {
        device: Option<String>,
    },
    MonitorStop,
}

/// `Clone` só copia o `Sender`: o orquestrador e os comandos da janela (o medidor
/// do microfone) falam com a mesma thread de áudio.
#[derive(Clone)]
pub struct Recorder {
    tx: mpsc::Sender<Command>,
}

/// Recebe o nível de áudio (0–1) a cada esvaziamento do ring buffer (~20 vezes por
/// segundo) enquanto se grava. `Box<dyn Fn>`: o módulo de áudio não precisa de
/// saber quem está a ouvir (a barra no ecrã); só que é uma função que pode ir para
/// outra thread (`Send`).
pub type LevelSink = Box<dyn Fn(f32) + Send>;

/// O que o medidor do microfone (fora de um ditado) conta à janela.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum MicEvent {
    /// O microfone que abriu de facto (pode ser o predefinido, se o escolhido falhou).
    Opened(String),
    /// Nível entre 0 e 1, ~20 vezes por segundo.
    Level(f32),
    /// Não abriu; a mensagem já vem pronta a mostrar.
    Failed(String),
}

pub type MicSink = Box<dyn Fn(MicEvent) + Send>;

impl Recorder {
    /// Lança a thread de áudio. O microfone só é aberto no `start`, não aqui.
    pub fn spawn(
        events: UnboundedSender<DictationEvent>,
        level: LevelSink,
        mic: MicSink,
    ) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("ditok-audio".into())
            .spawn(move || audio_thread(rx, events, level, mic))?;
        Ok(Self { tx })
    }

    /// Não espera que o microfone abra: o orquestrador continua livre. Se falhar, a
    /// thread envia `DictationEvent::AudioFailed`.
    pub fn start(&self, pressed_at: Instant, start_cue: bool, device: Option<String>) {
        let cmd = Command::Start {
            pressed_at,
            start_cue,
            device,
        };
        if self.tx.send(cmd).is_err() {
            tracing::error!("thread de áudio indisponível");
        }
    }

    /// Liga o medidor de nível (ou renova-o, se já estiver ligado). Ignorado
    /// durante um ditado: a gravação tem prioridade.
    pub fn monitor_start(&self, device: Option<String>) {
        let _ = self.tx.send(Command::MonitorStart { device });
    }

    pub fn monitor_stop(&self) {
        let _ = self.tx.send(Command::MonitorStop);
    }

    /// Pede para parar e devolve um recetor que entrega a gravação quando estiver
    /// pronta. `oneshot` = canal para exatamente uma resposta.
    pub fn stop(&self) -> impl Future<Output = Result<Recording, AudioError>> + use<> {
        let (reply, rx) = oneshot::channel();
        // Se o envio falhar, `reply` é largado e o `rx` devolve erro: tratado abaixo.
        let _ = self.tx.send(Command::Stop { reply });
        async move { rx.await.unwrap_or(Err(AudioError::ThreadGone)) }
    }

    pub fn cancel(&self) {
        let _ = self.tx.send(Command::Cancel);
    }
}

/// O medidor do microfone fora de um ditado: uma `Session` cujas amostras deito
/// fora depois de medir o nível.
struct Monitor {
    session: Session,
    /// O que foi pedido (para saber se mudou), não o que abriu de facto.
    wanted: Option<String>,
    until: Instant,
}

fn audio_thread(
    rx: mpsc::Receiver<Command>,
    events: UnboundedSender<DictationEvent>,
    level: LevelSink,
    mic: MicSink,
) {
    let mut session: Option<Session> = None;
    let mut monitor: Option<Monitor> = None;
    let mut to16k = To16k::default();

    loop {
        let cmd = if session.is_none() && monitor.is_none() {
            // Parado: bloqueia até chegar um comando (zero CPU).
            match rx.recv() {
                Ok(cmd) => cmd,
                Err(_) => break,
            }
        } else {
            // A gravar (ou a medir): acorda no próximo comando ou ao fim de
            // DRAIN_EVERY para esvaziar o ring buffer, o que vier primeiro.
            match rx.recv_timeout(DRAIN_EVERY) {
                Ok(cmd) => cmd,
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(s) = session.as_mut() {
                        let new_from = s.drain();
                        // Só as amostras que acabaram de chegar: o nível "de agora".
                        level(resample::level(&s.samples[new_from..]));
                    } else if let Some(m) = monitor.as_mut() {
                        if m.session.failed.load(Ordering::Relaxed) {
                            // O stream do medidor morreu (microfone desligado): a janela é avisada.
                            tracing::warn!("medidor do microfone: o stream falhou");
                            monitor = None;
                            mic(MicEvent::Failed("o microfone foi desligado".into()));
                        } else if Instant::now() >= m.until {
                            tracing::debug!("medidor do microfone parado (não foi renovado)");
                            monitor = None; // largar a sessão fecha o microfone
                        } else {
                            m.session.drain();
                            mic(MicEvent::Level(resample::level(&m.session.samples)));
                            // Só queremos o nível: não acumular minutos de áudio.
                            m.session.samples.clear();
                        }
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        };

        match cmd {
            Command::Start {
                pressed_at,
                start_cue,
                device,
            } => {
                if session.is_some() {
                    tracing::warn!("início pedido com uma gravação em curso; ignorado");
                    continue;
                }
                // Um ditado durante o guia: o medidor dá lugar à gravação (dois
                // streams no mesmo microfone nem sempre são permitidos).
                if monitor.take().is_some() {
                    tracing::debug!("medidor do microfone parado para gravar");
                }
                match Session::open(
                    pressed_at,
                    start_cue,
                    Some(events.clone()),
                    device.as_deref(),
                ) {
                    Ok(s) => session = Some(s),
                    Err(e) => {
                        tracing::error!(error = %e, "não foi possível abrir o microfone");
                        let _ = events.send(DictationEvent::AudioFailed(e.to_string()));
                    }
                }
            }
            Command::Stop { reply } => {
                let stop_at = Instant::now();
                let result = match session.take() {
                    Some(s) => s.finish(&mut to16k, stop_at),
                    None => Err(AudioError::NotRecording),
                };
                let _ = reply.send(result);
            }
            Command::Cancel => {
                // Largar a sessão fecha o stream (o Drop do cpal::Stream trata disso).
                if session.take().is_some() {
                    tracing::debug!("gravação descartada");
                }
            }
            Command::MonitorStart { device } => {
                if session.is_some() {
                    continue; // a meio de um ditado: a janela volta a pedir daqui a 2 s
                }
                let until = Instant::now() + MONITOR_KEEPALIVE;
                // `if let ... && ...` (let chains): renova só se for o mesmo microfone.
                if let Some(m) = monitor.as_mut()
                    && m.wanted == device
                {
                    m.until = until;
                    continue;
                }
                monitor = None; // outro microfone: fecha o anterior antes de abrir
                // Sem `events`: um erro do medidor não é um erro do ditado (ver `failed`).
                match Session::open(Instant::now(), false, None, device.as_deref()) {
                    Ok(s) => {
                        tracing::debug!(device = %s.device, "medidor do microfone ligado");
                        mic(MicEvent::Opened(s.device.clone()));
                        monitor = Some(Monitor {
                            session: s,
                            wanted: device,
                            until,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "medidor: não foi possível abrir o microfone");
                        mic(MicEvent::Failed(e.to_string()));
                    }
                }
            }
            Command::MonitorStop => {
                if monitor.take().is_some() {
                    tracing::debug!("medidor do microfone desligado");
                }
            }
        }
    }
    tracing::debug!("thread de áudio terminada");
}

pub(crate) struct Session {
    stream: cpal::Stream,
    consumer: rtrb::Consumer<f32>,
    samples: Vec<f32>,
    device: String,
    rate: u32,
    channels: u16,
    pressed_at: Instant,
    start_cue: bool,
    first_callback: Arc<OnceLock<Instant>>,
    dropped: Arc<AtomicUsize>,
    /// Ligada pelo callback de erro do stream (que não é um soluço). O medidor
    /// vê-a para largar o microfone e avisar a janela.
    failed: Arc<AtomicBool>,
}

impl Session {
    /// Abre o microfone `wanted` (pelo nome) ou, sem escolha, o predefinido. Se o
    /// escolhido já não existir ou não abrir, volto ao predefinido: um ditado
    /// nunca falha só porque desligaste o headset que estava escolhido.
    pub(crate) fn open(
        pressed_at: Instant,
        start_cue: bool,
        events: Option<UnboundedSender<DictationEvent>>,
        wanted: Option<&str>,
    ) -> Result<Self, AudioError> {
        // Pedimos o dispositivo no momento (e não no arranque): se trocares de
        // headset, a próxima gravação já usa o novo.
        let host = cpal::default_host();
        if let Some(name) = wanted {
            match find_input(&host, name) {
                Some(device) => {
                    match Self::open_device(&device, pressed_at, start_cue, events.clone()) {
                        Ok(s) => return Ok(s),
                        Err(e) => {
                            tracing::warn!(error = %e, device = name, "o microfone escolhido não abriu; a usar o predefinido")
                        }
                    }
                }
                None => tracing::warn!(
                    device = name,
                    "o microfone escolhido não existe; a usar o predefinido"
                ),
            }
        }
        let device = host.default_input_device().ok_or(AudioError::NoDevice)?;
        Self::open_device(&device, pressed_at, start_cue, events)
    }

    pub(crate) fn open_device(
        device: &cpal::Device,
        pressed_at: Instant,
        start_cue: bool,
        events: Option<UnboundedSender<DictationEvent>>,
    ) -> Result<Self, AudioError> {
        let supported = device.default_input_config()?;
        Self::open_configured(device, supported, pressed_at, start_cue, events)
    }

    /// O áudio que o sistema está a tocar (colunas ou auscultadores). No Windows, o
    /// WASAPI trata um dispositivo de *saída* aberto como entrada como loopback; no
    /// macOS (14.6+) o cpal cria uma "tap" do Core Audio. Em ambos o formato é o da
    /// saída, daí `default_output_config`.
    pub(crate) fn open_loopback(device: &cpal::Device) -> Result<Self, AudioError> {
        let supported = device.default_output_config()?;
        Self::open_configured(device, supported, Instant::now(), false, None)
    }

    fn open_configured(
        device: &cpal::Device,
        supported: cpal::SupportedStreamConfig,
        pressed_at: Instant,
        start_cue: bool,
        events: Option<UnboundedSender<DictationEvent>>,
    ) -> Result<Self, AudioError> {
        let name = device_name(device).unwrap_or_else(|| "desconhecido".to_owned());
        let format = supported.sample_format();
        let config = supported.config();
        let rate = config.sample_rate;
        let channels = config.channels;

        let (producer, consumer) = rtrb::RingBuffer::new(rate as usize * RING_SECONDS);
        // `Arc` porque o callback (outra thread) e a sessão precisam de ver o mesmo
        // valor; `OnceLock`/`AtomicUsize` porque permitem escrever sem `Mutex`.
        let first_callback = Arc::new(OnceLock::new());
        let dropped = Arc::new(AtomicUsize::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let shared = Shared {
            producer,
            first_callback: first_callback.clone(),
            dropped: dropped.clone(),
            failed: failed.clone(),
            events,
        };

        // O formato só é conhecido em tempo de execução, mas o callback é genérico em
        // `T`: o `match` escolhe qual versão monomorfizada do callback compilar e usar.
        let stream = match format {
            SampleFormat::F32 => build::<f32>(device, config, shared),
            SampleFormat::I16 => build::<i16>(device, config, shared),
            SampleFormat::I32 => build::<i32>(device, config, shared),
            SampleFormat::U16 => build::<u16>(device, config, shared),
            SampleFormat::F64 => build::<f64>(device, config, shared),
            other => return Err(AudioError::UnsupportedFormat(other)),
        }?;
        stream.play()?;
        tracing::debug!(device = %name, rate, channels, ?format, "microfone aberto");

        Ok(Self {
            stream,
            consumer,
            samples: Vec::with_capacity(rate as usize * INITIAL_SECONDS),
            device: name,
            rate,
            channels,
            pressed_at,
            start_cue,
            first_callback,
            dropped,
            failed,
        })
    }

    /// Move o que estiver no ring buffer para `samples`. O ring buffer é circular,
    /// por isso os dados podem vir em duas fatias (fim e início do buffer).
    /// Devolve o índice onde começam as amostras novas.
    fn drain(&mut self) -> usize {
        let start = self.samples.len();
        let n = self.consumer.slots();
        if n == 0 {
            return start;
        }
        if let Ok(chunk) = self.consumer.read_chunk(n) {
            let (a, b) = chunk.as_slices();
            self.samples.extend_from_slice(a);
            self.samples.extend_from_slice(b);
            chunk.commit_all();
        }
        start
    }

    /// Entrega ao chamador o que chegou desde a última vez, sem acumular nada aqui:
    /// o notetaker corre durante uma hora e não pode guardar o áudio todo na RAM.
    pub(crate) fn drain_into(&mut self, out: &mut Vec<f32>) {
        let n = self.consumer.slots();
        if n == 0 {
            return;
        }
        if let Ok(chunk) = self.consumer.read_chunk(n) {
            let (a, b) = chunk.as_slices();
            out.extend_from_slice(a);
            out.extend_from_slice(b);
            chunk.commit_all();
        }
    }

    pub(crate) fn rate(&self) -> u32 {
        self.rate
    }

    pub(crate) fn device(&self) -> &str {
        &self.device
    }

    /// O stream morreu (dispositivo desligado): é preciso reabri-lo.
    pub(crate) fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    pub(crate) fn dropped(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Consome a sessão (`self` por valor): depois de terminar já não pode ser usada,
    /// e é o compilador que o garante.
    fn finish(mut self, to16k: &mut To16k, stop_at: Instant) -> Result<Recording, AudioError> {
        // Pausar pára os callbacks; o que já está no ring buffer não se perde.
        if let Err(e) = self.stream.pause() {
            tracing::warn!(error = %e, "não foi possível pausar o stream");
        }
        self.drain();

        let mono16k = to16k.convert(&self.samples, self.rate)?;
        let voice = vad::detect(&mono16k, TARGET_RATE, self.start_cue);
        let kept = voice.speech.clone().unwrap_or(0..mono16k.len());
        let duration =
            |samples: usize| Duration::from_secs_f64(samples as f64 / f64::from(TARGET_RATE));
        let audio = duration(kept.len());
        let trimmed = duration(mono16k.len() - kept.len());
        // `&mono16k[kept]` é uma fatia: corta sem copiar as amostras.
        let wav = wav::encode_pcm16_mono(&mono16k[kept], TARGET_RATE);

        Ok(Recording {
            wav,
            audio,
            trimmed,
            voice,
            device: self.device,
            device_rate: self.rate,
            device_channels: self.channels,
            mic_start: self
                .first_callback
                .get()
                .map(|t| t.saturating_duration_since(self.pressed_at)),
            finalize: stop_at.elapsed(),
            dropped_samples: self.dropped.load(Ordering::Relaxed),
        })
        // Aqui `self.stream` sai de âmbito e o microfone é fechado.
    }
}

/// O nome que o sistema dá ao microfone ("Microfone (Realtek Audio)").
pub(crate) fn device_name(device: &cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_owned())
}

fn find_input(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    host.input_devices()
        .ok()?
        .find(|d| device_name(d).as_deref() == Some(name))
}

/// Um microfone, para o seletor das definições e do guia.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InputDevice {
    pub name: String,
    pub is_default: bool,
}

/// Bloqueante (o sistema pode demorar dezenas de ms a enumerar). O predefinido
/// vem primeiro; nomes repetidos aparecem uma só vez (escolhemos pelo nome).
pub fn input_devices() -> Result<Vec<InputDevice>, AudioError> {
    let host = cpal::default_host();
    let default = host.default_input_device().as_ref().and_then(device_name);
    let mut list: Vec<InputDevice> = Vec::new();
    for device in host.input_devices()? {
        let Some(name) = device_name(&device) else {
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
    // `sort_by_key` é estável: o resto fica pela ordem do sistema.
    list.sort_by_key(|d| !d.is_default);
    Ok(list)
}

/// O que o callback leva consigo para a thread de áudio.
struct Shared {
    producer: rtrb::Producer<f32>,
    first_callback: Arc<OnceLock<Instant>>,
    dropped: Arc<AtomicUsize>,
    failed: Arc<AtomicBool>,
    /// `None` no medidor: um erro dele não cancela um ditado.
    events: Option<UnboundedSender<DictationEvent>>,
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    shared: Shared,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let Shared {
        mut producer,
        first_callback,
        dropped,
        failed,
        events,
    } = shared;

    device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _info| {
            if first_callback.get().is_none() {
                let _ = first_callback.set(Instant::now());
            }
            resample::downmix(data, channels, f32::from_sample_, |s| {
                if producer.push(s).is_err() {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            });
        },
        move |err| {
            match err.kind() {
                // Um "xrun" pontual é comum no arranque do stream (sobretudo com
                // dispositivos virtuais) e não perde o que já está no ring buffer.
                cpal::ErrorKind::Xrun => tracing::warn!("soluço no stream de áudio (xrun)"),
                cpal::ErrorKind::DeviceNotAvailable => {
                    tracing::error!(error = %err, "microfone indisponível");
                    if let Some(events) = &events {
                        let _ = events.send(DictationEvent::AudioFailed(
                            "o microfone foi desligado".into(),
                        ));
                    }
                }
                _ => tracing::error!(error = %err, "erro no stream de áudio"),
            }
            if !matches!(err.kind(), cpal::ErrorKind::Xrun) {
                failed.store(true, Ordering::Relaxed);
            }
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_access_denied_from_wasapi() {
        assert!(is_access_denied("Access is denied. (os error -2147024891)"));
        assert!(is_access_denied("Acesso negado. (os error -2147024891)"));
        assert!(is_access_denied("HRESULT 0x80070005"));
        assert!(!is_access_denied("The device is in use"));
    }
}
