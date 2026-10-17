//! The demo: a girl dancing over copper bars, in the style of an old Amiga demo, drawn by the
//! graphics card in a layer of Freya's own on top of the skin window (see U7 in PLAN.md).
//!
//! A thread of its own draws it in step with the display: Slint and the app's main thread take
//! no part, and the CPU only hands each frame the time and the music's pulse. The skin marks
//! the place with `NativeDemo`; where the system has no such layer yet, nothing is drawn.

use slint_interpreter::Value;

use super::skin_window::SkinWindow;

#[cfg(target_os = "macos")]
#[path = "demo/metal.rs"]
mod platform;
#[cfg(not(target_os = "macos"))]
#[path = "demo/other.rs"]
mod platform;

/// Where the skin wants the demo, in logical pixels from the window's top left.
#[derive(Clone, Debug, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// The rounding of the box the demo fills, so its corners match the skin's.
    pub corner_radius: f64,
}

impl Area {
    /// The area the skin's `NativeDemo` reported, if it is shown.
    pub fn read(ui: &SkinWindow) -> Option<Self> {
        let Ok(Value::Struct(area)) = ui.get_global_property("Visualizer", "demo-area") else {
            return None;
        };
        let number = |name: &str| match area.get_field(name) {
            Some(Value::Number(n)) => *n,
            _ => 0.0,
        };
        let shown = matches!(area.get_field("shown"), Some(Value::Bool(true)));
        let area = Self {
            x: number("x"),
            y: number("y"),
            width: number("width"),
            height: number("height"),
            corner_radius: number("corner-radius"),
        };
        (shown && area.width > 0.0 && area.height > 0.0).then_some(area)
    }
}

/// The demo's layer in one window, and the thread that draws it.
pub struct Demo {
    renderer: platform::Renderer,
}

impl Demo {
    /// Whether this system can show the demo.
    pub const SUPPORTED: bool = platform::SUPPORTED;

    /// Adds the layer to `window`, hidden. `None` when the window is not open yet or the
    /// graphics card cannot be used.
    pub fn new(window: &slint::Window) -> Option<Self> {
        let renderer = platform::Renderer::new(window)?;
        log::info!(
            "the demo is drawn by the graphics card ({})",
            platform::NAME
        );
        Some(Self { renderer })
    }

    /// Shows the demo in `area` and lets it run, or hides it and lets its thread sleep.
    pub fn place(&mut self, area: Option<&Area>) {
        self.renderer.place(area);
    }

    /// How strongly the bass plays right now, from 0 to 1: the background pulses with it.
    pub fn set_pulse(&self, pulse: f32) {
        self.renderer.set_pulse(pulse.clamp(0.0, 1.0));
    }
}
