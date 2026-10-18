//! Systems the hologram has not been ported to yet: the skin says so in its box.

use super::Area;

pub const NAME: &str = "none";
pub const SUPPORTED: bool = false;

pub struct Renderer;

impl Renderer {
    pub fn new(_window: &slint::Window, _poses: &'static [&'static [u8]]) -> Option<Self> {
        None
    }

    pub fn failed(&self) -> bool {
        true
    }

    pub fn place(&mut self, _area: Option<&Area>) {}

    pub fn set_pulse(&self, _pulse: f32) {}
}
