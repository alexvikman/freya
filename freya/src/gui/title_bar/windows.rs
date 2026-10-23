//! A frameless window on Windows, for a skin that draws into the title bar. winit keeps the
//! window's styles without a frame, so snapping, the shadow and the animations stay, but the
//! whole window is content: the edges that resized it are found here again.

#![allow(
    unsafe_code,
    reason = "a window's messages are followed through the system's subclassing, for a window \
              that is open, on the main thread"
)]

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;
use slint::winit_030::winit::platform::windows::WindowExtWindows;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCLIENT, HTLEFT, HTRIGHT, HTTOP,
    HTTOPLEFT, HTTOPRIGHT, IsZoomed, SM_CXPADDEDBORDER, SM_CXSIZEFRAME, WM_NCHITTEST,
};

/// Whether the window is frameless now, for its edges.
static FRAMELESS: AtomicBool = AtomicBool::new(false);

/// Makes the window ready to go frameless: its shadow kept, its edges resizing it. Returns how
/// much wider and taller its content gets without the frame, measured while it is still on;
/// `None` while the window is not open.
pub fn prepare(window: &slint::Window) -> Option<(f32, f32)> {
    window
        .with_winit_window(|w| {
            let hwnd = hwnd(w)?;
            // SAFETY: `hwnd` is the open window's, on its own thread; the procedure stays
            // for the life of the window.
            if unsafe { SetWindowSubclass(hwnd, Some(edges), 1, 0) } == 0 {
                log::warn!("the window's edges would not resize it without its frame");
                return None;
            }
            w.set_undecorated_shadow(true);
            let (outer, inner) = (w.outer_size(), w.inner_size());
            let scale = w.scale_factor() as f32;
            Some((
                outer.width.saturating_sub(inner.width) as f32 / scale,
                outer.height.saturating_sub(inner.height) as f32 / scale,
            ))
        })
        .flatten()
}

/// Says whether the window is frameless now: the skin took the frame off, or put it back.
pub fn set_frameless(frameless: bool) {
    FRAMELESS.store(frameless, Ordering::Relaxed);
}

thread_local! {
    /// When the last press in the top bar was, for a double click.
    static LAST_PRESS: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Moves the window as the mouse moves, as from a title bar, snapping as Windows does; a
/// double click maximizes it, or puts it back.
pub fn drag(window: &slint::Window) {
    let now = Instant::now();
    // SAFETY: a plain query of a setting.
    let double = Duration::from_millis(u64::from(unsafe { GetDoubleClickTime() }));
    let last = LAST_PRESS.replace(Some(now));
    if last.is_some_and(|last| now - last <= double) {
        LAST_PRESS.set(None);
        window.set_maximized(!window.is_maximized());
        return;
    }
    window.with_winit_window(|w| {
        if let Err(e) = w.drag_window() {
            log::debug!("could not move the window: {e}");
        }
    });
}

fn hwnd(window: &slint::winit_030::winit::window::Window) -> Option<HWND> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
        _ => None,
    }
}

/// The window's procedure, before winit's: where the content would be, the edges of a
/// frameless window resize it, as its frame did. Not while maximized, nor in full screen,
/// which is not frameless here.
unsafe extern "system" fn edges(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    // SAFETY: passed on as the system gave it.
    let hit = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    if message != WM_NCHITTEST || hit != HTCLIENT as LRESULT || !FRAMELESS.load(Ordering::Relaxed) {
        return hit;
    }
    // SAFETY: queries of the window the message is for.
    let (zoomed, rect, border) = unsafe {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let dpi = GetDpiForWindow(hwnd);
        let border = GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi)
            + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
        (
            IsZoomed(hwnd) != 0,
            (GetWindowRect(hwnd, &mut rect) != 0).then_some(rect),
            border,
        )
    };
    let Some(rect) = rect.filter(|_| !zoomed) else {
        return hit;
    };
    // Screen coordinates, signed: a second screen may be left of or above the first.
    let x = i32::from(lparam as u16 as i16);
    let y = i32::from((lparam >> 16) as u16 as i16);
    let left = x < rect.left + border;
    let right = x >= rect.right - border;
    let top = y < rect.top + border;
    let bottom = y >= rect.bottom - border;
    let edge = match (left, right, top, bottom) {
        (true, _, true, _) => HTTOPLEFT,
        (_, true, true, _) => HTTOPRIGHT,
        (true, _, _, true) => HTBOTTOMLEFT,
        (_, true, _, true) => HTBOTTOMRIGHT,
        (true, ..) => HTLEFT,
        (_, true, ..) => HTRIGHT,
        (_, _, true, _) => HTTOP,
        (_, _, _, true) => HTBOTTOM,
        _ => HTCLIENT,
    };
    edge as LRESULT
}
