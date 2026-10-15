//! Fotografar o monitor onde está o cursor. Windows e macOS, com o `xcap`; noutros sistemas
//! (o Ditok não corre neles) devolve `Unsupported`, para o resto da app continuar a compilar
//! e a testar-se.
//!
//! O difícil é saber QUAL monitor do `xcap` é o que o Tauri escolheu para a janela de seleção:
//! cada um tem a sua lista, com unidades diferentes no macOS (o `xcap` dá pontos, o Tauri
//! píxeis físicos). `Bounds` põe os dois no mesmo espaço (píxeis físicos no ecrã virtual) e
//! `best_match` escolhe pelo centro.

// Fora do Windows e do macOS só existe o `Unsupported`: o resto é código sem uso (mas testado).
#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use image::RgbaImage;

/// Um monitor em píxeis físicos, no ecrã virtual (o canto do monitor principal é a origem).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Bounds {
    fn center(self) -> (f64, f64) {
        (
            f64::from(self.x) + f64::from(self.width) / 2.0,
            f64::from(self.y) + f64::from(self.height) / 2.0,
        )
    }

    fn contains(self, (px, py): (f64, f64)) -> bool {
        px >= f64::from(self.x)
            && px < f64::from(self.x) + f64::from(self.width)
            && py >= f64::from(self.y)
            && py < f64::from(self.y) + f64::from(self.height)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GrabError {
    #[error("a captura do ecrã só existe no Windows e no macOS")]
    #[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
    Unsupported,
    #[error("não foi possível fotografar o ecrã: {0}")]
    Failed(String),
}

/// Qual dos `candidates` é o monitor `target`: o que contém o centro dele, ou, se nenhum o
/// contiver (arredondamentos, monitores com escalas diferentes), o de centro mais próximo.
/// `None` só com a lista vazia.
pub fn best_match(target: Bounds, candidates: &[Bounds]) -> Option<usize> {
    let center = target.center();
    if let Some(i) = candidates.iter().position(|c| c.contains(center)) {
        return Some(i);
    }
    let distance = |c: &Bounds| {
        let (cx, cy) = c.center();
        (cx - center.0).powi(2) + (cy - center.1).powi(2)
    };
    candidates
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)))
        .map(|(i, _)| i)
}

/// O Ditok pode ver o ecrã? Só o macOS pede autorização («Gravação do ecrã»). Sem ela, o
/// sistema devolve uma imagem só com o papel de parede, sem erro nenhum: por isso pergunta-se
/// antes de fotografar. Se faltar, pede-a ao sistema (que mostra o seu aviso, uma vez).
pub fn ensure_access() -> bool {
    imp::ensure_access()
}

/// A fotografia do monitor `target`.
pub fn grab(target: Bounds) -> Result<RgbaImage, GrabError> {
    imp::grab(target)
}

#[cfg(any(windows, target_os = "macos"))]
mod imp {
    use super::{Bounds, GrabError, best_match};
    use image::RgbaImage;

    #[cfg(target_os = "macos")]
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        // macOS 10.15+. A primeira só pergunta; a segunda pergunta e, se for preciso,
        // mostra o aviso do sistema (devolve logo, sem esperar pela resposta).
        safe fn CGPreflightScreenCaptureAccess() -> bool;
        safe fn CGRequestScreenCaptureAccess() -> bool;
    }

    #[cfg(target_os = "macos")]
    pub fn ensure_access() -> bool {
        CGPreflightScreenCaptureAccess() || {
            // Mesmo depois de a dar, o macOS só a aplica à app quando ela reabre.
            CGRequestScreenCaptureAccess();
            false
        }
    }

    #[cfg(windows)]
    pub fn ensure_access() -> bool {
        true
    }

    /// O monitor do `xcap` em píxeis físicos. No Windows já o são; no macOS vêm em pontos,
    /// e o Tauri multiplica pela escala do próprio monitor.
    fn physical(monitor: &xcap::Monitor) -> Option<Bounds> {
        let scale = if cfg!(target_os = "macos") {
            f64::from(monitor.scale_factor().ok()?)
        } else {
            1.0
        };
        let px = |v: f64| (v * scale).round();
        Some(Bounds {
            x: px(f64::from(monitor.x().ok()?)) as i32,
            y: px(f64::from(monitor.y().ok()?)) as i32,
            width: px(f64::from(monitor.width().ok()?)) as u32,
            height: px(f64::from(monitor.height().ok()?)) as u32,
        })
    }

    pub fn grab(target: Bounds) -> Result<RgbaImage, GrabError> {
        let monitors = xcap::Monitor::all().map_err(|e| GrabError::Failed(e.to_string()))?;
        // Só entram os que se sabe medir; `best_match` devolve o índice dentro desta lista.
        let measured: Vec<(usize, Bounds)> = monitors
            .iter()
            .enumerate()
            .filter_map(|(i, m)| physical(m).map(|b| (i, b)))
            .collect();
        let bounds: Vec<Bounds> = measured.iter().map(|&(_, b)| b).collect();
        let chosen = best_match(target, &bounds)
            .map(|i| measured[i].0)
            .ok_or_else(|| GrabError::Failed("não há nenhum monitor".into()))?;
        tracing::debug!(
            ?target,
            chosen,
            monitors = monitors.len(),
            "monitor a fotografar"
        );
        monitors[chosen]
            .capture_image()
            .map_err(|e| GrabError::Failed(e.to_string()))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use super::{Bounds, GrabError};
    use image::RgbaImage;

    pub fn ensure_access() -> bool {
        true
    }

    pub fn grab(_target: Bounds) -> Result<RgbaImage, GrabError> {
        Err(GrabError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(x: i32, y: i32, width: u32, height: u32) -> Bounds {
        Bounds {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn the_monitor_with_the_same_center_wins() {
        let list = [b(0, 0, 1920, 1080), b(1920, 0, 2560, 1440)];
        assert_eq!(best_match(b(1920, 0, 2560, 1440), &list), Some(1));
        assert_eq!(best_match(b(0, 0, 1920, 1080), &list), Some(0));
    }

    #[test]
    fn monitors_left_of_or_above_the_main_one_have_negative_origins() {
        let list = [b(0, 0, 1920, 1080), b(-1280, -200, 1280, 1024)];
        assert_eq!(best_match(b(-1280, -200, 1280, 1024), &list), Some(1));
    }

    #[test]
    fn a_small_rounding_difference_still_matches() {
        // O Tauri diz 1536 de largura onde o xcap, ao arredondar a escala, diz 1537.
        let list = [b(0, 0, 1537, 864), b(1537, 0, 1920, 1080)];
        assert_eq!(best_match(b(0, 0, 1536, 864), &list), Some(0));
    }

    #[test]
    fn without_an_overlap_the_closest_center_wins() {
        // Escalas diferentes no macOS: o centro do alvo cai num intervalo entre os dois.
        let list = [b(0, 0, 1000, 800), b(1100, 0, 1000, 800)];
        assert_eq!(best_match(b(1030, 0, 80, 800), &list), Some(1));
        assert_eq!(best_match(b(900, 0, 80, 800), &list), Some(0));
        assert_eq!(best_match(b(1090, 0, 4, 800), &list), Some(1));
    }

    #[test]
    fn an_empty_list_matches_nothing() {
        assert_eq!(best_match(b(0, 0, 100, 100), &[]), None);
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn other_systems_are_unsupported() {
        assert!(matches!(grab(b(0, 0, 10, 10)), Err(GrabError::Unsupported)));
        assert!(ensure_access());
    }
}
