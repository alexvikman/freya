//! Core Animation: a shape layer added to the window's view. Freya sets its path each frame;
//! the system strokes it on the graphics card.

use objc2::rc::Retained;
use objc2_app_kit::NSView;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGMutablePath};
use objc2_quartz_core::{CALayer, CAShapeLayer, CATransaction, kCALineCapRound, kCALineJoinRound};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;

use super::Area;

pub const NAME: &str = "Core Animation";
pub const SUPPORTED: bool = true;

pub struct Layer {
    /// The window view's own layer, which the oscilloscope is added to.
    parent: Retained<CALayer>,
    /// Placed where the skin wants the oscilloscope; inside it, y counts from the bottom.
    root: Retained<CALayer>,
    line: Retained<CAShapeLayer>,
    /// The points last drawn, so a still line is not handed over again.
    drawn: Vec<(f64, f64)>,
}

impl Layer {
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
        root.setMasksToBounds(true);
        let line = CAShapeLayer::new();
        line.setFillColor(None);
        round_line(&line);
        root.addSublayer(&line);
        parent.addSublayer(&root);
        Some(Self {
            parent,
            root,
            line,
            drawn: Vec::new(),
        })
    }

    /// Core Animation works in points, like Slint's logical pixels.
    pub fn place(&mut self, area: Option<&Area>) {
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
                self.line.setFrame(rect(0.0, 0.0, area.width, area.height));
                self.line.setStrokeColor(Some(&cg_color(area.color)));
                self.line.setLineWidth(area.line_width);
                self.line.setContentsScale(self.parent.contentsScale());
                self.root.setHidden(false);
            }
        }
        CATransaction::commit();
    }

    pub fn draw(&mut self, points: &[(f64, f64)]) {
        if self.drawn == points {
            return;
        }
        self.drawn.clear();
        self.drawn.extend_from_slice(points);
        let points: Vec<CGPoint> = points.iter().map(|&(x, y)| CGPoint::new(x, y)).collect();
        let path = CGMutablePath::new();
        add_lines(&path, &points);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.line.setPath(Some(&path));
        CATransaction::commit();
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        self.root.removeFromSuperlayer();
    }
}

/// Rounds the line's joins and ends, as Slint strokes paths.
#[allow(unsafe_code, reason = "reads two constants Core Animation exports")]
fn round_line(line: &CAShapeLayer) {
    // SAFETY: the constants are immutable strings Core Animation always provides.
    let (join, cap) = unsafe { (kCALineJoinRound, kCALineCapRound) };
    line.setLineJoin(join);
    line.setLineCap(cap);
}

/// Adds the polyline through `points` to `path`.
#[allow(
    unsafe_code,
    reason = "CGPathAddLines reads exactly `count` points from a live slice"
)]
fn add_lines(path: &CGMutablePath, points: &[CGPoint]) {
    // SAFETY: no transform (null is allowed), and the pointer and count describe `points`.
    unsafe {
        CGMutablePath::add_lines(Some(path), std::ptr::null(), points.as_ptr(), points.len());
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

fn cg_color([r, g, b, a]: [f64; 4]) -> CFRetained<CGColor> {
    CGColor::new_srgb(r, g, b, a)
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
}
