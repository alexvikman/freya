//! The oscilloscope drawn by Freya itself, in a layer on top of the skin window.
//!
//! Drawn by Slint, the line costs a redraw of its whole box for every frame: with the software
//! renderer, two thirds of a core full screen. Here Freya only hands the system the line's
//! points, and the graphics card draws it. The skin marks the place with `NativeOscilloscope`;
//! where the system has no such layer yet, the skin draws the line itself.

use slint_interpreter::Value;

use super::skin_window::SkinWindow;

#[cfg(target_os = "macos")]
#[path = "native_oscilloscope/core_animation.rs"]
mod platform;
#[cfg(not(target_os = "macos"))]
#[path = "native_oscilloscope/other.rs"]
mod platform;

/// Where and how the skin wants the oscilloscope, in logical pixels from the window's top left.
#[derive(Clone, Debug, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// Red, green, blue and alpha from 0 to 1.
    pub color: [f64; 4],
    pub line_width: f64,
}

impl Area {
    /// The area the skin's `NativeOscilloscope` reported, if it is shown.
    pub fn read(ui: &SkinWindow) -> Option<Self> {
        let Ok(Value::Struct(area)) = ui.get_global_property("Visualizer", "oscilloscope-area")
        else {
            return None;
        };
        let number = |name: &str| match area.get_field(name) {
            Some(Value::Number(n)) => *n,
            _ => 0.0,
        };
        let color = match area.get_field("line-color") {
            Some(Value::Brush(brush)) => {
                let c = brush.color();
                [c.red(), c.green(), c.blue(), c.alpha()].map(|v| f64::from(v) / 255.0)
            }
            _ => [0.0; 4],
        };
        let shown = matches!(area.get_field("shown"), Some(Value::Bool(true)));
        let area = Self {
            x: number("x"),
            y: number("y"),
            width: number("width"),
            height: number("height"),
            color,
            line_width: number("line-width"),
        };
        (shown && area.width > 0.0 && area.height > 0.0).then_some(area)
    }
}

/// The line through `samples` in a `width` × `height` box, as `Visualizer.wave` draws it, with
/// y counted from the bottom: spread over the width, 90 % of the height, silence in the middle.
pub fn points(width: f64, height: f64, samples: &[f32]) -> Vec<(f64, f64)> {
    if samples.len() < 2 {
        return vec![(0.0, height / 2.0), (width, height / 2.0)];
    }
    let last = (samples.len() - 1) as f64;
    samples
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let x = i as f64 / last * width;
            let y = (0.5 + f64::from(v.clamp(-1.0, 1.0)) * 0.45) * height;
            (x, y)
        })
        .collect()
}

/// Freya's own oscilloscope layer in one window.
pub struct NativeOscilloscope {
    layer: platform::Layer,
    area: Option<Area>,
    /// The waveform last drawn, drawn again when the area changes: silence, a flat line, until
    /// something plays.
    samples: Vec<f32>,
}

impl NativeOscilloscope {
    /// Whether this system can have the oscilloscope drawn by Freya.
    pub const SUPPORTED: bool = platform::SUPPORTED;

    /// Adds the layer to `window`, hidden. `None` when the window is not open yet or the
    /// system does not allow it; the skin then draws the line itself.
    pub fn new(window: &slint::Window) -> Option<Self> {
        let layer = platform::Layer::new(window)?;
        log::info!("the oscilloscope is drawn by Freya ({})", platform::NAME);
        Some(Self {
            layer,
            area: None,
            samples: Vec::new(),
        })
    }

    /// Moves the oscilloscope to `area`, or hides it with `None`.
    pub fn place(&mut self, area: Option<Area>) {
        self.layer.place(area.as_ref());
        self.area = area;
        self.redraw();
    }

    /// Whether it is shown, so the skin needs no waveform.
    pub fn shown(&self) -> bool {
        self.area.is_some()
    }

    pub fn draw(&mut self, samples: &[f32]) {
        self.samples.clear();
        self.samples.extend_from_slice(samples);
        self.redraw();
    }

    fn redraw(&mut self) {
        if let Some(area) = &self.area {
            self.layer
                .draw(&points(area.width, area.height, &self.samples));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_matches_the_skin_path() {
        // `wave_path(100, 40, [1, 0, -1])` is "M0 2 L50 20 L100 38", y from the top.
        let line = points(100.0, 40.0, &[1.0, 0.0, -1.0]);
        let expected = [(0.0, 38.0), (50.0, 20.0), (100.0, 2.0)];
        assert_eq!(line.len(), expected.len());
        for ((x, y), (ex, ey)) in line.into_iter().zip(expected) {
            assert!((x - ex).abs() < 1e-9 && (y - ey).abs() < 1e-9, "{x},{y}");
        }
        assert_eq!(points(100.0, 40.0, &[]), [(0.0, 20.0), (100.0, 20.0)]);
    }
}
