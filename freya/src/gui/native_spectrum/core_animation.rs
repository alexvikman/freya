//! Core Animation: a layer added to the window's view, with two shape layers in it, one for
//! the bars and one for the peak markers. Each frame hands the system two paths, rather than
//! moving one layer per bar and marker.

use objc2::rc::Retained;
use objc2_app_kit::NSView;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGMutablePath};
use objc2_quartz_core::{CALayer, CAShapeLayer, CATransaction};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;

use super::{Area, Rect};

pub const NAME: &str = "Core Animation";
pub const SUPPORTED: bool = true;

pub struct Layers {
    /// The window view's own layer, which the spectrum is added to.
    parent: Retained<CALayer>,
    /// Placed where the skin wants the spectrum; inside it, y counts from the bottom.
    root: Retained<CALayer>,
    bars: Retained<CAShapeLayer>,
    peaks: Retained<CAShapeLayer>,
    colors: Option<([f64; 4], [f64; 4])>,
    /// The rectangles last drawn, so a still spectrum is not handed over again.
    drawn: (Vec<CGRect>, Vec<CGRect>),
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
        let bars = CAShapeLayer::new();
        let peaks = CAShapeLayer::new();
        root.addSublayer(&bars);
        root.addSublayer(&peaks);
        parent.addSublayer(&root);
        Some(Self {
            parent,
            root,
            bars,
            peaks,
            colors: None,
            drawn: (Vec::new(), Vec::new()),
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
                let inside = rect(0.0, 0.0, area.width, area.height);
                self.bars.setFrame(inside);
                self.peaks.setFrame(inside);
                self.root.setHidden(false);
                let colors = (area.bar_color, area.peak_color);
                if self.colors != Some(colors) {
                    self.colors = Some(colors);
                    self.bars.setFillColor(Some(&cg_color(area.bar_color)));
                    self.peaks.setFillColor(Some(&cg_color(area.peak_color)));
                }
            }
        }
        CATransaction::commit();
    }

    /// Nothing to do: the system keeps the layer drawn.
    pub fn refresh(&mut self) {}

    pub fn draw(&mut self, _area: &Area, bars: &[Rect], peaks: &[Option<Rect>]) {
        let to_cg = |r: &Rect| rect(r.x, r.bottom, r.width, r.height);
        let bars: Vec<CGRect> = bars.iter().map(to_cg).collect();
        let peaks: Vec<CGRect> = peaks.iter().flatten().map(to_cg).collect();
        let bars_changed = bars != self.drawn.0;
        let peaks_changed = peaks != self.drawn.1;
        if !bars_changed && !peaks_changed {
            return;
        }
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        if bars_changed {
            self.bars.setPath(Some(&path(&bars)));
            self.drawn.0 = bars;
        }
        if peaks_changed {
            self.peaks.setPath(Some(&path(&peaks)));
            self.drawn.1 = peaks;
        }
        CATransaction::commit();
    }
}

impl Drop for Layers {
    fn drop(&mut self) {
        self.root.removeFromSuperlayer();
    }
}

/// A path of the rectangles `rects`.
#[allow(
    unsafe_code,
    reason = "CGPathAddRects reads exactly `count` rectangles from a live slice"
)]
fn path(rects: &[CGRect]) -> CFRetained<CGMutablePath> {
    let path = CGMutablePath::new();
    // SAFETY: no transform (null is allowed), and the pointer and count describe `rects`.
    unsafe {
        CGMutablePath::add_rects(Some(&path), std::ptr::null(), rects.as_ptr(), rects.len());
    }
    path
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

fn cg_color([r, g, b, a]: [f64; 4]) -> CFRetained<CGColor> {
    CGColor::new_srgb(r, g, b, a)
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
}
