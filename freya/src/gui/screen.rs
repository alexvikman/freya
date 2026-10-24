//! Where Freya's window opens: where it was last, as large as it was and maximized if it was;
//! the first time, large on the screen, as the system places it. A window left on a screen that
//! is no longer there is brought back to one that is.

#![cfg_attr(
    any(target_os = "macos", windows),
    allow(
        unsafe_code,
        reason = "asks the system for the screen's area free of the menu bar or task bar"
    )
)]

use slint::winit_030::WinitWindowAccessor;

use crate::settings::Settings;

/// How much of the screen's free area a first window takes, wide and high.
const FIRST_SHARE: f32 = 0.85;

/// Gives the window, not shown yet, the place `settings` kept for it, or the first time a
/// large size. Returns whether it is to be made large once it is open, as on systems whose
/// screen is only known then.
pub fn place(window: &slint::Window, settings: &Settings) -> bool {
    let mut later = false;
    match settings.window_size {
        Some((w, h)) => window.set_size(slint::LogicalSize::new(w, h)),
        None => match free_area() {
            Some(area) => {
                let (w, h) = first_size(area);
                window.set_size(slint::LogicalSize::new(w, h));
            }
            None => later = true,
        },
    }
    if let Some((x, y)) = settings.window_position {
        window.set_position(slint::PhysicalPosition::new(x, y));
    }
    if settings.window_maximized {
        window.set_maximized(true);
    }
    later
}

/// Once the window is open: on a screen there is, and the first time large, where the screen
/// was not known before.
pub fn settle(window: &slint::Window, make_large: bool) {
    window.with_winit_window(|w| {
        let monitors: Vec<_> = w.available_monitors().collect();
        if let Ok(at) = w.outer_position() {
            let shown = monitors.iter().any(|m| {
                let (p, s) = (m.position(), m.size());
                // Its top left, within a screen: enough of it to take hold of.
                at.x >= p.x
                    && at.y >= p.y
                    && at.x < p.x + s.width as i32 - 64
                    && at.y < p.y + s.height as i32 - 64
            });
            if !shown && let Some(primary) = w.primary_monitor().or(monitors.into_iter().next()) {
                let p = primary.position();
                w.set_outer_position(slint::winit_030::winit::dpi::PhysicalPosition::new(
                    p.x + 48,
                    p.y + 48,
                ));
            }
        }
        if make_large && let Some(monitor) = w.current_monitor() {
            let scale = monitor.scale_factor() as f32;
            let size = monitor.size();
            let (width, height) =
                first_size((size.width as f32 / scale, size.height as f32 / scale));
            let _ = w.request_inner_size(slint::winit_030::winit::dpi::LogicalSize::new(
                width, height,
            ));
        }
    });
}

/// A first window's size, in points, on a screen whose free area is `area`: most of it. The
/// skin keeps it from getting smaller than it takes.
fn first_size(area: (f32, f32)) -> (f32, f32) {
    (
        (area.0 * FIRST_SHARE).round(),
        (area.1 * FIRST_SHARE).round(),
    )
}

/// The main screen's area free of the menu bar and the Dock, in points.
#[cfg(target_os = "macos")]
fn free_area() -> Option<(f32, f32)> {
    let mtm = objc2::MainThreadMarker::new()?;
    let frame = objc2_app_kit::NSScreen::mainScreen(mtm)?.visibleFrame();
    Some((frame.size.width as f32, frame.size.height as f32))
}

/// The main screen's area free of the task bar, in points.
#[cfg(windows)]
fn free_area() -> Option<(f32, f32)> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SystemParametersInfoW};
    let mut area = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: the work area is written into the rectangle given.
    let found = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, (&raw mut area).cast(), 0) };
    // SAFETY: a plain query.
    let scale = unsafe { GetDpiForSystem() } as f32 / 96.0;
    (found != 0 && scale > 0.0).then(|| {
        (
            (area.right - area.left) as f32 / scale,
            (area.bottom - area.top) as f32 / scale,
        )
    })
}

/// Elsewhere the screen is known only once the window is open.
#[cfg(not(any(target_os = "macos", windows)))]
fn free_area() -> Option<(f32, f32)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_window_takes_most_of_the_screen() {
        assert_eq!(first_size((1920.0, 1040.0)), (1632.0, 884.0));
        assert_eq!(first_size((1512.0, 945.0)), (1285.0, 803.0));
    }
}
