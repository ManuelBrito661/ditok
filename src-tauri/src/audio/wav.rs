//! WAV PCM 16-bit mono em memória. O formato é só um cabeçalho de 44 bytes seguido
//! das amostras, por isso escrevemo-lo à mão em vez de puxar uma dependência.

use bytes::{BufMut, Bytes, BytesMut};

const HEADER_LEN: usize = 44;

/// Converte amostras f32 (-1.0..=1.0) em WAV PCM16. Valores fora do intervalo são
/// cortados (clipping) em vez de darem a volta.
///
/// Devolve `Bytes` (buffer com contagem de referências): mais tarde o mesmo áudio
/// pode ir para o pedido HTTP e para o benchmark sem ser copiado.
pub fn encode_pcm16_mono(samples: &[f32], sample_rate: u32) -> Bytes {
    let data_len = samples.len() * 2;
    // Uma única alocação com o tamanho exato.
    let mut buf = BytesMut::with_capacity(HEADER_LEN + data_len);

    buf.put_slice(b"RIFF");
    buf.put_u32_le((36 + data_len) as u32);
    buf.put_slice(b"WAVE");
    buf.put_slice(b"fmt ");
    buf.put_u32_le(16); // tamanho do bloco fmt
    buf.put_u16_le(1); // PCM
    buf.put_u16_le(1); // mono
    buf.put_u32_le(sample_rate);
    buf.put_u32_le(sample_rate * 2); // byte rate = rate * canais * 2 bytes
    buf.put_u16_le(2); // block align
    buf.put_u16_le(16); // bits por amostra
    buf.put_slice(b"data");
    buf.put_u32_le(data_len as u32);

    for &s in samples {
        buf.put_i16_le(f32_to_i16(s));
    }
    buf.freeze()
}

