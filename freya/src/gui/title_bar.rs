//! The window's title bar, which a skin may draw into on macOS (`App.title-bar-height`): the
//! title bar turns transparent, the skin reaches the window's top edge, and the window buttons
//! move into the skin's top bar, centred in its height. Other systems keep their title bar.

use std::cell::Cell;
use std::rc::Rc;

#[cfg(target_os = "macos")]
use slint_interpreter::Value;

use super::skin_window::SkinWindow;

#[cfg(target_os = "macos")]
mod mac;

/// One window's title bar.
#[derive(Default)]
pub struct TitleBar {
    /// The height of the skin's top bar the title bar was made for; 0 while it is the system's
    /// own. Shared with `after_frame`.
    height: Rc<Cell<f32>>,
    /// How much taller the window's content got by reaching into the title bar.
    added: f32,
    /// Puts the buttons back as soon as the title changes.
    #[cfg(target_os = "macos")]
    watch: Option<mac::TitleWatch>,
}

impl TitleBar {
    /// Gives the window the title bar its skin asks for. Returns the room the window buttons
    /// take in the skin's top bar, for `App.window-buttons-width`.
    #[cfg(target_os = "macos")]
    pub fn update(&mut self, ui: &SkinWindow) -> f32 {
        let wanted = match ui.get_global_property("App", "title-bar-height") {
            Ok(Value::Number(height)) if height > 0.0 => height as f32,
            _ => 0.0,
        };
        if wanted != self.height.get()
            && let Some(added) = mac::set_height(ui.window(), wanted)
        {
            self.height.set(wanted);
            self.added = added;
            match &self.watch {
                Some(watch) => watch.set_height(wanted),
                None => self.watch = mac::TitleWatch::new(ui.window(), wanted),
            }
        }
        if self.height.get() > 0.0 {
            mac::buttons_width(ui.window(), self.height.get())
        } else {
            0.0
        }
    }

    /// What to do after each frame of `window`: put the window buttons back if AppKit moved
    /// them. It lays them out again when the window's title changes, as it does with every
    /// track, just before the frame is drawn; put back only at the next tick, they showed in
    /// the corner for a moment.
    pub fn after_frame(&self) -> impl Fn(&slint::Window) + 'static {
        let height = Rc::clone(&self.height);
        move |_window: &slint::Window| {
            #[cfg(target_os = "macos")]
            if height.get() > 0.0 {
                mac::buttons_width(_window, height.get());
            }
            let _ = &height;
        }
    }

    /// Other systems keep their own title bar, whatever the skin asks.
    #[cfg(not(target_os = "macos"))]
    pub fn update(&mut self, _ui: &SkinWindow) -> f32 {
        0.0
    }

    /// How much taller the window's content is than it would be under the system's title bar:
    /// subtracted when the window's size is saved, as it is restored under the system's.
    pub fn added_height(&self) -> f32 {
        self.added
    }
}

/// `App.drag-window`: the window follows the mouse, as from a title bar; a double click zooms
/// or minimizes it, as the system's settings say. Only during a mouse press.
pub fn drag(window: &slint::Window) {
    #[cfg(target_os = "macos")]
    mac::drag(window);
    #[cfg(not(target_os = "macos"))]
    {
        use slint::winit_030::WinitWindowAccessor;
        window.with_winit_window(|w| {
            if let Err(e) = w.drag_window() {
                log::debug!("could not move the window: {e}");
            }
        });
    }
}
