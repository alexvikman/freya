//! The window's title bar, which a skin may draw into on macOS (`App.title-bar-height`): the
//! title bar turns transparent, the skin reaches the window's top edge, and the window buttons
//! move into the skin's top bar, centred in its height. Other systems keep their title bar.

#[cfg(target_os = "macos")]
use slint_interpreter::Value;

use super::skin_window::SkinWindow;

#[cfg(target_os = "macos")]
mod mac;

/// One window's title bar.
#[derive(Default)]
pub struct TitleBar {
    /// The height of the skin's top bar the title bar was made for; 0 while it is the system's
    /// own.
    #[cfg(target_os = "macos")]
    height: f32,
    /// How much taller the window's content got by reaching into the title bar.
    added: f32,
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
        if wanted != self.height
            && let Some(added) = mac::set_height(ui.window(), wanted)
        {
            self.height = wanted;
            self.added = added;
        }
        if self.height > 0.0 {
            mac::buttons_width(ui.window(), self.height)
        } else {
            0.0
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