fn f32_to_i16(s: f32) -> i16 {
    // `as` de f32 para i16 já satura em Rust, mas o clamp torna a intenção explícita
    // e trata o NaN (que `as` converte em 0).
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[cfg_attr(not(feature = "local"), allow(dead_code))] // só o motor local descodifica WAV
pub enum WavError {
    #[error("o áudio não é um WAV")]
    NotWav,
    #[error("o WAV tem de ser PCM de 16 bits e mono")]
    Unsupported,
    #[error("o WAV está cortado")]
    Truncated,
}

/// O contrário de `encode_pcm16_mono`: WAV PCM16 mono → amostras f32 (-1.0..1.0) e
/// taxa. Só aceita o que o Ditok escreve (e o que o histórico guarda); percorre os
/// blocos em vez de assumir um cabeçalho de 44 bytes, para tolerar blocos extra.
#[cfg_attr(not(feature = "local"), allow(dead_code))] // só o motor local descodifica WAV
pub fn decode_pcm16_mono(wav: &[u8]) -> Result<(Vec<f32>, u32), WavError> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err(WavError::NotWav);
    }
    let mut rate = None;
    let mut at = 12;
    while at + 8 <= wav.len() {
        let id = &wav[at..at + 4];
        let size =
            u32::from_le_bytes([wav[at + 4], wav[at + 5], wav[at + 6], wav[at + 7]]) as usize;
        let body = at + 8;
        if id == b"fmt " {
            let fmt = wav.get(body..body + 16).ok_or(WavError::Truncated)?;
            let format = u16::from_le_bytes([fmt[0], fmt[1]]);
            let channels = u16::from_le_bytes([fmt[2], fmt[3]]);
            let bits = u16::from_le_bytes([fmt[14], fmt[15]]);
            if format != 1 || channels != 1 || bits != 16 {
                return Err(WavError::Unsupported);
            }
            rate = Some(u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]));
        } else if id == b"data" {
            let rate = rate.ok_or(WavError::NotWav)?;
            // Um ficheiro cortado a meio: lê-se o que há (um número par de bytes).
            let end = (body + size).min(wav.len());
            let samples = wav[body..end]
                .chunks_exact(2)
                .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0)
                .collect();
            return Ok((samples, rate));
        }
        // Os blocos têm tamanho par (com um byte de enchimento se for ímpar).
        at = body + size + (size & 1);
    }
    Err(WavError::Truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
    }
    fn u16_at(b: &[u8], i: usize) -> u16 {
        u16::from_le_bytes([b[i], b[i + 1]])
    }

    #[test]
    fn header_fields_are_correct() {
        let wav = encode_pcm16_mono(&[0.0; 16_000], 16_000);
        assert_eq!(wav.len(), 44 + 32_000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32_at(&wav, 4), 36 + 32_000);
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u16_at(&wav, 20), 1, "PCM");
        assert_eq!(u16_at(&wav, 22), 1, "mono");
        assert_eq!(u32_at(&wav, 24), 16_000);
        assert_eq!(u32_at(&wav, 28), 32_000);
        assert_eq!(u16_at(&wav, 34), 16);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32_at(&wav, 40), 32_000);
    }

    #[test]
    fn samples_are_scaled_and_clipped() {
        let wav = encode_pcm16_mono(&[0.0, 1.0, -1.0, 2.0, -3.0, 0.5, f32::NAN], 16_000);
        let s: Vec<i16> = wav[44..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(s, vec![0, 32767, -32767, 32767, -32767, 16383, 0]);
    }

    #[test]
    fn decode_is_the_inverse_of_encode() {
        let original = [0.0, 0.5, -0.5, 0.999, -0.999, 0.25];
        let wav = encode_pcm16_mono(&original, 16_000);
        let (samples, rate) = decode_pcm16_mono(&wav).unwrap();
        assert_eq!(rate, 16_000);
        assert_eq!(samples.len(), original.len());
        for (a, b) in samples.iter().zip(original) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn decode_rejects_other_formats() {
        assert_eq!(decode_pcm16_mono(b"nao sou um wav"), Err(WavError::NotWav));
        assert_eq!(decode_pcm16_mono(b""), Err(WavError::NotWav));
        // Estéreo (o canal está no byte 22 do cabeçalho).
        let mut stereo = encode_pcm16_mono(&[0.0; 4], 16_000).to_vec();
        stereo[22] = 2;
        assert_eq!(decode_pcm16_mono(&stereo), Err(WavError::Unsupported));
        // 8 bits por amostra (byte 34).
        let mut eight = encode_pcm16_mono(&[0.0; 4], 16_000).to_vec();
        eight[34] = 8;
        assert_eq!(decode_pcm16_mono(&eight), Err(WavError::Unsupported));
    }

    #[test]
    fn decode_reads_what_is_there_of_a_cut_file_and_skips_extra_chunks() {
        let wav = encode_pcm16_mono(&[0.5; 100], 16_000).to_vec();
        // Cortado a meio de uma amostra: lê-se um número inteiro de amostras.
        let cut = &wav[..44 + 51];
        let (samples, _) = decode_pcm16_mono(cut).unwrap();
        assert_eq!(samples.len(), 25);
        // Um bloco "LIST" entre o fmt e os dados.
        let mut with_list = wav[..36].to_vec();
        with_list.extend_from_slice(b"LIST");
        with_list.extend_from_slice(&3u32.to_le_bytes());
        with_list.extend_from_slice(&[1, 2, 3, 0]); // 3 bytes + 1 de enchimento
        with_list.extend_from_slice(&wav[36..]);
        let (samples, rate) = decode_pcm16_mono(&with_list).unwrap();
        assert_eq!((samples.len(), rate), (100, 16_000));
        // Sem bloco de dados.
        assert_eq!(decode_pcm16_mono(&wav[..36]), Err(WavError::Truncated));
    }

    #[test]
    fn empty_input_is_a_valid_wav() {
        let wav = encode_pcm16_mono(&[], 16_000);
        assert_eq!(wav.len(), 44);
        assert_eq!(u32_at(&wav, 40), 0);
    }
}
