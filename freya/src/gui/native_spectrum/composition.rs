//! Windows.UI.Composition: a visual tree hosted on top of the window's own content, with
//! one solid-color sprite per bar and peak marker. Like Core Animation on macOS, moving
//! sprites costs almost nothing; the system composites them.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;
use windows::System::{DispatcherQueue, DispatcherQueueController};
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionColorBrush, Compositor, ContainerVisual, SpriteVisual};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::core::Interface;
use windows_numerics::{Vector2, Vector3};

use super::{Area, Rect};

pub const NAME: &str = "Windows.UI.Composition";
pub const SUPPORTED: bool = true;

pub struct Layers {
    /// The window's scale: composition works in physical pixels.
    scale: f32,
    root: ContainerVisual,
    bars: Vec<SpriteVisual>,
    peaks: Vec<SpriteVisual>,
    bar_brush: CompositionColorBrush,
    peak_brush: CompositionColorBrush,
    compositor: Compositor,
    // Kept alive: without them the visuals are gone.
    _target: DesktopWindowTarget,
    _queue: Option<DispatcherQueueController>,
}

impl Layers {
    pub fn new(window: &slint::Window) -> Option<Self> {
        let hwnd = window
            .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
                RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
                _ => None,
            })
            .flatten()?;
        Self::create(HWND(hwnd as *mut _))
            .inspect_err(|e| log::warn!("no native spectrum: {e}"))
            .ok()
    }

    fn create(hwnd: HWND) -> windows::core::Result<Self> {
        // Composition needs a dispatcher queue on the UI thread.
        let queue = match DispatcherQueue::GetForCurrentThread() {
            Ok(_) => None,
            Err(_) => Some(create_dispatcher_queue()?),
        };
        let compositor = Compositor::new()?;
        let target = create_target(&compositor, hwnd)?;
        let root = compositor.CreateContainerVisual()?;
        root.SetIsVisible(false)?;
        target.SetRoot(&root)?;
        let transparent = Color {
            A: 0,
            R: 0,
            G: 0,
            B: 0,
        };
        Ok(Self {
            scale: 1.0,
            bar_brush: compositor.CreateColorBrushWithColor(transparent)?,
            peak_brush: compositor.CreateColorBrushWithColor(transparent)?,
            root,
            bars: Vec::new(),
            peaks: Vec::new(),
            compositor,
            _target: target,
            _queue: queue,
        })
    }

    pub fn place(&mut self, area: Option<&Area>, scale: f32) {
        self.scale = scale;
        if let Err(e) = self.try_place(area) {
            log::debug!("native spectrum: {e}");
        }
    }

    fn try_place(&mut self, area: Option<&Area>) -> windows::core::Result<()> {
        let Some(area) = area else {
            return self.root.SetIsVisible(false);
        };
        let s = f64::from(self.scale);
        self.root.SetOffset(offset(area.x * s, area.y * s))?;
        self.root.SetSize(Vector2::new(
            (area.width * s) as f32,
            (area.height * s) as f32,
        ))?;
        self.bar_brush.SetColor(color(area.bar_color))?;
        self.peak_brush.SetColor(color(area.peak_color))?;
        self.root.SetIsVisible(true)
    }

    /// Nothing to do: the system keeps the layer drawn.
    pub fn refresh(&mut self) {}

    pub fn draw(&mut self, area: &Area, bars: &[Rect], peaks: &[Option<Rect>]) {
        if let Err(e) = self.try_draw(area, bars, peaks) {
            log::debug!("native spectrum: {e}");
        }
    }

    fn try_draw(
        &mut self,
        area: &Area,
        bars: &[Rect],
        peaks: &[Option<Rect>],
    ) -> windows::core::Result<()> {
        resize(
            &self.compositor,
            &self.root,
            &mut self.bars,
            bars.len(),
            &self.bar_brush,
        )?;
        resize(
            &self.compositor,
            &self.root,
            &mut self.peaks,
            peaks.len(),
            &self.peak_brush,
        )?;
        for (sprite, r) in self.bars.iter().zip(bars) {
            self.position(sprite, area, r)?;
        }
        for (sprite, r) in self.peaks.iter().zip(peaks) {
            match r {
                Some(r) => {
                    self.position(sprite, area, r)?;
                    sprite.SetIsVisible(true)?;
                }
                None => sprite.SetIsVisible(false)?,
            }
        }
        Ok(())
    }

    /// Places a sprite; `r` counts from the area's bottom, composition from its top.
    fn position(&self, sprite: &SpriteVisual, area: &Area, r: &Rect) -> windows::core::Result<()> {
        let s = f64::from(self.scale);
        let top = area.height - r.bottom - r.height;
        sprite.SetOffset(offset(r.x * s, top * s))?;
        sprite.SetSize(Vector2::new((r.width * s) as f32, (r.height * s) as f32))
    }
}

impl Drop for Layers {
    fn drop(&mut self) {
        if let Ok(children) = self.root.Children() {
            let _ = children.RemoveAll();
        }
    }
}

#[allow(
    unsafe_code,
    reason = "a plain Windows API call; the options are fully set"
)]
fn create_dispatcher_queue() -> windows::core::Result<DispatcherQueueController> {
    let options = DispatcherQueueOptions {
        dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
        threadType: DQTYPE_THREAD_CURRENT,
        apartmentType: DQTAT_COM_STA,
    };
    // SAFETY: called on the UI thread with valid options.
    unsafe { CreateDispatcherQueueController(options) }
}

#[allow(
    unsafe_code,
    reason = "hosting composition on the window winit created"
)]
fn create_target(
    compositor: &Compositor,
    hwnd: HWND,
) -> windows::core::Result<DesktopWindowTarget> {
    let interop: ICompositorDesktopInterop = compositor.cast()?;
    // SAFETY: `hwnd` is the open window of this thread; topmost puts the spectrum above the
    // window's own content.
    unsafe { interop.CreateDesktopWindowTarget(hwnd, true) }
}

/// Adds or removes sprites in `root` until there are `count`.
fn resize(
    compositor: &Compositor,
    root: &ContainerVisual,
    sprites: &mut Vec<SpriteVisual>,
    count: usize,
    brush: &CompositionColorBrush,
) -> windows::core::Result<()> {
    let children = root.Children()?;
    while sprites.len() > count {
        if let Some(sprite) = sprites.pop() {
            children.Remove(&sprite)?;
        }
    }
    while sprites.len() < count {
        let sprite = compositor.CreateSpriteVisual()?;
        sprite.SetBrush(brush)?;
        children.InsertAtTop(&sprite)?;
        sprites.push(sprite);
    }
    Ok(())
}

/// Composition places visuals in 3D; the spectrum lies flat.
fn offset(x: f64, y: f64) -> Vector3 {
    Vector3 {
        X: x as f32,
        Y: y as f32,
        Z: 0.0,
    }
}

fn color([r, g, b, a]: [f64; 4]) -> Color {
    let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color {
        A: byte(a),
        R: byte(r),
        G: byte(g),
        B: byte(b),
    }
}
