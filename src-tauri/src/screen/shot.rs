//! A fotografia do ecrã, sem janelas nem rede: converter a área escolhida na página em píxeis
//! da imagem, recortar, reduzir e codificar. Tudo funções puras, por isso testáveis sem
//! ecrã.
//!
//! Três tamanhos andam por aqui, e convém não os misturar:
//! - **o ecrã**: a imagem inteira do monitor, tal como o sistema a deu (`RgbaImage`);
//! - **a página**: a janela de seleção, em píxeis CSS. No Windows tem tantos píxeis como a
//!   imagem a dividir pela escala do ecrã; no macOS, a imagem tem o dobro num ecrã Retina.
//!   Por isso a página manda também o tamanho da sua área visível (`Viewport`) e a conversão
//!   usa a razão entre os dois, em vez de adivinhar a escala do monitor;
//! - **o que segue para o modelo**: o recorte, reduzido a `MAX_SIDE` no lado maior.

use image::{
    DynamicImage, ExtendedColorType, ImageEncoder, RgbaImage,
    codecs::{
        jpeg::JpegEncoder,
        png::{CompressionType, FilterType as PngFilter, PngEncoder},
    },
    imageops::{self, FilterType},
};
use serde::Deserialize;

/// O lado maior do que se envia ao modelo. Acima disto o custo em tokens sobe e o texto
/// pequeno de uma interface continua legível: um ecrã 4K chega aos 2048 px sem perder o que
/// se lê num ecrã 1440p.
pub const MAX_SIDE: u32 = 2048;
/// Um PNG maior do que isto (uma fotografia, um vídeo) passa a JPEG.
const MAX_PNG_BYTES: usize = 4 * 1024 * 1024;
/// A qualidade do JPEG que segue para o modelo (quando o PNG é grande).
const MODEL_JPEG_QUALITY: u8 = 88;
/// A página mostra a fotografia em JPEG: só se vê, não se lê de volta.
const DISPLAY_JPEG_QUALITY: u8 = 82;
/// Lado maior da miniatura que o assistente mostra na conversa.
pub const THUMB_SIDE: u32 = 760;
const THUMB_JPEG_QUALITY: u8 = 80;
/// Uma área mais pequena do que isto (em píxeis do ecrã) é um clique, não uma seleção.
pub const MIN_SIDE: u32 = 4;

/// A área escolhida na página, em píxeis CSS da janela de seleção.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// O tamanho da área visível da página (`innerWidth` × `innerHeight`), em píxeis CSS.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Viewport {
    pub width: f64,
    pub height: f64,
}

/// Um retângulo em píxeis da imagem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ShotError {
    #[error("a área escolhida não é válida")]
    BadArea,
    #[error("a área escolhida é demasiado pequena")]
    TooSmall,
    #[error("não foi possível codificar a imagem: {0}")]
    Encode(#[from] image::ImageError),
}

/// Uma imagem pronta a enviar: PNG ou JPEG já codificado, com o tamanho de píxeis que tem.
#[derive(Debug, Clone)]
pub struct Encoded {
    pub mime: &'static str,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Converte a área da página em píxeis da imagem de `image` (largura, altura), com a mesma
/// razão nos dois eixos que o `viewport` tem para a imagem. A área é cortada aos limites da
/// imagem (uma seleção que sai um pouco da janela não falha).
pub fn to_pixels(
    area: Area,
    viewport: Viewport,
    image: (u32, u32),
) -> Result<PixelRect, ShotError> {
    let finite = [
        area.x,
        area.y,
        area.width,
        area.height,
        viewport.width,
        viewport.height,
    ]
    .iter()
    .all(|v| v.is_finite());
    if !finite
        || viewport.width < 1.0
        || viewport.height < 1.0
        || area.width <= 0.0
        || area.height <= 0.0
    {
        return Err(ShotError::BadArea);
    }
    let (iw, ih) = (f64::from(image.0), f64::from(image.1));
    let (sx, sy) = (iw / viewport.width, ih / viewport.height);
    // Bordas arredondadas ao píxel mais próximo e depois cortadas à imagem: assim duas áreas
    // vizinhas não deixam uma linha de fora.
    let left = (area.x * sx).round().clamp(0.0, iw);
    let top = (area.y * sy).round().clamp(0.0, ih);
    let right = ((area.x + area.width) * sx).round().clamp(0.0, iw);
    let bottom = ((area.y + area.height) * sy).round().clamp(0.0, ih);
    let (width, height) = ((right - left) as u32, (bottom - top) as u32);
    if width < MIN_SIDE || height < MIN_SIDE {
        return Err(ShotError::TooSmall);
    }
    Ok(PixelRect {
        x: left as u32,
        y: top as u32,
        width,
        height,
    })
}

/// Copia `rect` da imagem.
pub fn crop(image: &RgbaImage, rect: PixelRect) -> RgbaImage {
    imageops::crop_imm(image, rect.x, rect.y, rect.width, rect.height).to_image()
}

/// O tamanho que fica depois de reduzir `(w, h)` para o lado maior ser, no máximo, `max`.
/// Nunca amplia nem deixa um lado a zero.
pub fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    let longest = w.max(h);
    if longest <= max {
        return (w, h);
    }
    let scale = f64::from(max) / f64::from(longest);
    (
        ((f64::from(w) * scale).round() as u32).max(1),
        ((f64::from(h) * scale).round() as u32).max(1),
    )
}

