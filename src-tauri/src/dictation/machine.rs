//! Máquina de estados do atalho "segurar + alternar", sem I/O.
//!
//! É uma função pura de (estado, entrada, instante) → ação, o que permite testá-la
//! com instantes sintéticos sem teclado, áudio nem rede. O orquestrador
//! (`dictation::run`) é quem executa as ações.
//!
//! Há dois atalhos (ditado e comando) mas uma só máquina: há um só microfone e
//! uma só pílula, por isso os dois nunca correm ao mesmo tempo. O modo chega com
//! cada premir/largar e fica guardado enquanto a gravação dura.

use std::time::{Duration, Instant};

/// Abaixo deste tempo, um toque no atalho é interpretado como "alternar" (fica a
/// gravar depois de largar); acima, é push-to-talk (largar termina a gravação).
pub const TOGGLE_THRESHOLD: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    /// A gravar com a tecla em baixo desde `since`.
    Holding { since: Instant },
    /// A gravar em modo alternar: a tecla já foi largada e o próximo premir termina.
    Latched,
    /// À espera da transcrição/colagem. Um novo premir não começa outro ditado
    /// (só dá o aviso "ocupado"); cancelar aborta o processamento.
    Processing,
}

/// O que fazer com a gravação: colar o que disseste (ditado) ou tratar o que
/// disseste como uma instrução sobre o texto selecionado (comando).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Dictation,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// O atalho deste modo foi premido.
    Pressed(Mode),
    Released(Mode),
    Cancel,
    /// Clique na barra flutuante: começa em modo alternar ou termina-o.
    Click,
    /// "Tentar de novo" a transcrição da última gravação falhada. Quem envia isto
    /// já confirmou que há uma gravação guardada.
    Retry,
    ProcessingFinished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Começar a gravar neste modo.
    StartRecording(Mode),
    StopAndProcess,
    CancelRecording,
    /// Abortar a transcrição/colagem em curso, sem colar nada.
    CancelProcessing,
    /// Voltar a enviar a última gravação falhada.
    Retry,
    /// Premiste o atalho a meio do processamento: avisar que ainda está ocupado.
    Busy,
}

#[derive(Debug, Default)]
pub struct Machine {
    phase: Phase,
    /// O modo da gravação em curso (ou da última). Só o atalho deste modo a
    /// termina; o do outro modo é ignorado enquanto se grava.
    mode: Mode,
    /// Cada atalho está em baixo? Serve para distinguir um premir novo da
    /// auto-repetição do teclado (que envia `Pressed` repetidos sem `Released`).
    /// Um por modo: `[ditado, comando]` (ver `slot`).
    key_down: [bool; 2],
}

/// A posição de cada modo em `Machine::key_down`.
fn slot(mode: Mode) -> usize {
    match mode {
        Mode::Dictation => 0,
        Mode::Command => 1,
    }
}

