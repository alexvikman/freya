//! Systems the demo has not been ported to yet: Freya shows the spectrum there instead.

use super::Area;

pub const NAME: &str = "none";
pub const SUPPORTED: bool = false;

pub struct Renderer;

impl Renderer {
    pub fn new(_window: &slint::Window) -> Option<Self> {
        None
    }

    pub fn place(&mut self, _area: Option<&Area>) {}

    pub fn set_pulse(&self, _pulse: f32) {}
}
