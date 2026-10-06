//! Mono-to-stereo pan laws.

use std::f32::consts::FRAC_PI_4;

/// Attenuation of a centered signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub enum PanLaw {
    /// Constant power: -3 dB at center.
    #[default]
    Minus3,
    /// Compromise: -4.5 dB at center.
    Minus4_5,
    /// Constant voltage: -6 dB at center.
    Minus6,
    /// Balance: 0 dB at center, the far side fades out linearly.
    Zero,
}

impl PanLaw {
    /// Center attenuation in dB (negative).
    pub fn center_db(self) -> f32 {
        match self {
            PanLaw::Minus3 => -3.0,
            PanLaw::Minus4_5 => -4.5,
            PanLaw::Minus6 => -6.0,
            PanLaw::Zero => 0.0,
        }
    }
}

/// Left/right gains for `pan` in -1 (hard left) ..= 1 (hard right). NaN pans center.
pub fn gains(pan: f32, law: PanLaw) -> (f32, f32) {
    let pan = if pan.is_nan() { 0.0 } else { pan.clamp(-1.0, 1.0) };
    if law == PanLaw::Zero {
        return ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0));
    }
    let theta = (pan + 1.0) * FRAC_PI_4;
    let (l, r) = (theta.cos().max(0.0), theta.sin().max(0.0));
    // cos(pi/4) = -3.0103 dB; raise to a power to reach the requested center level.
    let exp = law.center_db() / -3.010_3;
    let l = if pan <= -1.0 { 1.0 } else { l.powf(exp) };
    let r = if pan >= 1.0 { 1.0 } else { r.powf(exp) };
    (l, r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gain_to_db;

    #[test]
    fn center_levels() {
        for law in [PanLaw::Minus3, PanLaw::Minus4_5, PanLaw::Minus6, PanLaw::Zero] {
            let (l, r) = gains(0.0, law);
            assert!((gain_to_db(l) - law.center_db()).abs() < 0.02, "{law:?}");
            assert!((l - r).abs() < 1e-6);
            let (l, r) = gains(-1.0, law);
            assert!((l - 1.0).abs() < 1e-6 && r.abs() < 1e-6);
        }
    }
}
