//! Rendering skins without a screen, for screenshot tests and skin previews.
//!
//! Uses Slint's software renderer through a headless platform. A process can only have one
//! Slint platform, so once this is used the process cannot also open real windows.

use std::cell::OnceCell;
use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{PhysicalSize, PlatformError, Rgb8Pixel};
use slint_interpreter::{ComponentHandle, ComponentInstance};

use crate::{Error, SkinSource};

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(Rc::clone(&self.window) as Rc<dyn WindowAdapter>)
    }
}

thread_local! {
    static WINDOW: OnceCell<Rc<MinimalSoftwareWindow>> = const { OnceCell::new() };
}

fn headless_window() -> Result<Rc<MinimalSoftwareWindow>, Error> {
    WINDOW.with(|cell| {
        if let Some(window) = cell.get() {
            return Ok(Rc::clone(window));
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless {
            window: Rc::clone(&window),
        }))
        .map_err(|e| Error::Platform(PlatformError::Other(e.to_string())))?;
        let _ = cell.set(Rc::clone(&window));
        Ok(window)
    })
}

/// Makes Slint render into memory instead of opening windows, for tests that drive skins
/// without a screen. Call it before loading a skin, on the thread that uses the skin.
pub fn init_headless() -> Result<(), Error> {
    headless_window().map(|_| ())
}

/// An RGB image, 3 bytes per pixel, row by row.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Renders an already created window, like [`render`] does for skins.
pub fn render_instance(
    instance: &ComponentInstance,
    width: u32,
    height: u32,
    scale: f32,
) -> Result<Image, Error> {
    let window = headless_window()?;
    instance.show()?;
    window.dispatch_event(WindowEvent::ScaleFactorChanged {
        scale_factor: scale,
    });
    let (pw, ph) = (
        (width as f32 * scale).round() as u32,
        (height as f32 * scale).round() as u32,
    );
    window.set_size(PhysicalSize::new(pw, ph));
    let mut buffer = vec![Rgb8Pixel::default(); (pw * ph) as usize];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, pw as usize);
    });
    instance.hide()?;
    Ok(Image {
        width: pw,
        height: ph,
        pixels: buffer.iter().flat_map(|p| [p.r, p.g, p.b]).collect(),
    })
}

/// Renders a skin at `width` × `height` logical pixels and `scale` (2.0 for a Retina screen).
/// `setup` can set API properties before rendering.
pub fn render(
    source: &SkinSource,
    width: u32,
    height: u32,
    scale: f32,
    setup: impl FnOnce(&ComponentInstance),
) -> Result<Image, Error> {
    let window = headless_window()?;
    let instance = crate::load(source)?;
    setup(&instance);
    instance.show()?;

    window.dispatch_event(WindowEvent::ScaleFactorChanged {
        scale_factor: scale,
    });
    let (pw, ph) = (
        (width as f32 * scale).round() as u32,
        (height as f32 * scale).round() as u32,
    );
    window.set_size(PhysicalSize::new(pw, ph));

    let mut buffer = vec![Rgb8Pixel::default(); (pw * ph) as usize];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, pw as usize);
    });
    instance.hide()?;

    Ok(Image {
        width: pw,
        height: ph,
        pixels: buffer.iter().flat_map(|p| [p.r, p.g, p.b]).collect(),
    })
}
