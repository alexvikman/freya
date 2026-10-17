//! Feeding the skin's visualization: spectrum bars, peaks and the oscilloscope waveform.

use std::time::Duration;

use freya_audio::Player;
use freya_audio::analysis::Analyzer;
use slint::{ModelRc, VecModel};
use slint_interpreter::Value;

use super::hologram::{self, Hologram};
use super::native_oscilloscope::{self, NativeOscilloscope};
use super::native_spectrum::{Area, NativeSpectrum};
use super::skin_state;
use super::skin_window::SkinWindow;
use slint::winit_030::WinitWindowAccessor;

/// Allowed visualization frame rates.
const FPS_RANGE: std::ops::RangeInclusive<u32> = 10..=120;

/// Time between visualization frames at `fps` frames per second.
pub fn frame_interval(fps: u32) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(fps.clamp(*FPS_RANGE.start(), *FPS_RANGE.end())))
}
/// Frame rate while another app has focus: the player is only glanced at, and every frame
/// redraws the whole window. Measured on an M-series MacBook: 11 % CPU at 30 fps, 8 % at 15.
const BACKGROUND_FPS: u32 = 15;

/// Frames per second to draw, or `None` when there is nothing to draw: nothing is `active`,
/// or the window is hidden. Unfocused windows are drawn at most at [`BACKGROUND_FPS`].
pub fn frame_rate(fps: u32, active: bool, hidden: bool, focused: bool) -> Option<u32> {
    if !active || hidden {
        return None;
    }
    Some(if focused {
        fps
    } else {
        fps.min(BACKGROUND_FPS)
    })
}

/// Points in the oscilloscope waveform.
const WAVE_POINTS: usize = 128;
/// Skins may ask for at most this many bars.
const MAX_BARS: usize = 256;
const MODES: [&str; 4] = ["spectrum", "oscilloscope", "hologram", "off"];
/// The hologram pulses with the strongest of the lowest bars: this share of them,
/// on the spectrum's logarithmic scale up to about 150 Hz.
const BASS_SHARE: usize = 8;
/// At least this many bars are computed, so the hologram has a bass even without a spectrum.
const MIN_BARS: usize = 16;

pub fn is_mode(mode: &str) -> bool {
    MODES.contains(&mode)
}

/// The mode after `mode`, in the order spectrum, oscilloscope, hologram, off.
pub fn next_mode(mode: &str) -> &'static str {
    let i = MODES
        .iter()
        .position(|m| *m == mode)
        .unwrap_or(MODES.len() - 1);
    MODES[(i + 1) % MODES.len()]
}

pub struct Visualizer {
    /// Whether the skin exports the `Visualizer` global.
    available: bool,
    analyzer: Option<Analyzer>,
    samples: Vec<f32>,
    /// What the skin was last given, so unchanged frames are not redrawn.
    bars_shown: Vec<f32>,
    peaks_shown: Vec<f32>,
    /// The spectrum layer Freya draws itself, where the system allows it, and the area and
    /// window size it was last placed for.
    native: Option<NativeSpectrum>,
    native_placed: Option<(Option<Area>, slint::PhysicalSize)>,
    mode_shown: Option<String>,
    /// The oscilloscope layer Freya draws itself, where the system allows it, and the area
    /// and window size it was last placed for.
    oscilloscope: Option<NativeOscilloscope>,
    oscilloscope_placed: Option<(Option<native_oscilloscope::Area>, slint::PhysicalSize)>,
    /// The hologram, drawn by the graphics card where the system allows it, and where it was
    /// last placed.
    hologram: Option<Hologram>,
    hologram_placed: Option<Option<hologram::Area>>,
    /// Whether the system cannot show the hologram, so it is not tried again.
    hologram_unavailable: bool,
    /// Whether the audio callback is copying samples for us.
    tapping: bool,
    /// Something is playing and should be shown.
    wanted: bool,
    /// Bars are still falling after playback stopped.
    settling: bool,
}

