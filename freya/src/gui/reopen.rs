//! Clicking Freya in the Dock with its window closed opens the window again, as Mac apps do:
//! closing it leaves Freya running, and what plays playing.

#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether Freya was clicked in the Dock with no window open, since this was last asked.
#[cfg(target_os = "macos")]
static REOPENED: AtomicBool = AtomicBool::new(false);

/// Hears Freya clicked in the Dock. Call once the event loop runs, which makes the application's
/// delegate: winit's, which hears no such click, so it is taught to, as Slint teaches it to
/// leave shortcuts unlocalized.
#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "a method added to winit's application delegate, which AppKit calls"
)]
pub fn watch() {
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::sel;

    unsafe extern "C-unwind" fn should_handle_reopen(
        _this: *mut AnyObject,
        _cmd: Sel,
        _app: *mut AnyObject,
        has_visible_windows: Bool,
    ) -> Bool {
        if !has_visible_windows.as_bool() {
            REOPENED.store(true, Ordering::Relaxed);
        }
        Bool::YES
    }

    let sel = sel!(applicationShouldHandleReopen:hasVisibleWindows:);
    match AnyClass::get(c"WinitApplicationDelegate") {
        Some(class) if class.instance_method(sel).is_none() => {
            // SAFETY: the function takes and gives what the selector's signature, "B@:@B", says;
            // the class is winit's, which lives as long as Freya.
            let added = unsafe {
                objc2::ffi::class_addMethod(
                    (class as *const AnyClass).cast_mut(),
                    sel,
                    std::mem::transmute::<
                        unsafe extern "C-unwind" fn(
                            *mut AnyObject,
                            Sel,
                            *mut AnyObject,
                            Bool,
                        ) -> Bool,
                        Imp,
                    >(should_handle_reopen),
                    c"B@:@B".as_ptr(),
                )
            };
            if !added.as_bool() {
                log::warn!("could not hear Freya clicked in the Dock");
            }
        }
        Some(_) => {}
        None => log::warn!("no application delegate to hear Freya clicked in the Dock"),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn watch() {}

/// Whether Freya was clicked in the Dock with no window open since this was last asked.
pub fn take() -> bool {
    #[cfg(target_os = "macos")]
    {
        REOPENED.swap(false, Ordering::Relaxed)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}
