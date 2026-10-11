//! Máquina de estados do atalho "segurar + alternar", sem I/O.
//!
//! É uma função pura de (estado, entrada, instante) → ação, o que permite testá-la
//! com instantes sintéticos sem teclado, áudio nem rede. O orquestrador
//! (`dictation::run`) é quem executa as ações.

use std::time::{Duration, Instant};

/// Abaixo deste tempo, um toque no atalho é interpretado como "alternar" (fica a
/// gravar depois de largar); acima, é push-to-talk (largar termina a gravação).
pub const TOGGLE_THRESHOLD: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// A gravar com a tecla em baixo desde `since`.
    Holding {
        since: Instant,
    },
    /// A gravar em modo alternar: a tecla já foi largada e o próximo premir termina.
    Latched,
    /// À espera da transcrição/colagem. Novos premires são ignorados.
    Processing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Pressed,
    Released,
    Cancel,
    /// Clique na barra flutuante: começa em modo alternar ou termina-o.
    Click,
    ProcessingFinished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    StartRecording,
    StopAndProcess,
    CancelRecording,
}

#[derive(Debug)]
pub struct Machine {
    phase: Phase,
}

impl Default for Machine {
    fn default() -> Self {
        Self { phase: Phase::Idle }
    }
}

impl Machine {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Aplica uma entrada e devolve a ação a executar, se houver.
    ///
    /// O `match` sobre o par (estado, entrada) obriga o compilador a confirmar que
    /// pensámos em todas as combinações: os casos "ignorar" estão explícitos no `_`.
    pub fn handle(&mut self, input: Input, now: Instant) -> Option<Action> {
        use {Input::*, Phase::*};
        let (next, action) = match (self.phase, input) {
            (Idle, Pressed) => (Holding { since: now }, Some(Action::StartRecording)),
            (Holding { since }, Released) => {
                if now.saturating_duration_since(since) >= TOGGLE_THRESHOLD {
                    (Processing, Some(Action::StopAndProcess))
                } else {
                    (Latched, None)
                }
            }
            (Latched, Pressed) => (Processing, Some(Action::StopAndProcess)),
            // Com o rato não há "segurar": um clique começa logo em modo alternar.
            (Idle, Click) => (Latched, Some(Action::StartRecording)),
            (Latched, Click) => (Processing, Some(Action::StopAndProcess)),
            (Holding { .. } | Latched, Cancel) => (Idle, Some(Action::CancelRecording)),
            (Processing, ProcessingFinished) => (Idle, None),
            // Auto-repetição do teclado (Pressed em Holding), Released depois de
            // parar, premir ou clicar durante o processamento, clicar com a
            // tecla em baixo, etc.: não mudam nada.
            (phase, _) => (phase, None),
        };
        self.phase = next;
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn hold_then_release_stops_and_processes() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        assert_eq!(m.handle(Input::Pressed, t0), Some(Action::StartRecording));
        assert_eq!(
            m.handle(Input::Released, t0 + ms(1500)),
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
        m.handle(Input::Pressed, t0);
        assert_eq!(m.handle(Input::Released, t0 + ms(120)), None);
        assert_eq!(m.phase(), Phase::Latched);
        assert_eq!(
            m.handle(Input::Pressed, t0 + ms(5000)),
            Some(Action::StopAndProcess)
        );
        // O Released que vem a seguir ao premir que parou é ignorado.
        assert_eq!(m.handle(Input::Released, t0 + ms(5100)), None);
        assert_eq!(m.phase(), Phase::Processing);
    }

    #[test]
    fn threshold_boundary_counts_as_hold() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.handle(Input::Pressed, t0);
        assert_eq!(
            m.handle(Input::Released, t0 + TOGGLE_THRESHOLD),
            Some(Action::StopAndProcess)
        );
    }

    #[test]
    fn key_repeat_while_holding_is_ignored() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.handle(Input::Pressed, t0);
        for i in 1..10 {
            assert_eq!(m.handle(Input::Pressed, t0 + ms(i * 30)), None);
        }
        // A repetição não reinicia o relógio: continua a contar desde t0.
        assert_eq!(m.phase(), Phase::Holding { since: t0 });
        assert_eq!(
            m.handle(Input::Released, t0 + ms(400)),
            Some(Action::StopAndProcess)
        );
    }

    #[test]
    fn cancel_while_recording_returns_to_idle() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.handle(Input::Pressed, t0);
        assert_eq!(
            m.handle(Input::Cancel, t0 + ms(50)),
            Some(Action::CancelRecording)
        );
        assert_eq!(m.phase(), Phase::Idle);

        m.handle(Input::Pressed, t0 + ms(100));
        m.handle(Input::Released, t0 + ms(150));
        assert_eq!(
            m.handle(Input::Cancel, t0 + ms(900)),
            Some(Action::CancelRecording)
        );
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn presses_during_processing_and_stray_events_are_ignored() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        assert_eq!(m.handle(Input::Released, t0), None);
        assert_eq!(m.handle(Input::Cancel, t0), None);
        assert_eq!(m.handle(Input::ProcessingFinished, t0), None);
        assert_eq!(m.phase(), Phase::Idle);

        m.handle(Input::Pressed, t0);
        m.handle(Input::Released, t0 + ms(500));
        assert_eq!(m.handle(Input::Pressed, t0 + ms(600)), None);
        assert_eq!(m.handle(Input::Cancel, t0 + ms(650)), None);
        assert_eq!(m.phase(), Phase::Processing);
    }

    #[test]
    fn click_starts_latched_and_next_click_stops() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        assert_eq!(m.handle(Input::Click, t0), Some(Action::StartRecording));
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
            m.handle(Input::Pressed, t0 + ms(2000)),
            Some(Action::StopAndProcess)
        );
        m.handle(Input::ProcessingFinished, t0 + ms(2500));
        // Clicar com a tecla em baixo não interrompe o push-to-talk.
        m.handle(Input::Pressed, t0 + ms(3000));
        assert_eq!(m.handle(Input::Click, t0 + ms(3100)), None);
        assert!(matches!(m.phase(), Phase::Holding { .. }));
        // Clicar durante o processamento é ignorado.
        m.handle(Input::Released, t0 + ms(4000));
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
}