impl Visualizer {
    pub fn new(ui: &SkinWindow) -> Self {
        let available = ui.has_global("Visualizer");
        Self {
            available,
            analyzer: None,
            samples: Vec::new(),
            bars_shown: Vec::new(),
            peaks_shown: Vec::new(),
            native: None,
            native_placed: None,
            mode_shown: None,
            oscilloscope: None,
            oscilloscope_placed: None,
            hologram: None,
            hologram_placed: None,
            hologram_unavailable: false,
            tapping: false,
            wanted: false,
            settling: false,
        }
    }

    /// Whether frames are still needed, including bars falling after playback stopped.
    pub fn active(&self) -> bool {
        self.wanted || self.settling
    }

    /// Called on every app tick: shows the mode and decides whether frames are needed.
    /// `window_shown` is false while the window is hidden, and the hologram then rests.
    pub fn update(
        &mut self,
        ui: &SkinWindow,
        player: &Player,
        playing: bool,
        mode: &str,
        window_shown: bool,
    ) {
        if !self.available {
            return;
        }
        if self.mode_shown.as_deref() != Some(mode) {
            let _ = ui.set_global_property("Visualizer", "mode", skin_state::string(mode));
            self.mode_shown = Some(mode.to_owned());
        }
        self.update_native(ui, mode);
        self.update_oscilloscope(ui, mode);
        self.update_hologram(ui, mode, window_shown);
        // No frames are computed while nothing plays: the hologram's pulse fades out instead.
        if !playing && let Some(hologram) = &self.hologram {
            hologram.set_pulse(0.0);
        }
        let showing = !matches!(
            ui.get_global_property("Visualizer", "showing"),
            Ok(Value::Bool(false))
        );
        let want = playing && showing && mode != "off";
        if want != self.tapping {
            player.set_visualization(want);
            self.tapping = want;
        }
        self.wanted = want;
    }

    /// Adds Freya's own spectrum layer once the window is open, and keeps it where the skin's
    /// `NativeSpectrum` is.
    fn update_native(&mut self, ui: &SkinWindow, mode: &str) {
        if !NativeSpectrum::SUPPORTED {
            return;
        }
        if self.native.is_none() {
            self.native = NativeSpectrum::new(ui.window());
            if self.native.is_none() {
                return;
            }
            let _ = ui.set_global_property("Visualizer", "native-drawing", Value::Bool(true));
        }
        if let Some(native) = &mut self.native {
            native.refresh();
        }
        let area = Area::read(ui).filter(|_| mode == "spectrum");
        let placed = Some((area.clone(), ui.window().size()));
        if self.native_placed != placed
            && let Some(native) = &mut self.native
        {
            native.place(area, ui.window().scale_factor());
            self.native_placed = placed;
        }
    }

    /// Adds Freya's own oscilloscope layer once the window is open, and keeps it where the
    /// skin's `NativeOscilloscope` is.
    fn update_oscilloscope(&mut self, ui: &SkinWindow, mode: &str) {
        if !NativeOscilloscope::SUPPORTED {
            return;
        }
        if self.oscilloscope.is_none() {
            if !ui.window().has_winit_window() {
                return;
            }
            self.oscilloscope = NativeOscilloscope::new(ui.window());
            if self.oscilloscope.is_none() {
                return;
            }
            let _ = ui.set_global_property("Visualizer", "oscilloscope-drawing", Value::Bool(true));
        }
        let area = native_oscilloscope::Area::read(ui).filter(|_| mode == "oscilloscope");
        let placed = Some((area.clone(), ui.window().size()));
        if self.oscilloscope_placed != placed
            && let Some(oscilloscope) = &mut self.oscilloscope
        {
            oscilloscope.place(area);
            self.oscilloscope_placed = placed;
        }
    }

    /// Starts the hologram once the window is open, and keeps it where the skin's `NativeHologram` is,
    /// hidden while the hologram is not shown or the window is hidden.
    fn update_hologram(&mut self, ui: &SkinWindow, mode: &str, window_shown: bool) {
        if !Hologram::SUPPORTED || self.hologram_unavailable {
            return;
        }
        let area = hologram::Area::read(ui).filter(|_| mode == "hologram" && window_shown);
        if self.hologram.is_none() {
            // Started as soon as the mode is chosen, even before its view is shown: setting it
            // up takes moments, which then pass out of sight. The window must be open first.
            if mode != "hologram" || !ui.window().has_winit_window() {
                return;
            }
            self.hologram = Hologram::new(ui.window());
            if self.hologram.is_none() {
                self.hologram_unavailable = true;
                return;
            }
            let _ = ui.set_global_property("Visualizer", "hologram-drawing", Value::Bool(true));
        }
        if self.hologram_placed.as_ref() != Some(&area)
            && let Some(hologram) = &mut self.hologram
        {
            hologram.place(area.as_ref());
            self.hologram_placed = Some(area);
        }
    }

