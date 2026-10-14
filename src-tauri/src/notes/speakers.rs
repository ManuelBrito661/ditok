//! Quem fala dentro de "Outros": a separação de oradores da xAI.
//!
//! Corre **depois** da reunião, sobre o áudio da faixa dos outros que se foi guardando
//! num ficheiro temporário (`diar.pcm`, apagado no fim, com a linha do tempo da sessão: os
//! silêncios são zeros). Esse áudio vai inteiro, de uma vez, ao `POST /v1/stt` com
//! `diarize=true`: a xAI devolve as palavras com o número de quem as disse. Tem de ser
//! numa só chamada, e não enunciado a enunciado como a transcrição ao vivo, porque o
//! número de um orador só quer dizer o mesmo dentro de um mesmo pedido.
//!
//! Das palavras fazem-se trechos (`segments_from_words`), numeram-se os oradores pela ordem
//! em que começam a falar (`renumber`) e dá-se a cada linha já transcrita o orador que mais
//! se sobrepõe a ela (`assign`). Tudo isto é puro e testa-se sem rede.

use bytes::Bytes;

use super::{capture::Track, store::Line};
use crate::xai::{
    ApiError, Xai,
    stt::{BYTES_PER_SEC, Word},
};

/// Mais do que isto (em bytes de PCM s16le a 16 kHz, 90 minutos) não se envia: são
/// ~170 MB e o pedido já demora.
pub const MAX_PCM_BYTES: usize = 90 * 60 * BYTES_PER_SEC as usize;

/// Palavras do mesmo orador separadas por menos do que isto (em segundos) fazem parte do
/// mesmo trecho.
const MERGE_GAP_S: f64 = 1.5;

/// Um trecho em que fala um orador (1, 2, 3…), em milissegundos desde o início da sessão.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum SpeakersError {
    #[error("a reunião é demasiado longa para separar os oradores (o limite são 90 minutos)")]
    TooLong,
    #[error("a xAI não conseguiu separar os oradores: {0}")]
    Api(#[from] ApiError),
}

/// Separa os oradores de `pcm` (s16le, 16 kHz, mono, com a linha do tempo da sessão).
pub async fn diarize(
    xai: &Xai,
    pcm: Bytes,
    language: Option<&str>,
) -> Result<Vec<Segment>, SpeakersError> {
    if pcm.len() > MAX_PCM_BYTES {
        return Err(SpeakersError::TooLong);
    }
    let words = xai.diarize(pcm, language).await?;
    let mut segments = segments_from_words(&words);
    renumber(&mut segments);
    Ok(segments)
}

// ---------------------------------------------------------------- puro

/// Junta as palavras seguidas do mesmo orador em trechos. Uma palavra sem orador (a xAI
/// nem sempre o atribui) não abre nem fecha nada: o trecho em curso aguenta-a se a pausa
/// for curta.
pub fn segments_from_words(words: &[Word]) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let ms = |s: f64| (s.max(0.0) * 1000.0).round() as u64;
    for w in words {
        let Some(speaker) = w.speaker else { continue };
        let (start, end) = (ms(w.start), ms(w.end.max(w.start)));
        match out.last_mut() {
            Some(last)
                if last.speaker == speaker
                    && start.saturating_sub(last.end_ms) as f64 <= MERGE_GAP_S * 1000.0 =>
            {
                last.end_ms = last.end_ms.max(end);
            }
            _ => out.push(Segment {
                start_ms: start,
                end_ms: end,
                speaker,
            }),
        }
    }
    out
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

    fn word(start: f64, end: f64, speaker: Option<u32>) -> Word {
        Word {
            start,
            end,
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
        // A xAI chamou "7" a quem falou primeiro e "3" ao segundo.
        let mut segs = [seg(5_000, 6_000, 3), seg(0, 2_000, 7), seg(8_000, 9_000, 7)];
        renumber(&mut segs);
        let by_start: Vec<(u64, u32)> = segs.iter().map(|s| (s.start_ms, s.speaker)).collect();
        assert_eq!(by_start, [(0, 1), (5_000, 2), (8_000, 1)]);
    }

    #[test]
    fn words_of_the_same_speaker_become_one_segment() {
        let words = [
            word(0.0, 0.4, Some(0)),
            word(0.5, 0.9, Some(0)),
            // Pausa de 3 s: o mesmo orador, mas outro trecho.
            word(3.9, 4.2, Some(0)),
            word(4.3, 4.8, Some(1)),
        ];
        assert_eq!(
            segments_from_words(&words),
            [seg(0, 900, 0), seg(3_900, 4_200, 0), seg(4_300, 4_800, 1)]
        );
    }

    #[test]
    fn words_without_a_speaker_are_skipped_and_do_not_split_a_segment() {
        let words = [
            word(0.0, 0.4, Some(2)),
            word(0.5, 0.8, None),
            word(0.9, 1.3, Some(2)),
        ];
        assert_eq!(segments_from_words(&words), [seg(0, 1_300, 2)]);
        assert!(segments_from_words(&[]).is_empty());
        assert!(segments_from_words(&[word(0.0, 1.0, None)]).is_empty());
    }

    #[test]
    fn a_word_that_ends_before_it_starts_does_not_shrink_the_segment() {
        let words = [word(2.0, 1.0, Some(0)), word(2.1, 2.5, Some(0))];
        let segs = segments_from_words(&words);
        assert_eq!(segs, [seg(2_000, 2_500, 0)]);
    }

    #[tokio::test]
    async fn too_long_a_meeting_is_refused_before_any_request() {
        let xai = Xai::new(crate::xai::http_client().unwrap(), "k".into());
        let pcm = Bytes::from(vec![0u8; MAX_PCM_BYTES + 2]);
        assert!(matches!(
            diarize(&xai, pcm, None).await,
            Err(SpeakersError::TooLong)
        ));
    }
}
