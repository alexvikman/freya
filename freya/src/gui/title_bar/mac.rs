//! AppKit's title bar, made transparent with the window's content reaching under it, and its
//! window buttons moved into the skin's top bar. AppKit lays the buttons out again whenever
//! the window changes, so they are moved back after each change.

#![allow(
    unsafe_code,
    reason = "a view's superview and a window's view are unsafe in the AppKit bindings, and \
              adding a method and delaying an action are plain runtime calls; all only for a \
              window that is open, on the main thread"
)]

use std::ffi::CString;

use objc2::encode::Encode;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, Imp, Sel};
use objc2::{MainThreadMarker, msg_send, sel};
use objc2_app_kit::{
    NSButton, NSView, NSWindow, NSWindowButton, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{NSPoint, NSString, NSUserDefaults};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::{EventResult, WinitWindowAccessor};

/// Where the close button starts, from the window's left edge, in points.
const BUTTONS_X: f64 = 20.0;
/// Room between the zoom button and the skin's own content, in points.
const BUTTONS_MARGIN: f64 = 16.0;

/// Draws the skin into the title bar when `height` is above 0, with the buttons centred in a
/// top bar that high; otherwise gives the window the system's title bar back. Returns how much
/// taller the window's content got, or `None` when the window is not open yet.
pub fn set_height(window: &slint::Window, height: f32) -> Option<f32> {
    let target = ns_window(window)?;
    let mtm = MainThreadMarker::new()?;
    let full = NSWindowStyleMask::FullSizeContentView.0;
    let mask = target.styleMask().0;
    let normal = NSWindowStyleMask(mask & !full);
    // What the window's content was under the system's title bar. AppKit keeps the content's
    // size as the style changes, so the window may just get lower.
    let before = NSWindow::contentRectForFrameRect_styleMask(target.frame(), normal, mtm);
    let unified = height > 0.0;
    target.setTitlebarAppearsTransparent(unified);
    target.setTitleVisibility(if unified {
        NSWindowTitleVisibility::Hidden
    } else {
        NSWindowTitleVisibility::Visible
    });
    target.setStyleMask(if unified {
        NSWindowStyleMask(mask | full)
    } else {
        normal
    });
    if !unified {
        window.on_winit_window_event(|_, _| EventResult::Propagate);
        return Some(0.0);
    }
    let height = f64::from(height);
    keep_presses_in_view(&target);
    place_buttons(&target, height);
    // Now the content is the whole window.
    let after = target.frame();
    window.on_winit_window_event(move |window, event| {
        if matches!(
            event,
            WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::Focused(_)
                | WindowEvent::ThemeChanged(_)
        ) && let Some(ns_window) = ns_window(window)
        {
            place_buttons(&ns_window, height);
        }
        EventResult::Propagate
    });
    Some((after.size.height - before.size.height) as f32)
}

/// The room the window buttons take from the window's left edge, in points; 0 in full screen,
/// where they are not shown.
///
/// Puts the buttons back first if AppKit laid them out again since: it may after the window
/// changes, e.g. once a zoom's animation is over, after the events that `set_height` follows.
pub fn buttons_width(window: &slint::Window, height: f32) -> f32 {
    let Some(ns_window) = ns_window(window) else {
        return 0.0;
    };
    let full_screen = NSWindowStyleMask::FullScreen.0;
    if ns_window.styleMask().0 & full_screen != 0 {
        return 0.0;
    }
    if moved(&ns_window, f64::from(height)) {
        place_buttons(&ns_window, f64::from(height));
    }
    match buttons(&ns_window) {
        Some([_, _, zoom]) => {
            let frame = zoom.frame();
            (frame.origin.x + frame.size.width + BUTTONS_MARGIN) as f32
        }
        None => 0.0,
    }
}

/// Moves the window as the mouse moves, through AppKit's own window dragging; a double click
/// does what the system's settings say (Desktop & Dock: "Double-click a window's title bar
/// to").
pub fn drag(window: &slint::Window) {
    let Some(ns_window) = ns_window(window) else {
        return;
    };
    let Some(event) = ns_window.currentEvent() else {
        return;
    };
    if event.clickCount() < 2 {
        ns_window.performWindowDragWithEvent(&event);
        return;
    }
    let key = NSString::from_str("AppleActionOnDoubleClick");
    let action = NSUserDefaults::standardUserDefaults()
        .stringForKey(&key)
        .map(|action| action.to_string());
    match action.as_deref() {
        Some("Minimize") => after_event(&ns_window, sel!(performMiniaturize:)),
        Some("None") => {}
        _ => after_event(&ns_window, sel!(performZoom:)),
    }
}

/// Has AppKit run `action` (`performZoom:` or `performMiniaturize:`) on the window just after
/// the mouse event in hand. Run from within it, its animation would play while winit still
/// handles the event, which keeps the skin from being redrawn until the animation is over.
fn after_event(ns_window: &NSWindow, action: Sel) {
    // SAFETY: `performSelector:withObject:afterDelay:` exists on every NSObject; the actions
    // take a sender, which may be nil.
    let _: () = unsafe {
        msg_send![ns_window, performSelector: action, withObject: None::<&AnyObject>, afterDelay: 0.0f64]
    };
}

/// Tells AppKit that presses on the window's content never move the window. winit's view does
/// not say, and AppKit's answer for a transparent view is that they do: in the title bar, now
/// as high as the skin's top bar, a double click on any of the skin's buttons would then zoom
/// the window. The skin's empty room still moves it, through `drag`.
fn keep_presses_in_view(ns_window: &NSWindow) {
    extern "C-unwind" fn no(_view: &AnyObject, _cmd: Sel) -> Bool {
        Bool::NO
    }
    let Some(view) = ns_window.contentView() else {
        return;
    };
    let Ok(types) = CString::new(format!("{}@:", Bool::ENCODING)) else {
        return;
    };
    // SAFETY: `no` has the signature of `mouseDownCanMoveWindow` (self, _cmd, BOOL result),
    // and `types` says so. Adding it to winit's view class only overrides NSView's answer; it
    // is ignored when the class already has it, as on a second window.
    unsafe {
        let imp: Imp =
            std::mem::transmute::<extern "C-unwind" fn(&AnyObject, Sel) -> Bool, Imp>(no);
        let class = std::ptr::from_ref(view.class()).cast_mut();
        objc2::ffi::class_addMethod(class, sel!(mouseDownCanMoveWindow), imp, types.as_ptr());
    }
}

/// Puts the buttons at `BUTTONS_X`, centred in a top bar `height` points high.
fn place_buttons(ns_window: &NSWindow, height: f64) {
    let Some([close, minimize, zoom]) = buttons(ns_window) else {
        return;
    };
    // SAFETY: views of an open window, on the main thread.
    let Some(title_bar) = (unsafe { close.superview() }) else {
        return;
    };
    // The title bar's container decides how high the buttons can go: make it as high as the
    // skin's top bar.
    if let Some(container) = unsafe { title_bar.superview() } {
        let mut frame = container.frame();
        frame.size.height = height;
        frame.origin.y = ns_window.frame().size.height - height;
        container.setFrame(frame);
    }
    let bar_height = title_bar.frame().size.height;
    let spacing = minimize.frame().origin.x - close.frame().origin.x;
    for (i, button) in [close, minimize, zoom].iter().enumerate() {
        let y = ((bar_height - button.frame().size.height) / 2.0).round();
        button.setFrameOrigin(NSPoint::new(BUTTONS_X + i as f64 * spacing, y));
    }
}

/// Whether the buttons are no longer where `place_buttons` put them.
fn moved(ns_window: &NSWindow, height: f64) -> bool {
    let Some([close, ..]) = buttons(ns_window) else {
        return false;
    };
    // SAFETY: views of an open window, on the main thread.
    let container = unsafe { close.superview().and_then(|bar| bar.superview()) };
    let container_height = container.map_or(height, |c| c.frame().size.height);
    (close.frame().origin.x - BUTTONS_X).abs() > 0.5 || (container_height - height).abs() > 0.5
}

fn buttons(ns_window: &NSWindow) -> Option<[Retained<NSButton>; 3]> {
    Some([
        ns_window.standardWindowButton(NSWindowButton::CloseButton)?,
        ns_window.standardWindowButton(NSWindowButton::MiniaturizeButton)?,
        ns_window.standardWindowButton(NSWindowButton::ZoomButton)?,
    ])
}

fn ns_window(window: &slint::Window) -> Option<Retained<NSWindow>> {
    window.with_winit_window(ns_window_of_winit).flatten()
}

fn ns_window_of_winit(
    window: &slint::winit_030::winit::window::Window,
) -> Option<Retained<NSWindow>> {
    let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: the pointer is the live NSView of a window that is open, on the main thread.
    let view: Retained<NSView> = unsafe { Retained::retain(handle.ns_view.as_ptr().cast()) }?;
    view.window()
}
