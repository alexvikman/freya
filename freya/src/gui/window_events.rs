//! Window events Slint does not pass on: files dragged onto the window, whether a window can
//! be seen and has focus, and the app being quit. They are taken from winit directly.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::winit_030::winit::event::{ElementState, MouseButton, WindowEvent as WinitEvent};
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
    /// The mouse's back (`false`) or forward (`true`) button.
    Navigate(bool),
}

/// What to do when the app quits.
type ExitAction = Box<dyn FnMut()>;

/// What to do at once after an event that should not wait for the next tick.
type WakeAction = Rc<dyn Fn()>;

/// Collects window events for the app to handle on its next tick.
#[derive(Clone, Default)]
pub struct WindowEvents {
    queue: Rc<RefCell<Vec<WindowEvent>>>,
    on_exit: Rc<RefCell<Option<ExitAction>>>,
    /// Handles the queue at once, for events that should not wait for the next tick.
    wake: Rc<RefCell<Option<WakeAction>>>,
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

    /// Runs `action` soon after an event that should not wait for the app's next tick, like a
    /// press of the mouse's back button.
    pub fn on_wake(&self, action: impl Fn() + 'static) {
        *self.wake.borrow_mut() = Some(Rc::new(action));
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
            WinitEvent::MouseInput {
                state: ElementState::Pressed,
                button: button @ (MouseButton::Back | MouseButton::Forward),
                ..
            } => {
                queue.push(WindowEvent::Navigate(*button == MouseButton::Forward));
                // After winit's event, when the app is free to handle it.
                if let Some(wake) = self.0.wake.borrow().clone() {
                    slint::Timer::single_shot(std::time::Duration::ZERO, move || wake());
                }
            }
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
