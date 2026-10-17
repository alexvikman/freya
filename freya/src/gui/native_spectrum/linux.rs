//! Linux: a Wayland subsurface or an X11 child window, whichever the player's window uses.

mod wayland;
mod x11;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;

use super::{Area, Rect};

pub const NAME: &str = "a Wayland subsurface or an X11 child window";
pub const SUPPORTED: bool = true;

/// Errors from the window system, only ever logged.
type Error = Box<dyn std::error::Error>;

pub enum Layers {
    Wayland(wayland::Layer),
    X11(x11::Layer),
}

impl Layers {
    pub fn new(window: &slint::Window) -> Option<Self> {
        let (window_handle, display_handle) = window
            .with_winit_window(|w| {
                Some((
                    w.window_handle().ok()?.as_raw(),
                    w.display_handle().ok()?.as_raw(),
                ))
            })
            .flatten()?;
        let layers = match (window_handle, display_handle) {
            (RawWindowHandle::Wayland(w), RawDisplayHandle::Wayland(d)) => {
                wayland::Layer::new(d.display, w.surface).map(Self::Wayland)
            }
            (RawWindowHandle::Xlib(w), _) => x11::Layer::new(w.window as u32).map(Self::X11),
            (RawWindowHandle::Xcb(w), _) => x11::Layer::new(w.window.get()).map(Self::X11),
            _ => return None,
        };
        layers
            .inspect_err(|e| log::warn!("no native spectrum: {e}"))
            .ok()
    }

    pub fn place(&mut self, area: Option<&Area>, scale: f32) {
        let result = match self {
            Self::Wayland(layer) => layer.place(area, scale),
            Self::X11(layer) => layer.place(area, scale),
        };
        log_error(result);
    }

    pub fn refresh(&mut self) {
        let result = match self {
            Self::Wayland(layer) => layer.refresh(),
            Self::X11(layer) => layer.refresh(),
        };
        log_error(result);
    }

    pub fn draw(&mut self, area: &Area, bars: &[Rect], peaks: &[Option<Rect>]) {
        let result = match self {
            Self::Wayland(layer) => layer.draw(area, bars, peaks),
            Self::X11(layer) => layer.draw(area, bars, peaks),
        };
        log_error(result);
    }
}

fn log_error(result: Result<(), Error>) {
    if let Err(e) = result {
        log::debug!("native spectrum: {e}");
    }
}

/// A rectangle in physical pixels from the area's top left, for drawing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pixels {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

/// `r` (logical pixels from the area's bottom) in physical pixels from its top.
fn pixels(area: &Area, r: &Rect, scale: f64) -> Pixels {
    let top = area.height - r.bottom - r.height;
    let x0 = (r.x * scale).round() as i32;
    let y0 = (top * scale).round() as i32;
    let x1 = ((r.x + r.width) * scale).round() as i32;
    let y1 = ((top + r.height) * scale).round() as i32;
    Pixels {
        x: x0,
        y: y0,
        width: (x1 - x0).max(1) as u32,
        height: (y1 - y0).max(1) as u32,
    }
}

/// The area's size in physical pixels.
fn area_size(area: &Area, scale: f64) -> (u32, u32) {
    (
        ((area.width * scale).round() as u32).max(1),
        ((area.height * scale).round() as u32).max(1),
    )
}

/// `color` drawn over `background`, as 0–255 values: for surfaces that cannot be
/// see-through.
fn blend(color: [f64; 4], background: [f64; 4]) -> [u8; 3] {
    let a = color[3].clamp(0.0, 1.0);
    let mix = |i: usize| ((color[i] * a + background[i] * (1.0 - a)) * 255.0).round() as u8;
    [mix(0), mix(1), mix(2)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rects_become_pixels_from_the_top() {
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
        let bar = Rect {
            x: 10.0,
            bottom: 0.0,
            width: 7.5,
            height: 20.0,
        };
        assert_eq!(
            pixels(&area, &bar, 2.0),
            Pixels {
                x: 20,
                y: 40,
                width: 15,
                height: 40
            }
        );
        assert_eq!(area_size(&area, 1.5), (150, 60));
        assert_eq!(
            blend([1.0, 0.0, 0.0, 0.5], [0.0, 0.0, 1.0, 1.0]),
            [128, 0, 128]
        );
    }
}
