//! Equalizer state as shown to skins: band labels, preset names and the response curve.

use crate::eq_presets::{EqPreset, builtin_presets};
use freya_audio::{BAND_FREQUENCIES, EqSettings, MAX_GAIN_DB};
use serde::{Deserialize, Serialize};

/// Gains closer to 0 dB than this snap to exactly 0, so a band is easy to flatten by hand.
const SNAP_DB: f32 = 0.3;
/// Points sampled along the curve.
const CURVE_POINTS: usize = 60;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqualizerSettings {
    pub enabled: bool,
    pub preamp_db: f32,
    pub bands_db: [f32; 10],
}

impl Default for EqualizerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            preamp_db: 0.0,
            bands_db: [0.0; 10],
        }
    }
}

impl EqualizerSettings {
    pub fn engine(&self) -> EqSettings {
        EqSettings {
            enabled: self.enabled,
            preamp_db: self.preamp_db,
            bands_db: self.bands_db,
        }
    }

    pub fn set_band(&mut self, index: usize, db: f32) {
        if let Some(band) = self.bands_db.get_mut(index) {
            *band = tidy(db);
        }
    }

    pub fn set_preamp(&mut self, db: f32) {
        self.preamp_db = tidy(db);
    }

    pub fn apply_preset(&mut self, preset: &EqPreset) {
        self.preamp_db = preset.preamp_db();
        self.bands_db = preset.bands_db();
    }

    pub fn reset(&mut self) {
        self.preamp_db = 0.0;
        self.bands_db = [0.0; 10];
    }

    /// Name of the built-in preset these settings match, or "Custom".
    pub fn preset_name(&self) -> String {
        let close = |a: f32, b: f32| (a - b).abs() < 0.05;
        builtin_presets()
            .into_iter()
            .find(|p| {
                close(p.preamp_db(), self.preamp_db)
                    && p.bands_db()
                        .iter()
                        .zip(&self.bands_db)
                        .all(|(a, b)| close(*a, *b))
            })
            .map_or_else(|| "Custom".to_owned(), |p| p.name)
    }

    pub fn as_preset(&self, name: &str) -> EqPreset {
        EqPreset::from_db(name, self.preamp_db, self.bands_db)
    }
}

/// Clamps to the allowed range, rounds to 0.1 dB and snaps values near zero to zero.
fn tidy(db: f32) -> f32 {
    let db = (db.clamp(-MAX_GAIN_DB, MAX_GAIN_DB) * 10.0).round() / 10.0;
    if db.abs() < SNAP_DB { 0.0 } else { db }
}

/// "60", "170", … "1k", … "16k".
pub fn band_labels() -> Vec<String> {
    BAND_FREQUENCIES
        .iter()
        .map(|&hz| {
            if hz >= 1000.0 {
                format!("{}k", hz / 1000.0)
            } else {
                format!("{hz}")
            }
        })
        .collect()
}

pub fn preset_names() -> Vec<String> {
    builtin_presets().into_iter().map(|p| p.name).collect()
}

/// The response curve through the band gains as SVG path data in a `width` × `height` box
/// (see the skin API). A natural cubic spline, so it looks like a smooth curve rather than a
/// polyline.
pub fn curve_path(width: f32, height: f32, bands_db: &[f32]) -> String {
    if bands_db.len() < 2 || width <= 0.0 || height <= 0.0 {
        return String::new();
    }
    let y = |db: f32| (0.5 - db / MAX_GAIN_DB * 0.45) * height;
    let points: Vec<f32> = bands_db.iter().map(|&db| y(db)).collect();
    let curve = spline(&points, CURVE_POINTS);
    let mut path = String::new();
    for (i, value) in curve.iter().enumerate() {
        let x = i as f32 / (curve.len() - 1) as f32 * width;
        let cmd = if i == 0 { 'M' } else { 'L' };
        path.push_str(&format!("{cmd}{x:.2} {:.2} ", value.clamp(0.0, height)));
    }
    path.trim_end().to_owned()
}

/// Natural cubic spline through equally spaced `points`, sampled at `samples` evenly spaced
/// positions from the first point to the last.
fn spline(points: &[f32], samples: usize) -> Vec<f32> {
    let n = points.len();
    // Second derivatives (spacing 1), from the tridiagonal system via the Thomas algorithm.
    let mut m = vec![0.0f32; n];
    if n > 2 {
        let mut c = vec![0.0f32; n];
        let mut d = vec![0.0f32; n];
        for i in 1..n - 1 {
            let rhs = 6.0 * (points[i + 1] - 2.0 * points[i] + points[i - 1]);
            let denom = 4.0 - c[i - 1];
            c[i] = 1.0 / denom;
            d[i] = (rhs - d[i - 1]) / denom;
        }
        for i in (1..n - 1).rev() {
            m[i] = d[i] - c[i] * m[i + 1];
        }
    }
    (0..samples)
        .map(|s| {
            let x = s as f32 / (samples - 1) as f32 * (n - 1) as f32;
            let i = (x.floor() as usize).min(n - 2);
            let t = x - i as f32;
            let (a, b) = (1.0 - t, t);
            m[i] * a * a * a / 6.0
                + m[i + 1] * b * b * b / 6.0
                + (points[i] - m[i] / 6.0) * a
                + (points[i + 1] - m[i + 1] / 6.0) * b
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_clamps_rounds_and_snaps() {
        let mut eq = EqualizerSettings::default();
        eq.set_band(0, 20.0);
        eq.set_band(1, 0.2);
        eq.set_band(2, -3.04);
        eq.set_band(99, 5.0);
        assert_eq!(eq.bands_db[..3], [12.0, 0.0, -3.0]);
    }

    #[test]
    fn matches_presets_by_value() {
        let mut eq = EqualizerSettings::default();
        assert_eq!(eq.preset_name(), "Flat");
        let rock = builtin_presets()
            .into_iter()
            .find(|p| p.name == "Rock")
            .unwrap();
        eq.apply_preset(&rock);
        assert_eq!(eq.preset_name(), "Rock");
        eq.set_band(0, 1.0);
        assert_eq!(eq.preset_name(), "Custom");
    }

    #[test]
    fn labels_and_curve() {
        assert_eq!(
            band_labels(),
            [
                "60", "170", "310", "600", "1k", "3k", "6k", "12k", "14k", "16k"
            ]
        );
        let flat = curve_path(400.0, 100.0, &[0.0; 10]);
        assert!(flat.starts_with("M0.00 50.00 L"), "{flat}");
        assert!(flat.ends_with("400.00 50.00"), "{flat}");
        let boosted = curve_path(400.0, 200.0, &[12.0; 10]);
        assert!(boosted.starts_with("M0.00 10.00"), "{boosted}");
        assert_eq!(curve_path(0.0, 100.0, &[0.0; 10]), "");
        // The curve passes through each band's point.
        let points = [3.0, -2.0, 5.0, 0.0, 1.0, -4.0, 6.0, 2.0, -1.0, 0.0]
            .map(|db: f32| 50.0 - db / 12.0 * 45.0);
        let curve = spline(&points, 10);
        for (a, b) in curve.iter().zip(points) {
            assert!((a - b).abs() < 1e-3);
        }
    }
}