/// Sem canal alfa (alguns sistemas dão 0 em todos os píxeis) e, se for preciso, reduzida.
fn opaque(image: &RgbaImage, max_side: u32) -> image::RgbImage {
    let (w, h) = fit(image.width(), image.height(), max_side);
    if (w, h) == (image.width(), image.height()) {
        return DynamicImage::ImageRgba8(image.clone()).into_rgb8();
    }
    let resized = imageops::resize(image, w, h, FilterType::Lanczos3);
    DynamicImage::ImageRgba8(resized).into_rgb8()
}

fn jpeg(rgb: &image::RgbImage, quality: u8) -> Result<Vec<u8>, ShotError> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality).write_image(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(out)
}

fn png(rgb: &image::RgbImage) -> Result<Vec<u8>, ShotError> {
    let mut out = Vec::new();
    // `Fast` + filtro adaptativo: um ecrã é cheio de zonas planas e comprime bem à mesma, e
    // o tempo conta (o utilizador está à espera da resposta).
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, PngFilter::Adaptive)
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ExtendedColorType::Rgb8,
        )?;
    Ok(out)
}

/// A imagem que vai ao modelo: PNG (sem perdas, o melhor para texto pequeno), ou JPEG se o
/// PNG ficar grande demais. O lado maior não passa de `MAX_SIDE`.
pub fn encode_for_model(image: &RgbaImage) -> Result<Encoded, ShotError> {
    encode_with(image, MAX_SIDE, MAX_PNG_BYTES)
}

fn encode_with(image: &RgbaImage, max_side: u32, max_png: usize) -> Result<Encoded, ShotError> {
    let rgb = opaque(image, max_side);
    let (width, height) = (rgb.width(), rgb.height());
    let bytes = png(&rgb)?;
    if bytes.len() <= max_png {
        return Ok(Encoded {
            mime: "image/png",
            bytes,
            width,
            height,
        });
    }
    Ok(Encoded {
        mime: "image/jpeg",
        bytes: jpeg(&rgb, MODEL_JPEG_QUALITY)?,
        width,
        height,
    })
}

/// O JPEG que a página de seleção mostra (o ecrã inteiro, no tamanho original).
pub fn encode_display(image: &RgbaImage) -> Result<Vec<u8>, ShotError> {
    jpeg(&opaque(image, u32::MAX), DISPLAY_JPEG_QUALITY)
}

