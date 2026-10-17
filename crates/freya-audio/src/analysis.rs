//! Turning played samples into a spectrum and a waveform for visualization.

use std::f32::consts::PI;
use std::sync::Arc;
use std::time::Duration;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// FFT length. At 48 kHz this is 43 ms of audio and 23 Hz per bin.
const FFT_SIZE: usize = 2048;
const LOWEST_HZ: f32 = 40.0;
const HIGHEST_HZ: f32 = 16_000.0;
/// Levels at or below this many dBFS show as an empty bar; 0 dBFS is a full one.
const FLOOR_DB: f32 = -72.0;
/// How fast bars fall, in bar heights per second. They rise instantly.
const BAR_FALL: f32 = 1.6;
/// Peak markers stay put this long before falling at PEAK_FALL heights per second.
const PEAK_HOLD: Duration = Duration::from_millis(400);
const PEAK_FALL: f32 = 0.8;

/// Spectrum bars with peak markers, and a short waveform.
pub struct Analyzer {
    sample_rate: f32,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    /// The latest FFT_SIZE samples, oldest first.
    history: Vec<f32>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    bars: Vec<f32>,
    peaks: Vec<f32>,
    peak_age: Vec<Duration>,
}

impl Analyzer {
    pub fn new(sample_rate: u32) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let window = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / FFT_SIZE as f32).cos())
            .collect();
        Self {
            sample_rate: sample_rate as f32,
            input: fft.make_input_vec(),
            output: fft.make_output_vec(),
            fft,
            window,
            history: vec![0.0; FFT_SIZE],
            bars: Vec::new(),
            peaks: Vec::new(),
            peak_age: Vec::new(),
        }
    }

    /// Adds newly played mono samples.
    pub fn push(&mut self, samples: &[f32]) {
        if samples.len() >= FFT_SIZE {
            self.history
                .copy_from_slice(&samples[samples.len() - FFT_SIZE..]);
        } else {
            self.history.drain(..samples.len());
            self.history.extend_from_slice(samples);
        }
    }

    /// Lets bars and peaks fall towards silence, for when nothing is playing.
    pub fn silence(&mut self) {
        self.history.fill(0.0);
    }

    pub fn bars(&self) -> &[f32] {
        &self.bars
    }

    pub fn peaks(&self) -> &[f32] {
        &self.peaks
    }

    /// Whether every bar and peak has fallen to zero.
    pub fn is_settled(&self) -> bool {
        self.bars.iter().chain(&self.peaks).all(|&v| v == 0.0)
    }

    /// Recomputes `count` bars (heights 0 to 1, low to high frequency) after `dt` has passed.
    pub fn update(&mut self, count: usize, dt: Duration) {
        if self.bars.len() != count {
            self.bars = vec![0.0; count];
            self.peaks = vec![0.0; count];
            self.peak_age = vec![Duration::ZERO; count];
        }
        if count == 0 {
            return;
        }
        for ((input, sample), w) in self.input.iter_mut().zip(&self.history).zip(&self.window) {
            *input = sample * w;
        }
        if self.fft.process(&mut self.input, &mut self.output).is_err() {
            return;
        }

        // A full-scale sine has magnitude FFT_SIZE / 4 with a Hann window.
        let reference = FFT_SIZE as f32 / 4.0;
        let bin_hz = self.sample_rate / FFT_SIZE as f32;
        let top = HIGHEST_HZ.min(self.sample_rate / 2.0);
        let ratio = (top / LOWEST_HZ).powf(1.0 / count as f32);
        let secs = dt.as_secs_f32();
        for i in 0..count {
            let lo_hz = LOWEST_HZ * ratio.powi(i as i32);
            let hi_hz = lo_hz * ratio;
            let lo = ((lo_hz / bin_hz).floor() as usize).min(self.output.len() - 1);
            let hi = ((hi_hz / bin_hz).ceil() as usize).clamp(lo + 1, self.output.len());
            let magnitude = self.output[lo..hi]
                .iter()
                .map(|c| c.norm())
                .fold(0.0f32, f32::max);
            let db = 20.0 * (magnitude / reference).max(1e-9).log10();
            let level = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);

            let bar = &mut self.bars[i];
            *bar = level.max(*bar - BAR_FALL * secs).max(0.0);

            let (peak, age) = (&mut self.peaks[i], &mut self.peak_age[i]);
            if *bar >= *peak {
                *peak = *bar;
                *age = Duration::ZERO;
            } else {
                *age += dt;
                if *age > PEAK_HOLD {
                    *peak = (*peak - PEAK_FALL * secs).max(*bar);
                }
            }
        }
    }

    /// The latest samples, reduced to `points` values from -1 to 1.
    pub fn waveform(&self, points: usize) -> Vec<f32> {
        // About 10 ms of audio: a few periods of most notes.
        let span = ((self.sample_rate * 0.010) as usize).clamp(points.max(1), FFT_SIZE);
        let recent = &self.history[FFT_SIZE - span..];
        (0..points)
            .map(|i| {
                let index = i * (span - 1) / points.saturating_sub(1).max(1);
                recent[index.min(span - 1)].clamp(-1.0, 1.0)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, amplitude: f32, rate: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * PI * freq * i as f32 / rate as f32).sin() * amplitude)
            .collect()
    }

    #[test]
    fn sine_lights_its_band() {
        let mut a = Analyzer::new(48_000);
        a.push(&sine(1000.0, 0.5, 48_000, FFT_SIZE));
        a.update(32, Duration::from_millis(16));
        let bars = a.bars();
        let loudest = bars
            .iter()
            .enumerate()
            .max_by(|x, y| x.1.total_cmp(y.1))
            .unwrap()
            .0;
        // Bar edges are log-spaced from 40 Hz; find the bar that contains 1 kHz.
        let ratio = (16_000f32 / 40.0).powf(1.0 / 32.0);
        let expected = ((1000f32 / 40.0).ln() / ratio.ln()).floor() as usize;
        assert!(
            loudest.abs_diff(expected) <= 1,
            "loudest {loudest}, expected {expected}"
        );
        // -6 dBFS lands near the top.
        assert!(bars[loudest] > 0.85, "{}", bars[loudest]);
        // Far away from the tone, almost nothing.
        assert!(bars[0] < 0.2 && bars[31] < 0.2, "{bars:?}");
    }

    #[test]
    fn bars_fall_and_peaks_hold() {
        let mut a = Analyzer::new(48_000);
        a.push(&sine(200.0, 0.5, 48_000, FFT_SIZE));
        a.update(16, Duration::from_millis(16));
        let top = a.bars().iter().copied().fold(0.0, f32::max);
        a.silence();
        a.update(16, Duration::from_millis(100));
        let after = a.bars().iter().copied().fold(0.0, f32::max);
        assert!((top - after - 0.16).abs() < 0.01, "{top} -> {after}");
        // Peaks are still at the top during the hold time.
        assert_eq!(a.peaks().iter().copied().fold(0.0, f32::max), top);
        for _ in 0..200 {
            a.update(16, Duration::from_millis(16));
        }
        assert!(a.is_settled());
    }

    #[test]
    fn waveform_has_requested_points() {
        let mut a = Analyzer::new(48_000);
        a.push(&sine(480.0, 2.0, 48_000, FFT_SIZE));
        let wave = a.waveform(64);
        assert_eq!(wave.len(), 64);
        assert!(wave.iter().all(|v| (-1.0..=1.0).contains(v)));
        assert!(wave.iter().any(|&v| v > 0.9) && wave.iter().any(|&v| v < -0.9));
    }
}
