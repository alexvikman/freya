//! Equalizer presets: Winamp's built-in set and the `.eqf` preset file format.
//!
//! Values use Winamp's scale from 1 (-12 dB) to 64 (+12 dB).

/// Gains range from -MAX_DB to +MAX_DB.
const MAX_DB: f32 = 12.0;
const EQF_HEADER: &[u8] = b"Winamp EQ library file v1.1\x1a!--";
const EQF_NAME_LEN: usize = 257;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EqPreset {
    pub name: String,
    pub preamp: u8,
    pub bands: [u8; 10],
}

/// Converts a Winamp slider value (1..=64) to decibels. Values within a fifth of a decibel
/// of zero count as flat, since Winamp's scale has no exact middle.
pub fn value_to_db(value: u8) -> f32 {
    let db = ((f32::from(value.clamp(1, 64)) - 1.0) / 63.0 - 0.5) * 2.0 * MAX_DB;
    if db.abs() < 0.2 { 0.0 } else { db }
}

pub fn db_to_value(db: f32) -> u8 {
    ((db.clamp(-MAX_DB, MAX_DB) / (2.0 * MAX_DB) + 0.5) * 63.0 + 1.0).round() as u8
}

impl EqPreset {
    pub fn from_db(name: impl Into<String>, preamp_db: f32, bands_db: [f32; 10]) -> Self {
        Self {
            name: name.into(),
            preamp: db_to_value(preamp_db),
            bands: bands_db.map(db_to_value),
        }
    }

    pub fn preamp_db(&self) -> f32 {
        value_to_db(self.preamp)
    }

    pub fn bands_db(&self) -> [f32; 10] {
        self.bands.map(value_to_db)
    }
}

/// The presets that ship with Winamp.
pub fn builtin_presets() -> Vec<EqPreset> {
    const PRESETS: [(&str, [u8; 10]); 18] = [
        ("Flat", [33; 10]),
        ("Classical", [33, 33, 33, 33, 33, 33, 20, 20, 20, 16]),
        ("Club", [33, 33, 38, 42, 42, 42, 38, 33, 33, 33]),
        ("Dance", [48, 44, 36, 32, 32, 22, 20, 20, 32, 32]),
        ("Full Bass", [48, 48, 48, 42, 35, 25, 18, 15, 14, 14]),
        (
            "Full Bass & Treble",
            [44, 42, 33, 20, 24, 35, 46, 50, 52, 52],
        ),
        ("Full Treble", [16, 16, 16, 25, 37, 50, 58, 58, 58, 60]),
        (
            "Laptop speakers/headphones",
            [40, 50, 41, 26, 28, 35, 40, 48, 53, 56],
        ),
        ("Large hall", [49, 49, 42, 42, 33, 24, 24, 24, 33, 33]),
        ("Live", [24, 33, 39, 41, 42, 42, 39, 37, 37, 36]),
        ("Party", [44, 44, 33, 33, 33, 33, 33, 33, 44, 44]),
        ("Pop", [29, 40, 44, 45, 41, 30, 28, 28, 29, 29]),
        ("Reggae", [33, 33, 31, 22, 33, 43, 43, 33, 33, 33]),
        ("Rock", [45, 40, 23, 19, 26, 39, 47, 50, 50, 50]),
        ("Ska", [28, 24, 25, 31, 39, 42, 47, 48, 50, 48]),
        ("Soft", [40, 35, 30, 28, 30, 39, 46, 48, 50, 52]),
        ("Soft Rock", [39, 39, 36, 31, 25, 23, 26, 31, 37, 47]),
        ("Techno", [45, 42, 33, 23, 24, 33, 45, 48, 48, 47]),
    ];
    PRESETS
        .iter()
        .map(|&(name, bands)| EqPreset {
            name: name.into(),
            preamp: 33,
            bands,
        })
        .collect()
}

/// Parses a Winamp `.eqf` file, which can hold several presets.
pub fn read_eqf(bytes: &[u8]) -> Result<Vec<EqPreset>, String> {
    let mut rest = bytes
        .strip_prefix(EQF_HEADER)
        .ok_or("not a Winamp EQ preset file")?;
    let mut presets = Vec::new();
    while rest.len() >= EQF_NAME_LEN + 11 {
        let (name, tail) = rest.split_at(EQF_NAME_LEN);
        let name_len = name.iter().position(|&b| b == 0).unwrap_or(EQF_NAME_LEN);
        let name: String = name[..name_len].iter().map(|&b| b as char).collect();
        // Values are stored inverted: 0 is the top of the slider.
        let value = |b: u8| 64u8.saturating_sub(b).clamp(1, 64);
        presets.push(EqPreset {
            name,
            bands: std::array::from_fn(|i| value(tail[i])),
            preamp: value(tail[10]),
        });
        rest = &tail[11..];
    }
    if presets.is_empty() {
        return Err("no presets in file".into());
    }
    Ok(presets)
}

pub fn write_eqf(presets: &[EqPreset]) -> Vec<u8> {
    let mut out = EQF_HEADER.to_vec();
    for preset in presets {
        let mut name = [0u8; EQF_NAME_LEN];
        // Names are single-byte; anything else becomes '?'.
        for (slot, c) in name
            .iter_mut()
            .zip(preset.name.chars().take(EQF_NAME_LEN - 1))
        {
            *slot = u8::try_from(c).unwrap_or(b'?');
        }
        out.extend_from_slice(&name);
        out.extend(preset.bands.iter().map(|&v| 64 - v.clamp(1, 64)));
        out.push(64 - preset.preamp.clamp(1, 64));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_conversion() {
        assert_eq!(value_to_db(1), -12.0);
        assert_eq!(value_to_db(64), 12.0);
        assert_eq!(value_to_db(33), 0.0);
        assert_eq!(db_to_value(0.0), 33);
        for v in 1..=64 {
            assert_eq!(db_to_value(value_to_db(v)), if v == 32 { 33 } else { v });
        }
    }

    #[test]
    fn eqf_round_trip() {
        let presets = builtin_presets();
        let bytes = write_eqf(&presets);
        assert!(bytes.starts_with(b"Winamp EQ library file v1.1"));
        assert_eq!(read_eqf(&bytes).unwrap(), presets);
    }

    #[test]
    fn rejects_other_files() {
        assert!(read_eqf(b"RIFF....").is_err());
    }
}
