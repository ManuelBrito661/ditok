//! Parte um fluxo contínuo de áudio (uma faixa de uma reunião) em enunciados, cortando
//! nas pausas. Não sabe nada de cpal nem de threads: recebe amostras, devolve
//! enunciados, por isso testa-se com sinais sintéticos.
//!
//! Difere de `audio::vad::detect` (que analisa uma gravação inteira já terminada) em
//! duas coisas: decide *à medida que o áudio chega* e nunca deixa um enunciado passar
//! de `MAX_UTTERANCE` (um modelo local de Whisper só aceita ~30 s, e um pedido de uma
//! reunião de 1 h não pode ficar à espera do fim).
//!
//! Trabalha à taxa do dispositivo: só mede a energia de blocos de 20 ms, não precisa de
//! 16 kHz. A conversão faz-se uma vez por enunciado, o que dá áudio mais limpo do que
//! reamostrar em blocos pequenos (cada bloco teria uma emenda).

use std::time::Duration;

use crate::audio::vad::dbfs;

const FRAME_MS: usize = 20;
/// Como no `vad`: o limiar fica `MARGIN_DB` acima do ruído de fundo, entre estes limites.
const MARGIN_DB: f32 = 12.0;
const MIN_THRESHOLD_DB: f32 = -55.0;
const MAX_THRESHOLD_DB: f32 = -35.0;
/// Blocos seguidos com voz para começar um enunciado (4 × 20 ms = 80 ms): um clique
/// de tecla é forte mas curto demais.
const START_FRAMES: usize = 4;
/// Silêncio que fecha um enunciado. Mais curto parte frases a meio de uma respiração;
/// mais longo junta quem fala a seguir.
const HANG: Duration = Duration::from_millis(800);
/// Margem de áudio mantida antes do início e depois do fim da fala.
const PAD_BEFORE: Duration = Duration::from_millis(300);
const PAD_AFTER: Duration = Duration::from_millis(400);
/// Tamanho máximo de um enunciado. Ao chegar aqui corta-se na pausa mais funda dos
/// últimos `CUT_SEARCH`.
pub const MAX_UTTERANCE: Duration = Duration::from_secs(45);
const CUT_SEARCH: Duration = Duration::from_secs(10);
/// Quão depressa o ruído de fundo estimado sobe quando o som sobe (por bloco). Desce
/// logo, sobe devagar (~20 s): assim a fala, que é mais forte, não o arrasta consigo.
const FLOOR_RISE: f32 = 0.001;

/// Um enunciado cortado: o áudio à taxa do dispositivo e onde começa na faixa.
#[derive(Debug, Clone, PartialEq)]
pub struct Cut {
    /// Primeira amostra, contada desde o início da faixa.
    pub start: u64,
    pub samples: Vec<f32>,
}

pub struct Segmenter {
    frame: usize,
    /// Amostras de um bloco incompleto, à espera das restantes.
    partial: Vec<f32>,
    /// Áudio desde o início do enunciado em curso (com a margem anterior) ou, parado,
    /// só a margem anterior.
    buf: Vec<f32>,
    /// Posição absoluta de `buf[0]`.
    buf_start: u64,
    speaking: bool,
    voiced_run: usize,
    /// Blocos sem voz desde a última fala (só enquanto `speaking`).
    silent_run: usize,
    /// Até onde vai a última fala dentro de `buf` (índice de amostra).
    last_voiced_end: usize,
    floor_db: Option<f32>,
}

impl Segmenter {
    pub fn new(rate: u32) -> Self {
        Self {
            frame: (rate as usize * FRAME_MS / 1000).max(1),
            partial: Vec::new(),
            buf: Vec::new(),
            buf_start: 0,
            speaking: false,
            voiced_run: 0,
            silent_run: 0,
            last_voiced_end: 0,
            floor_db: None,
        }
    }

    fn frames_in(&self, d: Duration) -> usize {
        (d.as_millis() as usize / FRAME_MS).max(1)
    }

    /// Acrescenta áudio e entrega em `out` os enunciados que ficaram completos.
    pub fn push(&mut self, mut samples: &[f32], out: &mut Vec<Cut>) {
        // Completa primeiro o bloco que ficou a meio da chamada anterior.
        if !self.partial.is_empty() {
            let need = self.frame - self.partial.len();
            let take = need.min(samples.len());
            self.partial.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.partial.len() < self.frame {
                return;
            }
            let block = std::mem::take(&mut self.partial);
            self.frame_in(&block, out);
        }
        let mut blocks = samples.chunks_exact(self.frame);
        for block in &mut blocks {
            self.frame_in(block, out);
        }
        self.partial.extend_from_slice(blocks.remainder());
    }

