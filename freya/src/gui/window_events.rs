//! Window events Slint does not pass on: files dragged onto the window, whether a window can
//! be seen and has focus, and the app being quit. They are taken from winit directly.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::winit_030::winit::event::WindowEvent as WinitEvent;
use slint::winit_030::winit::event_loop::ActiveEventLoop;
use slint::winit_030::winit::window::{Window, WindowId};
use slint::winit_030::{CustomApplicationHandler, EventResult};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowEvent {
    /// Files are being dragged over a window (`true`) or left it (`false`).
    FileHover(bool),
    FileDropped(PathBuf),
    /// A window became hidden (`true`): covered by other windows, minimized, or on another
    /// desktop. Not every system reports this.
    Occluded(WindowId, bool),
    Focused(WindowId, bool),
}

/// What to do when the app quits.
type ExitAction = Box<dyn FnMut()>;

/// Collects window events for the app to handle on its next tick.
#[derive(Clone, Default)]
pub struct WindowEvents {
    queue: Rc<RefCell<Vec<WindowEvent>>>,
    on_exit: Rc<RefCell<Option<ExitAction>>>,
}

impl WindowEvents {
    pub fn take(&self) -> Vec<WindowEvent> {
        std::mem::take(&mut self.queue.borrow_mut())
    }

    /// Runs `action` when the app is about to quit. Needed because quitting from the system
    /// (Cmd+Q, the Dock, logging out on macOS) ends the process without returning from the
    /// event loop.
    pub fn on_exit(&self, action: impl FnMut() + 'static) {
        *self.on_exit.borrow_mut() = Some(Box::new(action));
    }

    /// A winit event hook that fills this queue.
    pub fn handler(&self) -> Handler {
        Handler(self.clone())
    }
}

pub struct Handler(WindowEvents);

impl CustomApplicationHandler for Handler {
    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        _winit_window: Option<&Window>,
        _slint_window: Option<&slint::Window>,
        event: &WinitEvent,
    ) -> EventResult {
        let mut queue = self.0.queue.borrow_mut();
        match event {
            WinitEvent::HoveredFile(_) => queue.push(WindowEvent::FileHover(true)),
            WinitEvent::HoveredFileCancelled => queue.push(WindowEvent::FileHover(false)),
            WinitEvent::DroppedFile(path) => {
                queue.push(WindowEvent::FileHover(false));
                queue.push(WindowEvent::FileDropped(path.clone()));
            }
            WinitEvent::Occluded(hidden) => queue.push(WindowEvent::Occluded(window_id, *hidden)),
            WinitEvent::Focused(focused) => queue.push(WindowEvent::Focused(window_id, *focused)),
            _ => {}
        }
        EventResult::Propagate
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) -> EventResult {
        if let Some(action) = self.0.on_exit.borrow_mut().as_mut() {
            action();
        }
        EventResult::Propagate
    }
}
