//! The spectrum drawn by Freya itself, in a layer on top of the skin window.
//!
//! Every frame Slint draws redraws the whole window, which costs far more than computing the
//! spectrum. Where the system lets us add our own layer to the window (macOS: Core Animation;
//! Windows: Windows.UI.Composition; Linux: a Wayland subsurface or an X11 child window), the
//! bars are drawn there each frame, and the window is not redrawn at all.
//! The skin marks the place with `NativeSpectrum`; elsewhere that component draws the
//! spectrum with Slint as before.

use slint_interpreter::Value;

use super::skin_window::SkinWindow;

/// Where and how the skin wants the spectrum, in logical pixels from the window's top left.
#[derive(Clone, Debug, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub shown: bool,
    /// Red, green, blue and alpha from 0 to 1.
    pub bar_color: [f64; 4],
    pub peak_color: [f64; 4],
    pub peak_thickness: f64,
    /// Behind the bars; only used where the layer cannot be see-through.
    pub background: [f64; 4],
}

impl Area {
    /// The area the skin's `NativeSpectrum` reported, if it has one.
    pub fn read(ui: &SkinWindow) -> Option<Self> {
        let Ok(Value::Struct(area)) = ui.get_global_property("Visualizer", "native-area") else {
            return None;
        };
        let number = |name: &str| match area.get_field(name) {
            Some(Value::Number(n)) => *n,
            _ => 0.0,
        };
        let color = |name: &str| match area.get_field(name) {
            Some(Value::Brush(brush)) => {
                let c = brush.color();
                [c.red(), c.green(), c.blue(), c.alpha()].map(|v| f64::from(v) / 255.0)
            }
            _ => [0.0; 4],
        };
        let area = Self {
            x: number("x"),
            y: number("y"),
            width: number("width"),
            height: number("height"),
            shown: matches!(area.get_field("shown"), Some(Value::Bool(true))),
            bar_color: color("bar-color"),
            peak_color: color("peak-color"),
            peak_thickness: number("peak-thickness"),
            background: color("background"),
        };
        (area.width > 0.0 && area.height > 0.0).then_some(area)
    }
}

/// One bar or peak marker, in logical pixels from the area's bottom left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub bottom: f64,
    pub width: f64,
    pub height: f64,
}

/// The bars and peak markers for `values`, laid out like `bars-path` and `peaks-path`.
/// Peaks near zero are `None`, so their markers are hidden.
pub fn layout(area: &Area, bars: &[f32], peaks: &[f32]) -> (Vec<Rect>, Vec<Option<Rect>>) {
    let n = bars.len().max(1) as f64;
    let slot = area.width / n;
    let bar_width = (slot * 0.75).max(1.0);
    let level = |v: f32| f64::from(v.clamp(0.0, 1.0)) * area.height;
    let bars = bars
        .iter()
        .enumerate()
        .map(|(i, &v)| Rect {
            x: i as f64 * slot,
            bottom: 0.0,
            width: bar_width,
            height: level(v).max(1.0),
        })
        .collect();
    let peaks = peaks
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            (v > 0.02).then(|| Rect {
                x: i as f64 * slot,
                bottom: (level(v) + 1.0).min(area.height - area.peak_thickness),
                width: bar_width,
                height: area.peak_thickness,
            })
        })
        .collect();
    (bars, peaks)
}

/// Freya's own spectrum layer in one window.
pub struct NativeSpectrum {
    layers: platform::Layers,
    area: Option<Area>,
}

impl NativeSpectrum {
    /// Whether this system can have the spectrum drawn by Freya.
    pub const SUPPORTED: bool = platform::SUPPORTED;

    /// Adds the layer to `window`. `None` when the window is not open yet, the system does not
    /// allow it, or `FREYA_NATIVE_SPECTRUM=off` is set; the skin then draws the spectrum itself.
    pub fn new(window: &slint::Window) -> Option<Self> {
        if std::env::var("FREYA_NATIVE_SPECTRUM").is_ok_and(|v| v == "off") {
            return None;
        }
        let layers = platform::Layers::new(window)?;
        log::info!("the spectrum is drawn by Freya ({})", platform::NAME);
        Some(Self { layers, area: None })
    }

    /// Moves the spectrum to `area`, or hides it with `None`. `scale` is the window's scale
    /// factor, which changes when the window moves to another screen.
    pub fn place(&mut self, area: Option<Area>, scale: f32) {
        let area = area.filter(|a| a.shown);
        self.layers.place(area.as_ref(), scale);
        self.area = area;
    }

    /// Handles what the system asks of the layer between frames, like redrawing it after
    /// another window uncovered it. Called on every app tick.
    pub fn refresh(&mut self) {
        self.layers.refresh();
    }

    pub fn draw(&mut self, bars: &[f32], peaks: &[f32]) {
        if let Some(area) = &self.area {
            let (bars, peaks) = layout(area, bars, peaks);
            self.layers.draw(area, &bars, &peaks);
        }
    }
}

#[cfg(windows)]
mod composition;
#[cfg(target_os = "macos")]
mod core_animation;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod other;

#[cfg(windows)]
use composition as platform;
#[cfg(target_os = "macos")]
use core_animation as platform;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
use other as platform;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_and_peaks_match_the_skin_paths() {
        let area = Area {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 40.0,
            shown: true,
            bar_color: [1.0; 4],
            peak_color: [1.0; 4],
            peak_thickness: 2.0,
            background: [1.0; 4],
        };
        let (bars, peaks) = layout(&area, &[0.5, 0.0], &[0.5, 0.0]);
        assert_eq!(
            bars,
            [
                Rect {
                    x: 0.0,
                    bottom: 0.0,
                    width: 37.5,
                    height: 20.0
                },
                Rect {
                    x: 50.0,
                    bottom: 0.0,
                    width: 37.5,
                    height: 1.0
                },
            ]
        );
        // The same place as `peaks-path`: 1 px above the bar, counted from the bottom.
        assert_eq!(
            peaks,
            [
                Some(Rect {
                    x: 0.0,
                    bottom: 21.0,
                    width: 37.5,
                    height: 2.0
                }),
                None
            ]
        );
    }
}