    /// Fim da faixa: o que estiver a meio é um enunciado também.
    pub fn finish(&mut self, out: &mut Vec<Cut>) {
        self.buf.append(&mut self.partial);
        if self.speaking && self.last_voiced_end > 0 {
            let end = (self.last_voiced_end + self.pad_after()).min(self.buf.len());
            self.emit(end, out);
        }
        self.buf.clear();
        self.speaking = false;
    }

    fn pad_after(&self) -> usize {
        self.frames_in(PAD_AFTER) * self.frame
    }

    fn frame_in(&mut self, block: &[f32], out: &mut Vec<Cut>) {
        let db = dbfs(block);
        // Ruído de fundo: o mínimo recente, que sobe muito devagar.
        let floor = match self.floor_db {
            None => db,
            Some(f) if db < f => db,
            Some(f) => f + (db - f) * FLOOR_RISE,
        };
        self.floor_db = Some(floor);
        let threshold = (floor + MARGIN_DB).clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB);
        let voiced = db >= threshold;

        self.buf.extend_from_slice(block);

        if !self.speaking {
            self.voiced_run = if voiced { self.voiced_run + 1 } else { 0 };
            // Mantém só a margem anterior mais os blocos que podem vir a ser o início.
            let keep = (self.frames_in(PAD_BEFORE) + START_FRAMES) * self.frame;
            if self.voiced_run >= START_FRAMES {
                self.speaking = true;
                self.silent_run = 0;
                self.last_voiced_end = self.buf.len();
                // O enunciado começa na margem antes da primeira fala.
                let lead = (self.frames_in(PAD_BEFORE) + self.voiced_run) * self.frame;
                self.trim_front(self.buf.len().saturating_sub(lead));
            } else {
                self.trim_front(self.buf.len().saturating_sub(keep));
            }
            return;
        }

        if voiced {
            self.silent_run = 0;
            self.last_voiced_end = self.buf.len();
        } else {
            self.silent_run += 1;
        }

