//! A 10-band graphic equalizer with Winamp's band layout.

use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Center frequencies of the bands, as in Winamp.
pub const BAND_FREQUENCIES: [f32; 10] = [
    60.0, 170.0, 310.0, 600.0, 1000.0, 3000.0, 6000.0, 12000.0, 14000.0, 16000.0,
];
/// Band and preamp gains range from -MAX_GAIN_DB to +MAX_GAIN_DB.
pub const MAX_GAIN_DB: f32 = 12.0;
/// Bandwidth of each band; roughly one octave.
const Q: f32 = 1.2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqSettings {
    pub enabled: bool,
    pub preamp_db: f32,
    pub bands_db: [f32; 10],
}

impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            preamp_db: 0.0,
            bands_db: [0.0; 10],
        }
    }
}

/// Equalizer settings shared lock-free with the audio callback.
pub(crate) struct SharedEq {
    enabled: AtomicBool,
    preamp: AtomicU32,
    bands: [AtomicU32; 10],
    version: AtomicU64,
}

impl SharedEq {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            preamp: AtomicU32::new(0f32.to_bits()),
            bands: std::array::from_fn(|_| AtomicU32::new(0f32.to_bits())),
            version: AtomicU64::new(0),
        }
    }

    pub fn set(&self, s: &EqSettings) {
        let clamp = |db: f32| db.clamp(-MAX_GAIN_DB, MAX_GAIN_DB).to_bits();
        self.enabled.store(s.enabled, Ordering::Relaxed);
        self.preamp.store(clamp(s.preamp_db), Ordering::Relaxed);
        for (slot, db) in self.bands.iter().zip(s.bands_db) {
            slot.store(clamp(db), Ordering::Relaxed);
        }
        self.version.fetch_add(1, Ordering::Release);
    }

    pub fn get(&self) -> EqSettings {
        EqSettings {
            enabled: self.enabled.load(Ordering::Relaxed),
            preamp_db: f32::from_bits(self.preamp.load(Ordering::Relaxed)),
            bands_db: std::array::from_fn(|i| {
                f32::from_bits(self.bands[i].load(Ordering::Relaxed))
            }),
        }
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    /// Peaking filter from the RBJ audio EQ cookbook.
    fn peaking(rate: f32, freq: f32, q: f32, gain_db: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq / rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha / a;
        Self {
            b0: (1.0 + alpha * a) / a0,
            b1: -2.0 * cos / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha / a) / a0,
        }
    }
}

/// Filter memory for one band of one channel (transposed direct form II).
#[derive(Clone, Copy, Default)]
struct BandState {
    z1: f32,
    z2: f32,
}

/// The equalizer as run in the audio callback. Never allocates after construction.
pub(crate) struct Equalizer {
    channels: usize,
    rate: f32,
    filters: [Option<Biquad>; 10],
    state: Vec<[BandState; 10]>,
    preamp: f32,
    enabled: bool,
}

impl Equalizer {
    pub fn new(rate: u32, channels: usize) -> Self {
        Self {
            channels,
            rate: rate as f32,
            filters: [None; 10],
            state: vec![[BandState::default(); 10]; channels],
            preamp: 1.0,
            enabled: false,
        }
    }

    pub fn configure(&mut self, s: &EqSettings) {
        if s.enabled && !self.enabled {
            // Stale filter memory from earlier playback would click.
            self.state
                .iter_mut()
                .for_each(|c| *c = [BandState::default(); 10]);
        }
        self.enabled = s.enabled;
        self.preamp = 10f32.powf(s.preamp_db / 20.0);
        for (i, filter) in self.filters.iter_mut().enumerate() {
            let freq = BAND_FREQUENCIES[i];
            // Flat bands cost nothing, and bands too close to Nyquist cannot be built.
            *filter = (s.bands_db[i].abs() > 0.01 && freq < self.rate * 0.45)
                .then(|| Biquad::peaking(self.rate, freq, Q, s.bands_db[i]));
        }
    }

    /// Filters interleaved samples in place.
    pub fn process(&mut self, samples: &mut [f32]) {
        if !self.enabled {
            return;
        }
        for frame in samples.chunks_exact_mut(self.channels) {
            for (sample, state) in frame.iter_mut().zip(self.state.iter_mut()) {
                let mut x = *sample * self.preamp;
                for (filter, s) in self.filters.iter().zip(state.iter_mut()) {
                    if let Some(f) = filter {
                        let y = f.b0 * x + s.z1;
                        s.z1 = f.b1 * x - f.a1 * y + s.z2;
                        s.z2 = f.b2 * x - f.a2 * y;
                        x = y;
                    }
                }
                *sample = x;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Gain in dB that the equalizer applies to a sine at `freq` (mono).
    fn measured_gain_db(settings: &EqSettings, freq: f32) -> f32 {
        let mut eq = Equalizer::new(RATE, 1);
        eq.configure(settings);
        let n = RATE as usize;
        let input: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * freq * i as f32 / RATE as f32).sin() * 0.1)
            .collect();
        let mut output = input.clone();
        eq.process(&mut output);
        // Skip the filter's settling time.
        let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
        20.0 * (rms(&output[n / 2..]) / rms(&input[n / 2..])).log10()
    }

    #[test]
    fn band_gain_is_applied_at_its_center() {
        let mut s = EqSettings {
            enabled: true,
            ..Default::default()
        };
        s.bands_db[4] = 6.0; // 1 kHz
        let gain = measured_gain_db(&s, 1000.0);
        assert!((gain - 6.0).abs() < 0.3, "{gain}");
        // Far away from the band nothing changes.
        assert!(measured_gain_db(&s, 60.0).abs() < 0.3);
    }

    #[test]
    fn preamp_and_disabled() {
        let s = EqSettings {
            enabled: true,
            preamp_db: -6.0,
            ..Default::default()
        };
        assert!((measured_gain_db(&s, 440.0) + 6.0).abs() < 0.1);
        let off = EqSettings {
            enabled: false,
            ..s
        };
        assert!(measured_gain_db(&off, 440.0).abs() < 0.01);
    }

    #[test]
    fn shared_settings_round_trip_and_clamp() {
        let shared = SharedEq::new();
        let mut s = EqSettings {
            enabled: true,
            preamp_db: 20.0,
            ..Default::default()
        };
        s.bands_db[9] = -3.0;
        let before = shared.version();
        shared.set(&s);
        assert!(shared.version() > before);
        let got = shared.get();
        assert_eq!(got.preamp_db, MAX_GAIN_DB);
        assert_eq!(got.bands_db[9], -3.0);
    }
}
