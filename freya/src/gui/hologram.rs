//! The hologram: a woman projected in translucent light over a city in the rain, in the style
//! of Blade Runner 2049 (see U7 in PLAN.md), drawn by the graphics card in a layer of Freya's
//! own on top of the skin window.
//!
//! A thread of its own draws it in step with the display: Slint and the app's main thread take
//! no part, and the CPU only hands each frame the time, the music's bass and which of her poses
//! to show. Her poses are photographs built into Freya from assets/hologram/. The skin marks the
//! place with `NativeHologram`; where the system has no such layer yet, nothing is drawn.

use slint_interpreter::Value;

use super::skin_window::SkinWindow;

#[cfg(target_os = "macos")]
#[path = "hologram/metal.rs"]
mod platform;
#[cfg(not(target_os = "macos"))]
#[path = "hologram/other.rs"]
mod platform;

mod poses {
    include!(concat!(env!("OUT_DIR"), "/hologram_poses.rs"));
}

/// Seconds she holds a pose, and seconds the glitch to the next one takes.
pub const POSE_SECONDS: f32 = 9.0;
pub const TRANSITION_SECONDS: f32 = 1.1;

/// Where the skin wants the hologram, in logical pixels from the window's top left.
#[derive(Clone, Debug, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// The rounding of the box the hologram fills, so its corners match the skin's.
    pub corner_radius: f64,
}

impl Area {
    /// The area the skin's `NativeHologram` reported, if it is shown.
    pub fn read(ui: &SkinWindow) -> Option<Self> {
        let Ok(Value::Struct(area)) = ui.get_global_property("Visualizer", "hologram-area") else {
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

/// Which poses to show `seconds` in: the one she holds, the next, and how far the glitch from
/// one to the other has come, from 0 to 1.
pub fn poses_at(seconds: f32, count: usize) -> (usize, usize, f32) {
    // With one pose there is nothing to change to.
    if count <= 1 {
        return (0, 0, 0.0);
    }
    let held = (seconds.max(0.0) / POSE_SECONDS) as usize;
    let into = seconds.max(0.0) % POSE_SECONDS;
    let current = held % count;
    let next = (held + 1) % count;
    let transition = ((into - (POSE_SECONDS - TRANSITION_SECONDS)) / TRANSITION_SECONDS).max(0.0);
    (current, next, transition.min(1.0))
}

/// The hologram's layer in one window, and the thread that draws it.
pub struct Hologram {
    renderer: platform::Renderer,
}

impl Hologram {
    /// Whether this system can show the hologram.
    pub const SUPPORTED: bool = platform::SUPPORTED;

    /// Adds the layer to `window`, hidden. `None` when the window is not open yet or the
    /// graphics card cannot be used.
    pub fn new(window: &slint::Window) -> Option<Self> {
        let renderer = platform::Renderer::new(window, poses::POSES)?;
        log::info!(
            "the hologram is drawn by the graphics card ({}), {} poses",
            platform::NAME,
            poses::POSES.len()
        );
        Some(Self { renderer })
    }

    /// Shows the hologram in `area` and lets it run, or hides it and lets its thread sleep.
    pub fn place(&mut self, area: Option<&Area>) {
        self.renderer.place(area);
    }

    /// How strongly the bass plays right now, from 0 to 1: it drives the glitches and glow.
    pub fn set_pulse(&self, pulse: f32) {
        self.renderer.set_pulse(pulse.clamp(0.0, 1.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_each_pose_then_glitches_to_the_next() {
        assert_eq!(poses_at(0.0, 5), (0, 1, 0.0));
        assert_eq!(
            poses_at(POSE_SECONDS - TRANSITION_SECONDS - 0.1, 5),
            (0, 1, 0.0)
        );
        let (current, next, transition) = poses_at(POSE_SECONDS - TRANSITION_SECONDS / 2.0, 5);
        assert_eq!((current, next), (0, 1));
        assert!((transition - 0.5).abs() < 0.01);
        assert_eq!(poses_at(POSE_SECONDS, 5), (1, 2, 0.0));
        assert_eq!(poses_at(5.0 * POSE_SECONDS, 5).0, 0, "round again");
        assert_eq!(poses_at(3.0, 0), (0, 0, 0.0));
        assert_eq!(
            poses_at(POSE_SECONDS - 0.5, 1),
            (0, 0, 0.0),
            "one pose never glitches"
        );
    }
}
