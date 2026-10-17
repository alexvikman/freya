//! Systems the oscilloscope layer has not been ported to yet: the skin draws the line.

use super::Area;

pub const NAME: &str = "none";
pub const SUPPORTED: bool = false;

pub struct Layer;

impl Layer {
    pub fn new(_window: &slint::Window) -> Option<Self> {
        None
    }

    pub fn place(&mut self, _area: Option<&Area>) {}

    pub fn draw(&mut self, _points: &[(f64, f64)]) {}
}
