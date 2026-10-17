//! Feeding the skin's visualization: spectrum bars, peaks and the oscilloscope waveform.

use std::time::Duration;

use freya_audio::Player;
use freya_audio::analysis::Analyzer;
use slint::{ModelRc, VecModel};
use slint_interpreter::Value;

use super::native_spectrum::{Area, NativeSpectrum};
use super::skin_state;
use super::skin_window::SkinWindow;

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
const MODES: [&str; 3] = ["spectrum", "oscilloscope", "off"];

pub fn is_mode(mode: &str) -> bool {
    MODES.contains(&mode)
}

/// The mode after `mode`, in the order spectrum, oscilloscope, off.
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
    pub fn update(&mut self, ui: &SkinWindow, player: &Player, playing: bool, mode: &str) {
        if !self.available {
            return;
        }
        if self.mode_shown.as_deref() != Some(mode) {
            let _ = ui.set_global_property("Visualizer", "mode", skin_state::string(mode));
            self.mode_shown = Some(mode.to_owned());
        }
        self.update_native(ui, mode);
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
        analyzer.update(count, dt);
        match &mut self.native {
            // Freya draws the spectrum: the skin's window is not touched.
            Some(native) => native.draw(analyzer.bars(), analyzer.peaks()),
            None => {
                publish(ui, "bars", analyzer.bars(), &mut self.bars_shown);
                publish(ui, "peaks", analyzer.peaks(), &mut self.peaks_shown);
            }
        }

        if mode == "oscilloscope" {
            let wave: Vec<Value> = analyzer
                .waveform(WAVE_POINTS)
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
        assert_eq!(next_mode("oscilloscope"), "off");
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
