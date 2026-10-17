//! Core Animation: a layer added to the window's view, with one sublayer per bar and
//! peak marker. Moving layers costs almost nothing; the system composites them.

use objc2::rc::Retained;
use objc2_app_kit::NSView;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGColor;
use objc2_quartz_core::{CALayer, CATransaction};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;

use super::{Area, Rect};

pub const NAME: &str = "Core Animation";
pub const SUPPORTED: bool = true;

pub struct Layers {
    /// The window view's own layer, which the spectrum is added to.
    parent: Retained<CALayer>,
    root: Retained<CALayer>,
    bars: Vec<Retained<CALayer>>,
    peaks: Vec<Retained<CALayer>>,
    colors: Option<([f64; 4], [f64; 4])>,
}

impl Layers {
    pub fn new(window: &slint::Window) -> Option<Self> {
        let view = window
            .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
                RawWindowHandle::AppKit(handle) => Some(handle.ns_view),
                _ => None,
            })
            .flatten()?;
        let view: Retained<NSView> = retain_view(view.as_ptr())?;
        // Only add to views that already have a layer; giving one to a view that draws
        // with OpenGL could change how it draws.
        let parent = view.layer()?;
        let root = CALayer::new();
        root.setHidden(true);
        parent.addSublayer(&root);
        Some(Self {
            parent,
            root,
            bars: Vec::new(),
            peaks: Vec::new(),
            colors: None,
        })
    }

    /// Core Animation works in points, like Slint's logical pixels, so `_scale` is unused.
    pub fn place(&mut self, area: Option<&Area>, _scale: f32) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        match area {
            None => self.root.setHidden(true),
            Some(area) => {
                // The view's layer may count y from the top or from the bottom. The root
                // flips the same way, so inside it y always counts from the bottom.
                let flipped = self.parent.isGeometryFlipped();
                let parent_height = self.parent.bounds().size.height;
                let y = if flipped {
                    area.y
                } else {
                    parent_height - area.y - area.height
                };
                self.root.setGeometryFlipped(flipped);
                self.root.setFrame(rect(area.x, y, area.width, area.height));
                self.root.setHidden(false);
                let colors = (area.bar_color, area.peak_color);
                if self.colors != Some(colors) {
                    self.colors = Some(colors);
                    recolor(&self.bars, area.bar_color);
                    recolor(&self.peaks, area.peak_color);
                }
            }
        }
        CATransaction::commit();
    }

    /// Nothing to do: the system keeps the layer drawn.
    pub fn refresh(&mut self) {}

    pub fn draw(&mut self, area: &Area, bars: &[Rect], peaks: &[Option<Rect>]) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        resize(&self.root, &mut self.bars, bars.len(), area.bar_color);
        resize(&self.root, &mut self.peaks, peaks.len(), area.peak_color);
        for (layer, r) in self.bars.iter().zip(bars) {
            layer.setFrame(rect(r.x, r.bottom, r.width, r.height));
        }
        for (layer, r) in self.peaks.iter().zip(peaks) {
            match r {
                Some(r) => {
                    layer.setFrame(rect(r.x, r.bottom, r.width, r.height));
                    layer.setHidden(false);
                }
                None => layer.setHidden(true),
            }
        }
        CATransaction::commit();
    }
}

impl Drop for Layers {
    fn drop(&mut self) {
        self.root.removeFromSuperlayer();
    }
}

/// Takes a reference to the window's view, from the pointer winit hands out.
#[allow(
    unsafe_code,
    reason = "winit's handle is a valid NSView for the window's lifetime"
)]
fn retain_view(view: *mut std::ffi::c_void) -> Option<Retained<NSView>> {
    // SAFETY: the pointer is the live NSView of a window that is open, on the main thread.
    unsafe { Retained::retain(view.cast::<NSView>()) }
}

/// Adds or removes sublayers of `root` until there are `count`.
fn resize(root: &CALayer, layers: &mut Vec<Retained<CALayer>>, count: usize, color: [f64; 4]) {
    while layers.len() > count {
        if let Some(layer) = layers.pop() {
            layer.removeFromSuperlayer();
        }
    }
    while layers.len() < count {
        let layer = CALayer::new();
        layer.setBackgroundColor(Some(&cg_color(color)));
        root.addSublayer(&layer);
        layers.push(layer);
    }
}

fn recolor(layers: &[Retained<CALayer>], color: [f64; 4]) {
    let color = cg_color(color);
    for layer in layers {
        layer.setBackgroundColor(Some(&color));
    }
}

fn cg_color([r, g, b, a]: [f64; 4]) -> CFRetained<CGColor> {
    CGColor::new_srgb(r, g, b, a)
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
}