impl Machine {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Aplica uma entrada e devolve a ação a executar, se houver.
    ///
    /// O `match` sobre o par (estado, entrada) obriga o compilador a confirmar que
    /// pensámos em todas as combinações: os casos "ignorar" estão explícitos no `_`.
    pub fn handle(&mut self, input: Input, now: Instant) -> Option<Action> {
        use {Input::*, Phase::*};
        // A tecla deste atalho já estava em baixo antes deste evento?
        let was_down = match input {
            // Um padrão com `|` liga `m` nos dois casos: o mesmo código serve
            // para premir e para largar.
            Pressed(m) | Released(m) => {
                let was = self.key_down[slot(m)];
                self.key_down[slot(m)] = matches!(input, Pressed(_));
                was
            }
            _ => false,
        };
        // É o atalho do modo da gravação em curso?
        let current = self.mode;
        let same = |m: Mode| m == current;
        let (next, action) = match (self.phase, input) {
            (Idle, Pressed(m)) => {
                self.mode = m;
                (Holding { since: now }, Some(Action::StartRecording(m)))
            }
            (Holding { since }, Released(m)) if same(m) => {
                if now.saturating_duration_since(since) >= TOGGLE_THRESHOLD {
                    (Processing, Some(Action::StopAndProcess))
                } else {
                    (Latched, None)
                }
            }
            (Latched, Pressed(m)) if same(m) => (Processing, Some(Action::StopAndProcess)),
            // Com o rato não há "segurar": um clique começa logo em modo alternar.
            // A pílula só dita; o comando tem o seu atalho.
            (Idle, Click) => {
                self.mode = Mode::Dictation;
                (Latched, Some(Action::StartRecording(Mode::Dictation)))
            }
            (Latched, Click) => (Processing, Some(Action::StopAndProcess)),
            (Holding { .. } | Latched, Cancel) => (Idle, Some(Action::CancelRecording)),
            (Processing, Cancel) => (Idle, Some(Action::CancelProcessing)),
            // Só os ditados oferecem "Tentar de novo" (ver `pipeline::command`).
            (Idle, Retry) => {
                self.mode = Mode::Dictation;
                (Processing, Some(Action::Retry))
            }
            // Só um premir novo: se a tecla que parou o modo alternar ainda está
            // em baixo, a auto-repetição não conta. Vale para os dois atalhos.
            (Processing, Pressed(_)) if !was_down => (Processing, Some(Action::Busy)),
            (Processing, Retry) => (Processing, Some(Action::Busy)),
            (Processing, ProcessingFinished) => (Idle, None),
            // Auto-repetição do teclado (Pressed em Holding ou a processar), o
            // atalho do outro modo a meio de uma gravação, Released depois de
            // parar, clicar durante o processamento, clicar ou tentar de novo a
            // gravar, etc.: não mudam nada.
            (phase, _) => (phase, None),
        };
        self.phase = next;
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: Mode = Mode::Dictation;
    const C: Mode = Mode::Command;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn hold_then_release_stops_and_processes() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        assert_eq!(
            m.handle(Input::Pressed(D), t0),
            Some(Action::StartRecording(D))
        );
        assert_eq!(
            m.handle(Input::Released(D), t0 + ms(1500)),
            Some(Action::StopAndProcess)
        );
        assert_eq!(m.phase(), Phase::Processing);
        assert_eq!(m.handle(Input::ProcessingFinished, t0 + ms(2000)), None);
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn quick_tap_latches_and_next_press_stops() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.handle(Input::Pressed(D), t0);
        assert_eq!(m.handle(Input::Released(D), t0 + ms(120)), None);
        assert_eq!(m.phase(), Phase::Latched);
        assert_eq!(
            m.handle(Input::Pressed(D), t0 + ms(5000)),
            Some(Action::StopAndProcess)
        );
        // O Released que vem a seguir ao premir que parou é ignorado.
        assert_eq!(m.handle(Input::Released(D), t0 + ms(5100)), None);
        assert_eq!(m.phase(), Phase::Processing);
    }

