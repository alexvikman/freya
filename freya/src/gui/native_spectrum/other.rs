//! No layer of our own yet on this system: the skin draws the spectrum.

use super::{Area, Rect};

pub const NAME: &str = "none";
pub const SUPPORTED: bool = false;

pub struct Layers;

impl Layers {
    pub fn new(_window: &slint::Window) -> Option<Self> {
        None
    }

    pub fn place(&mut self, _area: Option<&Area>, _scale: f32) {}

    /// Nothing to do: the system keeps the layer drawn.
    pub fn refresh(&mut self) {}

    pub fn draw(&mut self, _area: &Area, _bars: &[Rect], _peaks: &[Option<Rect>]) {}
}