/// A miniatura que o assistente mostra na conversa.
pub fn encode_thumbnail(image: &RgbaImage) -> Result<Vec<u8>, ShotError> {
    jpeg(&opaque(image, THUMB_SIDE), THUMB_JPEG_QUALITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: f64, y: f64, width: f64, height: f64) -> Area {
        Area {
            x,
            y,
            width,
            height,
        }
    }

    fn view(width: f64, height: f64) -> Viewport {
        Viewport { width, height }
    }

    #[test]
    fn a_one_to_one_screen_keeps_the_pixels() {
        let r = to_pixels(
            area(100.0, 50.0, 300.0, 200.0),
            view(1920.0, 1080.0),
            (1920, 1080),
        )
        .unwrap();
        assert_eq!(
            r,
            PixelRect {
                x: 100,
                y: 50,
                width: 300,
                height: 200
            }
        );
    }

    #[test]
    fn a_retina_image_is_scaled_by_the_viewport_ratio() {
        // A janela tem 1440×900 pontos; a imagem, 2880×1800 píxeis.
        let r = to_pixels(
            area(10.0, 20.0, 100.0, 50.0),
            view(1440.0, 900.0),
            (2880, 1800),
        )
        .unwrap();
        assert_eq!(
            r,
            PixelRect {
                x: 20,
                y: 40,
                width: 200,
                height: 100
            }
        );
    }

    #[test]
    fn a_fractional_scale_rounds_the_edges_not_the_size() {
        // 125 %: 1536×864 pontos para 1920×1080 píxeis.
        let r = to_pixels(
            area(0.4, 0.4, 100.0, 100.0),
            view(1536.0, 864.0),
            (1920, 1080),
        )
        .unwrap();
        assert_eq!((r.x, r.y), (1, 1));
        assert_eq!((r.width, r.height), (125, 125));
    }

    #[test]
    fn the_whole_viewport_is_the_whole_image() {
        let r = to_pixels(
            area(0.0, 0.0, 1536.0, 864.0),
            view(1536.0, 864.0),
            (1920, 1080),
        )
        .unwrap();
        assert_eq!(
            r,
            PixelRect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080
            }
        );
    }

    #[test]
    fn an_area_leaving_the_window_is_clipped() {
        let r = to_pixels(
            area(-30.0, 900.0, 200.0, 400.0),
            view(1000.0, 1000.0),
            (1000, 1000),
        )
        .unwrap();
        assert_eq!(
            r,
            PixelRect {
                x: 0,
                y: 900,
                width: 170,
                height: 100
            }
        );
    }

    #[test]
    fn nonsense_areas_are_refused() {
        let v = view(1000.0, 1000.0);
        let img = (1000, 1000);
        assert!(matches!(
            to_pixels(area(f64::NAN, 0.0, 10.0, 10.0), v, img),
            Err(ShotError::BadArea)
        ));
        assert!(matches!(
            to_pixels(area(0.0, 0.0, -5.0, 10.0), v, img),
            Err(ShotError::BadArea)
        ));
        assert!(matches!(
            to_pixels(area(0.0, 0.0, 10.0, 10.0), view(0.0, 0.0), img),
            Err(ShotError::BadArea)
        ));
        assert!(matches!(
            to_pixels(area(0.0, 0.0, 10.0, 10.0), view(f64::INFINITY, 10.0), img),
            Err(ShotError::BadArea)
        ));
        // Toda fora da imagem: fica sem píxeis.
        assert!(matches!(
            to_pixels(area(2000.0, 0.0, 50.0, 50.0), v, img),
            Err(ShotError::TooSmall)
        ));
    }

    #[test]
    fn a_click_is_not_a_selection() {
        assert!(matches!(
            to_pixels(
                area(10.0, 10.0, 3.0, 3.0),
                view(1000.0, 1000.0),
                (1000, 1000)
            ),
            Err(ShotError::TooSmall)
        ));
        assert!(
            to_pixels(
                area(10.0, 10.0, 4.0, 4.0),
                view(1000.0, 1000.0),
                (1000, 1000)
            )
            .is_ok()
        );
    }

    #[test]
    fn fit_only_shrinks() {
        assert_eq!(fit(800, 600, 2048), (800, 600));
        assert_eq!(fit(3840, 2160, 2048), (2048, 1152));
        assert_eq!(fit(1000, 4000, 2000), (500, 2000));
        assert_eq!(fit(10_000, 1, 100), (100, 1), "nunca fica a zero");
    }

    /// Um ecrã de mentira: listras para o PNG não ser trivial, e alfa a 0 como alguns
    /// sistemas dão.
    fn fake_screen(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            let v = if (x / 8 + y / 8) % 2 == 0 { 240 } else { 20 };
            image::Rgba([v, (x % 256) as u8, (y % 256) as u8, 0])
        })
    }

    #[test]
    fn crop_takes_the_right_pixels() {
        let img = fake_screen(100, 80);
        let r = PixelRect {
            x: 10,
            y: 20,
            width: 30,
            height: 15,
        };
        let c = crop(&img, r);
        assert_eq!((c.width(), c.height()), (30, 15));
        assert_eq!(c.get_pixel(0, 0), img.get_pixel(10, 20));
        assert_eq!(c.get_pixel(29, 14), img.get_pixel(39, 34));
    }

    #[test]
    fn a_small_capture_goes_as_png_and_drops_the_alpha() {
        let enc = encode_for_model(&fake_screen(300, 200)).unwrap();
        assert_eq!(enc.mime, "image/png");
        assert_eq!((enc.width, enc.height), (300, 200));
        assert_eq!(&enc.bytes[..8], b"\x89PNG\r\n\x1a\n");
        let back = image::load_from_memory(&enc.bytes).unwrap();
        assert_eq!(back.color(), image::ColorType::Rgb8, "sem canal alfa");
        assert_eq!((back.width(), back.height()), (300, 200));
    }

    #[test]
    fn a_big_capture_is_scaled_down_before_encoding() {
        let enc = encode_with(&fake_screen(400, 200), 100, usize::MAX).unwrap();
        assert_eq!((enc.width, enc.height), (100, 50));
        let back = image::load_from_memory(&enc.bytes).unwrap();
        assert_eq!((back.width(), back.height()), (100, 50));
    }

    #[test]
    fn a_heavy_png_falls_back_to_jpeg() {
        let enc = encode_with(&fake_screen(300, 200), 2048, 10).unwrap();
        assert_eq!(enc.mime, "image/jpeg");
        assert_eq!(&enc.bytes[..2], b"\xff\xd8");
        assert!(image::load_from_memory(&enc.bytes).is_ok());
    }

    #[test]
    fn display_and_thumbnail_are_jpegs_of_the_right_size() {
        let img = fake_screen(1600, 900);
        let display = encode_display(&img).unwrap();
        let shown = image::load_from_memory(&display).unwrap();
        assert_eq!(
            (shown.width(), shown.height()),
            (1600, 900),
            "tamanho original"
        );
        let thumb = encode_thumbnail(&img).unwrap();
        let small = image::load_from_memory(&thumb).unwrap();
        assert_eq!((small.width(), small.height()), (THUMB_SIDE, 428));
    }
}