    #[test]
    fn threshold_boundary_counts_as_hold() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.handle(Input::Pressed(D), t0);
        assert_eq!(
            m.handle(Input::Released(D), t0 + TOGGLE_THRESHOLD),
            Some(Action::StopAndProcess)
        );
    }

    #[test]
    fn key_repeat_while_holding_is_ignored() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.handle(Input::Pressed(D), t0);
        for i in 1..10 {
            assert_eq!(m.handle(Input::Pressed(D), t0 + ms(i * 30)), None);
        }
        // A repetição não reinicia o relógio: continua a contar desde t0.
        assert_eq!(m.phase(), Phase::Holding { since: t0 });
        assert_eq!(
            m.handle(Input::Released(D), t0 + ms(400)),
            Some(Action::StopAndProcess)
        );
    }

    #[test]
    fn cancel_while_recording_returns_to_idle() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.handle(Input::Pressed(D), t0);
        assert_eq!(
            m.handle(Input::Cancel, t0 + ms(50)),
            Some(Action::CancelRecording)
        );
        assert_eq!(m.phase(), Phase::Idle);

        m.handle(Input::Pressed(D), t0 + ms(100));
        m.handle(Input::Released(D), t0 + ms(150));
        assert_eq!(
            m.handle(Input::Cancel, t0 + ms(900)),
            Some(Action::CancelRecording)
        );
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn stray_events_are_ignored() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        assert_eq!(m.handle(Input::Released(D), t0), None);
        assert_eq!(m.handle(Input::Cancel, t0), None);
        assert_eq!(m.handle(Input::ProcessingFinished, t0), None);
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn pressing_during_processing_says_busy_once_per_press() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.handle(Input::Pressed(D), t0);
        m.handle(Input::Released(D), t0 + ms(500));
        assert_eq!(m.phase(), Phase::Processing);
        assert_eq!(
            m.handle(Input::Pressed(D), t0 + ms(600)),
            Some(Action::Busy)
        );
        // A auto-repetição de segurar a tecla não repete o aviso.
        assert_eq!(m.handle(Input::Pressed(D), t0 + ms(650)), None);
        assert_eq!(m.handle(Input::Released(D), t0 + ms(700)), None);
        assert_eq!(
            m.handle(Input::Pressed(D), t0 + ms(800)),
            Some(Action::Busy)
        );
        // Continua a processar: o premir não começou outro ditado.
        assert_eq!(m.phase(), Phase::Processing);
    }

    #[test]
    fn key_still_down_after_stopping_latched_is_not_busy() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.handle(Input::Pressed(D), t0);
        m.handle(Input::Released(D), t0 + ms(100));
        // O premir que termina o modo alternar fica em baixo uns instantes.
        assert_eq!(
            m.handle(Input::Pressed(D), t0 + ms(3000)),
            Some(Action::StopAndProcess)
        );
        assert_eq!(m.handle(Input::Pressed(D), t0 + ms(3500)), None);
        assert_eq!(m.handle(Input::Pressed(D), t0 + ms(3530)), None);
        assert_eq!(m.handle(Input::Released(D), t0 + ms(3600)), None);
        assert_eq!(
            m.handle(Input::Pressed(D), t0 + ms(4000)),
            Some(Action::Busy)
        );
    }

    #[test]
    fn cancel_while_processing_aborts() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.handle(Input::Pressed(D), t0);
        m.handle(Input::Released(D), t0 + ms(500));
        assert_eq!(
            m.handle(Input::Cancel, t0 + ms(900)),
            Some(Action::CancelProcessing)
        );
        assert_eq!(m.phase(), Phase::Idle);
        // O fim do processamento abortado, se ainda chegar, não muda nada.
        assert_eq!(m.handle(Input::ProcessingFinished, t0 + ms(950)), None);
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn retry_only_from_idle() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        assert_eq!(m.handle(Input::Retry, t0), Some(Action::Retry));
        assert_eq!(m.phase(), Phase::Processing);
        // Pedir outra vez enquanto tenta: "ocupado".
        assert_eq!(m.handle(Input::Retry, t0 + ms(100)), Some(Action::Busy));
        // Pode ser cancelado como um ditado normal.
        assert_eq!(
            m.handle(Input::Cancel, t0 + ms(200)),
            Some(Action::CancelProcessing)
        );
        // A gravar, o "tentar de novo" é ignorado.
        m.handle(Input::Pressed(D), t0 + ms(300));
        assert_eq!(m.handle(Input::Retry, t0 + ms(400)), None);
        assert!(matches!(m.phase(), Phase::Holding { .. }));
    }

    #[test]
    fn click_starts_latched_and_next_click_stops() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        assert_eq!(m.handle(Input::Click, t0), Some(Action::StartRecording(D)));
        assert_eq!(m.phase(), Phase::Latched);
        assert_eq!(
            m.handle(Input::Click, t0 + ms(3000)),
            Some(Action::StopAndProcess)
        );
        assert_eq!(m.phase(), Phase::Processing);
    }

    #[test]
    fn click_and_shortcut_mix() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        // Começa com um clique, termina com o atalho.
        m.handle(Input::Click, t0);
        assert_eq!(
            m.handle(Input::Pressed(D), t0 + ms(2000)),
            Some(Action::StopAndProcess)
        );
        m.handle(Input::ProcessingFinished, t0 + ms(2500));
        // Clicar com a tecla em baixo não interrompe o push-to-talk.
        m.handle(Input::Pressed(D), t0 + ms(3000));
        assert_eq!(m.handle(Input::Click, t0 + ms(3100)), None);
        assert!(matches!(m.phase(), Phase::Holding { .. }));
        // Clicar durante o processamento é ignorado.
        m.handle(Input::Released(D), t0 + ms(4000));
        assert_eq!(m.handle(Input::Click, t0 + ms(4100)), None);
        assert_eq!(m.phase(), Phase::Processing);
    }

    #[test]
    fn cancel_after_click_returns_to_idle() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.handle(Input::Click, t0);
        assert_eq!(
            m.handle(Input::Cancel, t0 + ms(500)),
            Some(Action::CancelRecording)
        );
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn command_shortcut_starts_in_command_mode() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        assert_eq!(
            m.handle(Input::Pressed(C), t0),
            Some(Action::StartRecording(C))
        );
        assert_eq!(m.mode(), C);
        assert_eq!(
            m.handle(Input::Released(C), t0 + ms(1000)),
            Some(Action::StopAndProcess)
        );
        assert_eq!(m.mode(), C, "o modo fica até ao fim do processamento");
        m.handle(Input::ProcessingFinished, t0 + ms(2000));
        // O ditado seguinte volta ao modo ditado.
        assert_eq!(
            m.handle(Input::Pressed(D), t0 + ms(3000)),
            Some(Action::StartRecording(D))
        );
        assert_eq!(m.mode(), D);
    }

    #[test]
    fn the_other_shortcut_is_ignored_while_recording() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.handle(Input::Pressed(C), t0);
        // Premir ou largar o atalho de ditado não termina um comando.
        assert_eq!(m.handle(Input::Pressed(D), t0 + ms(50)), None);
        assert_eq!(m.handle(Input::Released(D), t0 + ms(60)), None);
        assert!(matches!(m.phase(), Phase::Holding { .. }));
        // Toque rápido: modo alternar; só o atalho de comando o termina.
        assert_eq!(m.handle(Input::Released(C), t0 + ms(100)), None);
        assert_eq!(m.phase(), Phase::Latched);
        assert_eq!(m.handle(Input::Pressed(D), t0 + ms(300)), None);
        assert_eq!(m.handle(Input::Released(D), t0 + ms(350)), None);
        assert_eq!(m.phase(), Phase::Latched);
        assert_eq!(
            m.handle(Input::Pressed(C), t0 + ms(2000)),
            Some(Action::StopAndProcess)
        );
    }

    #[test]
    fn either_shortcut_says_busy_while_processing() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.handle(Input::Pressed(D), t0);
        m.handle(Input::Released(D), t0 + ms(500));
        assert_eq!(
            m.handle(Input::Pressed(C), t0 + ms(600)),
            Some(Action::Busy)
        );
        // Auto-repetição do atalho de comando: um só aviso.
        assert_eq!(m.handle(Input::Pressed(C), t0 + ms(650)), None);
        assert_eq!(
            m.handle(Input::Pressed(D), t0 + ms(700)),
            Some(Action::Busy)
        );
        assert_eq!(m.phase(), Phase::Processing);
    }

    #[test]
    fn click_and_retry_are_always_dictation() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.handle(Input::Pressed(C), t0);
        m.handle(Input::Cancel, t0 + ms(100));
        assert_eq!(
            m.handle(Input::Click, t0 + ms(200)),
            Some(Action::StartRecording(D))
        );
        assert_eq!(m.mode(), D);
        m.handle(Input::Cancel, t0 + ms(300));
        m.handle(Input::Pressed(C), t0 + ms(400));
        m.handle(Input::Cancel, t0 + ms(500));
        assert_eq!(m.handle(Input::Retry, t0 + ms(600)), Some(Action::Retry));
        assert_eq!(m.mode(), D);
    }
}
