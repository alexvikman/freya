//! Scrolling by the mouse's middle button: a click marks the place, and moving the mouse away
//! from it scrolls whatever is under the mark, faster the farther; another click, any other
//! button, or Esc stops it. Held down instead, the button scrolls until it is let go. Whatever
//! scrolls is asked as by the wheel, so every list and page scrolls so, without a view of its
//! own doing it.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use slint::LogicalPosition;
use slint::winit_030::winit::event::{ElementState, MouseButton, WindowEvent as WinitEvent};
use slint::winit_030::winit::keyboard::{Key, NamedKey};

/// How far the mouse may move from the mark without scrolling, in points.
const STILL: f32 = 12.0;
/// How many points a frame it scrolls for each point the mouse is beyond that…
const SPEED: f32 = 0.12;
/// …and at most.
const FASTEST: f32 = 160.0;
/// How often it scrolls while on: every frame of a 60 Hz screen.
const FRAME: Duration = Duration::from_millis(16);

/// What is done every frame while it scrolls.
type FrameAction = Rc<dyn Fn()>;

/// What a frame does: where it scrolls, and how much.
pub struct Step {
    pub at: LogicalPosition,
    pub delta_x: f32,
    pub delta_y: f32,
}

#[derive(Default)]
struct State {
    /// Where it was started, while it scrolls.
    mark: Option<LogicalPosition>,
    cursor: LogicalPosition,
    /// Whether the button is still held since it was started, and whether the mouse went
    /// beyond the still zone since: letting it go then stops scrolling.
    held: bool,
    moved: bool,
}

/// Scrolling by the middle button, shared between the window's events and the app.
#[derive(Clone)]
pub struct Autoscroll {
    state: Rc<RefCell<State>>,
    timer: Rc<slint::Timer>,
    /// Called every frame while it scrolls, and once after it stops.
    frame: Rc<RefCell<Option<FrameAction>>>,
}

impl Default for Autoscroll {
    fn default() -> Self {
        Self {
            state: Rc::default(),
            timer: Rc::new(slint::Timer::default()),
            frame: Rc::default(),
        }
    }
}

impl Autoscroll {
    /// Runs `action` every frame while it scrolls, and once after it stops: the app asks
    /// [`Self::step`] and scrolls its window.
    pub fn on_frame(&self, action: impl Fn() + 'static) {
        *self.frame.borrow_mut() = Some(Rc::new(action));
    }

    /// Takes a window's event, at `scale` physical pixels to the point. Returns whether it was
    /// this one's, which the window is then not given.
    pub fn event(&self, event: &WinitEvent, scale: f32) -> bool {
        let mut state = self.state.borrow_mut();
        match event {
            WinitEvent::CursorMoved { position, .. } => {
                state.cursor =
                    LogicalPosition::new(position.x as f32 / scale, position.y as f32 / scale);
                if let Some(mark) = state.mark
                    && distance(mark, state.cursor) > STILL
                {
                    state.moved = true;
                }
                false
            }
            WinitEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                ..
            } => {
                if state.mark.is_some() {
                    drop(state);
                    self.stop();
                } else {
                    state.mark = Some(state.cursor);
                    state.held = true;
                    state.moved = false;
                    drop(state);
                    self.start();
                }
                true
            }
            WinitEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Middle,
                ..
            } => {
                let was_held = std::mem::take(&mut state.held);
                let stop = state.mark.is_some() && was_held && state.moved;
                drop(state);
                if stop {
                    self.stop();
                }
                true
            }
            // Any other button stops it, and does nothing else.
            WinitEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } if state.mark.is_some() => {
                drop(state);
                self.stop();
                true
            }
            WinitEvent::KeyboardInput { event, .. }
                if state.mark.is_some()
                    && event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                drop(state);
                self.stop();
                true
            }
            WinitEvent::Focused(false) | WinitEvent::CursorLeft { .. }
                if state.mark.is_some() && !state.held =>
            {
                drop(state);
                self.stop();
                false
            }
            _ => false,
        }
    }

    /// Where it scrolls this frame, and how much; `None` while it does not.
    pub fn step(&self) -> Option<Step> {
        let state = self.state.borrow();
        let mark = state.mark?;
        let speed = |offset: f32| {
            let beyond = offset.abs() - STILL;
            if beyond <= 0.0 {
                0.0
            } else {
                // Moving the mouse down shows what is further down, as the wheel turned so.
                -offset.signum() * (beyond * SPEED).min(FASTEST)
            }
        };
        Some(Step {
            at: mark,
            delta_x: speed(state.cursor.x - mark.x),
            delta_y: speed(state.cursor.y - mark.y),
        })
    }

    /// Where it was started, while it scrolls.
    pub fn mark(&self) -> Option<LogicalPosition> {
        self.state.borrow().mark
    }

    fn start(&self) {
        let frame = Rc::clone(&self.frame);
        self.timer
            .start(slint::TimerMode::Repeated, FRAME, move || call(&frame));
        call(&self.frame);
    }

    fn stop(&self) {
        self.state.borrow_mut().mark = None;
        self.timer.stop();
        call(&self.frame);
    }
}

fn call(frame: &RefCell<Option<FrameAction>>) {
    let action = frame.borrow().clone();
    if let Some(action) = action {
        action();
    }
}

fn distance(a: LogicalPosition, b: LogicalPosition) -> f32 {
    (a.x - b.x).hypot(a.y - b.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(autoscroll: &Autoscroll, x: f32, y: f32) {
        let mut state = autoscroll.state.borrow_mut();
        state.cursor = LogicalPosition::new(x, y);
        if let Some(mark) = state.mark
            && distance(mark, state.cursor) > STILL
        {
            state.moved = true;
        }
    }

    fn start_at(autoscroll: &Autoscroll, x: f32, y: f32) {
        at(autoscroll, x, y);
        let mut state = autoscroll.state.borrow_mut();
        state.mark = Some(LogicalPosition::new(x, y));
        state.held = false;
    }

    #[test]
    fn scrolls_faster_the_farther_the_mouse() {
        let autoscroll = Autoscroll::default();
        assert!(autoscroll.step().is_none());
        start_at(&autoscroll, 100.0, 100.0);
        at(&autoscroll, 105.0, 108.0);
        let still = autoscroll.step().unwrap();
        assert_eq!((still.delta_x, still.delta_y), (0.0, 0.0), "near the mark");
        at(&autoscroll, 100.0, 162.0);
        let down = autoscroll.step().unwrap();
        assert!(
            down.delta_y < 0.0,
            "below the mark shows what is further down"
        );
        at(&autoscroll, 100.0, 300.0);
        let faster = autoscroll.step().unwrap();
        assert!(faster.delta_y < down.delta_y);
        assert_eq!(faster.at, LogicalPosition::new(100.0, 100.0));
        at(&autoscroll, 100.0, 50_000.0);
        assert_eq!(autoscroll.step().unwrap().delta_y, -FASTEST);
        at(&autoscroll, 100.0, 40.0);
        assert!(autoscroll.step().unwrap().delta_y > 0.0, "above it, up");
        autoscroll.stop();
        assert!(autoscroll.step().is_none());
    }
}
