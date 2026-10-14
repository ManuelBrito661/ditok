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
    fn empty_input_is_a_valid_wav() {
        let wav = encode_pcm16_mono(&[], 16_000);
        assert_eq!(wav.len(), 44);
        assert_eq!(u32_at(&wav, 40), 0);
    }
}
