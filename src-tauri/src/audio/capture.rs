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
        atomic::{AtomicUsize, Ordering},
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

enum Command {
    Start {
        pressed_at: Instant,
        /// O som de início vai tocar: a deteção de voz ignora-o (ver `vad.rs`).
        start_cue: bool,
    },
    Stop {
        reply: oneshot::Sender<Result<Recording, AudioError>>,
    },
    Cancel,
}

pub struct Recorder {
    tx: mpsc::Sender<Command>,
}

/// Recebe o nível de áudio (0–1) a cada esvaziamento do ring buffer (~20 vezes por
/// segundo) enquanto se grava. `Box<dyn Fn>`: o módulo de áudio não precisa de
/// saber quem está a ouvir (a barra no ecrã); só que é uma função que pode ir para
/// outra thread (`Send`).
pub type LevelSink = Box<dyn Fn(f32) + Send>;

impl Recorder {
    /// Lança a thread de áudio. O microfone só é aberto no `start`, não aqui.
    pub fn spawn(
        events: UnboundedSender<DictationEvent>,
        level: LevelSink,
    ) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("ditok-audio".into())
            .spawn(move || audio_thread(rx, events, level))?;
        Ok(Self { tx })
    }

    /// Não espera que o microfone abra: o orquestrador continua livre. Se falhar, a
    /// thread envia `DictationEvent::AudioFailed`.
    pub fn start(&self, pressed_at: Instant, start_cue: bool) {
        let cmd = Command::Start {
            pressed_at,
            start_cue,
        };
        if self.tx.send(cmd).is_err() {
            tracing::error!("thread de áudio indisponível");
        }
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

fn audio_thread(
    rx: mpsc::Receiver<Command>,
    events: UnboundedSender<DictationEvent>,
    level: LevelSink,
) {
    let mut session: Option<Session> = None;
    let mut to16k = To16k::default();

    loop {
        let cmd = match session.as_mut() {
            // Parado: bloqueia até chegar um comando (zero CPU).
            None => match rx.recv() {
                Ok(cmd) => cmd,
                Err(_) => break,
            },
            // A gravar: acorda no próximo comando ou ao fim de DRAIN_EVERY para
            // esvaziar o ring buffer, o que vier primeiro.
            Some(s) => match rx.recv_timeout(DRAIN_EVERY) {
                Ok(cmd) => cmd,
                Err(RecvTimeoutError::Timeout) => {
                    let new_from = s.drain();
                    // Só as amostras que acabaram de chegar: o nível "de agora".
                    level(resample::level(&s.samples[new_from..]));
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            },
        };

        match cmd {
            Command::Start {
                pressed_at,
                start_cue,
            } => {
                if session.is_some() {
                    tracing::warn!("início pedido com uma gravação em curso; ignorado");
                    continue;
                }
                match Session::open(pressed_at, start_cue, events.clone()) {
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
        }
    }
    tracing::debug!("thread de áudio terminada");
}

struct Session {
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
}

impl Session {
    fn open(
        pressed_at: Instant,
        start_cue: bool,
        events: UnboundedSender<DictationEvent>,
    ) -> Result<Self, AudioError> {
        // Pedimos sempre o dispositivo por omissão no momento: se trocares de
        // headset, a próxima gravação já usa o novo.
        let device = cpal::default_host()
            .default_input_device()
            .ok_or(AudioError::NoDevice)?;
        let name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| "desconhecido".to_owned());
        let supported = device.default_input_config()?;
        let format = supported.sample_format();
        let config = supported.config();
        let rate = config.sample_rate;
        let channels = config.channels;

        let (producer, consumer) = rtrb::RingBuffer::new(rate as usize * RING_SECONDS);
        // `Arc` porque o callback (outra thread) e a sessão precisam de ver o mesmo
        // valor; `OnceLock`/`AtomicUsize` porque permitem escrever sem `Mutex`.
        let first_callback = Arc::new(OnceLock::new());
        let dropped = Arc::new(AtomicUsize::new(0));
        let shared = Shared {
            producer,
            first_callback: first_callback.clone(),
            dropped: dropped.clone(),
            events,
        };

        // O formato só é conhecido em tempo de execução, mas o callback é genérico em
        // `T`: o `match` escolhe qual versão monomorfizada do callback compilar e usar.
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, config, shared),
            SampleFormat::I16 => build::<i16>(&device, config, shared),
            SampleFormat::I32 => build::<i32>(&device, config, shared),
            SampleFormat::U16 => build::<u16>(&device, config, shared),
            SampleFormat::F64 => build::<f64>(&device, config, shared),
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

/// O que o callback leva consigo para a thread de áudio.
struct Shared {
    producer: rtrb::Producer<f32>,
    first_callback: Arc<OnceLock<Instant>>,
    dropped: Arc<AtomicUsize>,
    events: UnboundedSender<DictationEvent>,
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
        move |err| match err.kind() {
            // Um "xrun" pontual é comum no arranque do stream (sobretudo com
            // dispositivos virtuais) e não perde o que já está no ring buffer.
            cpal::ErrorKind::Xrun => tracing::warn!("soluço no stream de áudio (xrun)"),
            cpal::ErrorKind::DeviceNotAvailable => {
                tracing::error!(error = %err, "microfone indisponível");
                let _ = events.send(DictationEvent::AudioFailed(
                    "o microfone foi desligado".into(),
                ));
            }
            _ => tracing::error!(error = %err, "erro no stream de áudio"),
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
