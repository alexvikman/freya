//! Closing the window, opening it again, and quitting.
//!
//! Freya's own close button keeps Freya running, as music apps do: in the Dock on macOS, in the
//! notification area on Windows, and minimized on Linux, where not every desktop has a
//! notification area; unless the settings say closing quits. The system asking the window to
//! close, as the taskbar's Close window and Alt+F4 do, quits on Windows and Linux; on macOS the
//! system's button is the window's own. Exit, and Quit, always quit.

use super::*;

/// What asked the window to close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseBy {
    /// Freya's close button, or Close Window in its menu.
    Button,
    /// The system: the title bar's button, the taskbar, Alt+F4.
    System,
}

/// What closing the window does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OnClose {
    Quit,
    /// The window goes, Freya staying in the Dock or the notification area.
    Hide,
    Minimize,
}

/// What closing the window by `by` does on this system, when the settings say whether it keeps
/// Freya running.
fn on_close(by: CloseBy, keep_running: bool) -> OnClose {
    if cfg!(target_os = "macos") {
        return OnClose::Hide;
    }
    match by {
        CloseBy::System => OnClose::Quit,
        CloseBy::Button if !keep_running => OnClose::Quit,
        CloseBy::Button if cfg!(windows) => OnClose::Hide,
        CloseBy::Button => OnClose::Minimize,
    }
}

impl App {
    /// Closes the window, as asked by `by`: what plays plays on unless Freya quits.
    pub fn close_window(&mut self, by: CloseBy) {
        match on_close(by, self.settings.close_minimizes) {
            OnClose::Quit => self.quit(),
            OnClose::Hide => {
                self.window_closed = true;
                if let Err(e) = self.view.ui.hide() {
                    log::warn!("could not close the window: {e}");
                }
            }
            OnClose::Minimize => self.view.ui.window().set_minimized(true),
        }
    }

    /// Opens the window again, from the Dock or the notification area, in front.
    pub fn open_window(&mut self) {
        self.window_closed = false;
        let ui = self.view.ui.clone();
        if let Err(e) = ui.show() {
            log::warn!("could not open the window again: {e}");
        }
        ui.window().set_minimized(false);
        ui.window().with_winit_window(|w| w.focus_window());
    }

    /// Quits Freya: the event loop ends, and what Freya keeps is saved after it.
    pub fn quit(&mut self) {
        let _ = slint::quit_event_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_keeps_freya_running_as_its_system_has_it() {
        if cfg!(target_os = "macos") {
            for by in [CloseBy::Button, CloseBy::System] {
                assert_eq!(on_close(by, true), OnClose::Hide);
                assert_eq!(on_close(by, false), OnClose::Hide);
            }
            return;
        }
        let kept = if cfg!(windows) {
            OnClose::Hide
        } else {
            OnClose::Minimize
        };
        assert_eq!(on_close(CloseBy::Button, true), kept);
        assert_eq!(on_close(CloseBy::Button, false), OnClose::Quit);
        // The taskbar's Close window, Alt+F4.
        assert_eq!(on_close(CloseBy::System, true), OnClose::Quit);
        assert_eq!(on_close(CloseBy::System, false), OnClose::Quit);
    }
}