    /// Draws one frame.
    pub fn frame(
        &mut self,
        ui: &SkinWindow,
        player: &Player,
        playing: bool,
        mode: &str,
        dt: Duration,
    ) {
        if !self.available {
            return;
        }
        let analyzer = self
            .analyzer
            .get_or_insert_with(|| Analyzer::new(player.sample_rate()));
        self.samples.clear();
        player.visualization_samples(&mut self.samples);
        if !playing || mode == "off" {
            analyzer.silence();
        } else if !self.samples.is_empty() {
            analyzer.push(&self.samples);
        }

        let count = match ui.get_global_property("Visualizer", "bar-count") {
            Ok(Value::Number(n)) => (n.max(0.0) as usize).min(MAX_BARS),
            _ => 0,
        };
        // The hologram's pulse needs the lowest bars even when the skin draws none.
        analyzer.update(count.max(MIN_BARS), dt);
        if let Some(hologram) = &self.hologram {
            let bars = analyzer.bars();
            let low = &bars[..(bars.len() / BASS_SHARE).max(2).min(bars.len())];
            hologram.set_pulse(low.iter().copied().fold(0.0, f32::max));
        }
        match &mut self.native {
            // Freya draws the spectrum: the skin's window is not touched.
            Some(native) => native.draw(analyzer.bars(), analyzer.peaks()),
            None => {
                publish(ui, "bars", analyzer.bars(), &mut self.bars_shown);
                publish(ui, "peaks", analyzer.peaks(), &mut self.peaks_shown);
            }
        }

        if mode == "oscilloscope" {
            let wave = analyzer.waveform(WAVE_POINTS);
            // Freya draws the line: the skin's window is not touched.
            if let Some(oscilloscope) = self.oscilloscope.as_mut().filter(|o| o.shown()) {
                oscilloscope.draw(&wave);
                self.settling = !analyzer.is_settled();
                return;
            }
            let wave: Vec<Value> = wave
                .into_iter()
                .map(|v| Value::Number(f64::from(v)))
                .collect();
            // A new model each frame, so the skin's path binding is re-evaluated.
            let _ = ui.set_global_property(
                "Visualizer",
                "waveform",
                Value::Model(ModelRc::new(VecModel::from(wave))),
            );
        }
        self.settling = !analyzer.is_settled();
    }
}

/// Gives the skin new values, as a new list so that bindings reading the whole list (like
/// `bars-path`) are evaluated again. Nothing happens when the values did not change, so a
/// still spectrum costs no redraws.
fn publish(ui: &SkinWindow, name: &str, values: &[f32], shown: &mut Vec<f32>) {
    if shown.as_slice() == values {
        return;
    }
    shown.clear();
    shown.extend_from_slice(values);
    let list: Vec<Value> = values
        .iter()
        .map(|&v| Value::Number(f64::from(v)))
        .collect();
    let _ = ui.set_global_property(
        "Visualizer",
        name,
        Value::Model(ModelRc::new(VecModel::from(list))),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_cycle() {
        assert_eq!(next_mode("spectrum"), "oscilloscope");
        assert_eq!(next_mode("oscilloscope"), "hologram");
        assert_eq!(next_mode("hologram"), "off");
        assert_eq!(next_mode("off"), "spectrum");
        assert_eq!(next_mode("nonsense"), "spectrum");
    }

    #[test]
    fn frames_stop_when_hidden_and_slow_down_unfocused() {
        assert_eq!(frame_rate(30, true, false, true), Some(30));
        assert_eq!(frame_rate(30, true, false, false), Some(BACKGROUND_FPS));
        assert_eq!(frame_rate(10, true, false, false), Some(10));
        assert_eq!(frame_rate(30, true, true, true), None);
        assert_eq!(frame_rate(30, false, false, true), None);
    }
}