        if self.silent_run >= self.frames_in(HANG) {
            let end = (self.last_voiced_end + self.pad_after()).min(self.buf.len());
            self.emit(end, out);
            // O que sobra é silêncio: o ramo "parado" aparará para a margem anterior.
            self.speaking = false;
            self.voiced_run = 0;
        } else if self.buf.len() >= self.max_len() {
            self.cut_at_quietest(out);
        }
    }

    fn max_len(&self) -> usize {
        self.frames_in(MAX_UTTERANCE) * self.frame
    }

    /// Descarta as primeiras `n` amostras de `buf`.
    fn trim_front(&mut self, n: usize) {
        if n > 0 {
            self.buf.drain(..n);
            self.buf_start += n as u64;
            self.last_voiced_end = self.last_voiced_end.saturating_sub(n);
        }
    }

    /// O enunciado chegou ao máximo sem pausa: corta no trecho mais silencioso dos
    /// últimos `CUT_SEARCH` e o resto passa a ser o início do próximo.
    fn cut_at_quietest(&mut self, out: &mut Vec<Cut>) {
        let frame = self.frame;
        let from = self
            .buf
            .len()
            .saturating_sub(self.frames_in(CUT_SEARCH) * frame);
        // Janela de 3 blocos (60 ms): uma pausa entre palavras, não um bloco solto.
        let window = 3 * frame;
        let mut best = (f32::INFINITY, self.buf.len() - window.min(self.buf.len()));
        let mut at = from - from % frame;
        while at + window <= self.buf.len() {
            let e = dbfs(&self.buf[at..at + window]);
            if e < best.0 {
                best = (e, at);
            }
            at += frame;
        }
        let cut = best.1 + window / 2;
        self.emit(cut, out);
        // `emit` já tirou o enunciado de `buf`: o que resta continua a fala.
        self.last_voiced_end = self.buf.len();
        self.silent_run = 0;
    }

    /// Tira `buf[..end]` para `out`.
    fn emit(&mut self, end: usize, out: &mut Vec<Cut>) {
        if end == 0 {
            return;
        }
        let samples: Vec<f32> = self.buf.drain(..end).collect();
        out.push(Cut {
            start: self.buf_start,
            samples,
        });
        self.buf_start += end as u64;
        self.last_voiced_end = self.last_voiced_end.saturating_sub(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;
    const MS: usize = 16;

    /// Ruído determinístico (xorshift) com o nível RMS pedido, em dBFS.
    fn noise(ms: usize, db: f32, seed: u32) -> Vec<f32> {
        let a = 10f32.powf(db / 20.0) * 3f32.sqrt();
        let mut x = seed.max(1);
        (0..ms * MS)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                ((x as f32 / u32::MAX as f32) * 2.0 - 1.0) * a
            })
            .collect()
    }

    fn speech(ms: usize) -> Vec<f32> {
        noise(ms, -20.0, 7)
    }

    fn quiet(ms: usize) -> Vec<f32> {
        noise(ms, -70.0, 3)
    }

    fn run(signal: &[f32]) -> Vec<Cut> {
        let mut seg = Segmenter::new(RATE);
        let mut out = Vec::new();
        // Em fatias de tamanho estranho, como os callbacks reais.
        for chunk in signal.chunks(333) {
            seg.push(chunk, &mut out);
        }
        seg.finish(&mut out);
        out
    }

    fn ms(samples: usize) -> usize {
        samples / MS
    }

    #[test]
    fn utterance_cut_at_pause() {
        let mut s = quiet(1000);
        s.extend(speech(1500));
        s.extend(quiet(2000));
        s.extend(speech(1000));
        s.extend(quiet(1500));
        let cuts = run(&s);
        assert_eq!(cuts.len(), 2, "{cuts:?}");
        // 1,5 s de fala + margens (300 ms antes, 400 ms depois).
        let first = ms(cuts[0].samples.len());
        assert!((2100..=2300).contains(&first), "primeiro = {first} ms");
        let start = ms(cuts[0].start as usize);
        assert!((650..=750).contains(&start), "começa em {start} ms");
        assert!(cuts[1].start > cuts[0].start + cuts[0].samples.len() as u64);
    }

    #[test]
    fn short_pause_does_not_split() {
        let mut s = quiet(500);
        s.extend(speech(800));
        s.extend(quiet(500));
        s.extend(speech(800));
        s.extend(quiet(1500));
        assert_eq!(run(&s).len(), 1);
    }

    #[test]
    fn silence_and_clicks_make_nothing() {
        let mut s = quiet(3000);
        s.extend(speech(40)); // um clique
        s.extend(quiet(3000));
        assert!(run(&s).is_empty());
    }

    #[test]
    fn utterance_never_exceeds_max() {
        let mut s = quiet(500);
        s.extend(speech(120_000)); // 2 min seguidos sem pausa
        s.extend(quiet(2000));
        let cuts = run(&s);
        assert!(cuts.len() >= 3, "{} cortes", cuts.len());
        for c in &cuts {
            assert!(
                ms(c.samples.len()) <= MAX_UTTERANCE.as_millis() as usize + 100,
                "{} ms",
                ms(c.samples.len())
            );
        }
        let total: usize = cuts.iter().map(|c| c.samples.len()).sum();
        assert!(ms(total) >= 119_000, "perdeu áudio: {} ms", ms(total));
    }

    #[test]
    fn long_cut_prefers_the_pause() {
        // 40 s de fala, uma pausa de 400 ms (abaixo do HANG), mais 20 s de fala.
        let mut s = speech(40_000);
        s.extend(quiet(400));
        s.extend(speech(20_000));
        s.extend(quiet(2000));
        let cuts = run(&s);
        assert!(cuts.len() >= 2);
        let first_end = ms(cuts[0].samples.len());
        assert!(
            (40_000..=40_500).contains(&first_end),
            "cortou em {first_end} ms, a pausa estava aos 40 s"
        );
    }

    #[test]
    fn cuts_never_overlap_and_are_ordered() {
        let mut s = Vec::new();
        for i in 0..6 {
            s.extend(quiet(1200));
            s.extend(speech(900 + i * 300));
        }
        s.extend(quiet(2000));
        let cuts = run(&s);
        assert_eq!(cuts.len(), 6);
        for w in cuts.windows(2) {
            assert!(w[0].start + w[0].samples.len() as u64 <= w[1].start);
        }
    }

    #[test]
    fn finish_flushes_speech_in_progress() {
        let mut s = quiet(500);
        s.extend(speech(1200)); // a reunião acaba a meio de uma frase
        let cuts = run(&s);
        assert_eq!(cuts.len(), 1);
    }

    #[test]
    fn start_positions_are_absolute() {
        let mut s = quiet(10_000);
        s.extend(speech(1000));
        s.extend(quiet(2000));
        let cuts = run(&s);
        let start = ms(cuts[0].start as usize);
        assert!((9_650..=9_750).contains(&start), "começa em {start} ms");
        // As amostras cortadas são mesmo as que lá estavam.
        let at = cuts[0].start as usize;
        assert_eq!(cuts[0].samples[..64], s[at..at + 64]);
    }
}
