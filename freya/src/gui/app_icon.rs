//! Freya's name and icon where the system takes them from the running program rather than
//! from an installed package. The name: macOS shows the process's, which is the program's
//! file name, `freya`, in the menu bar and the app menu ("Quit freya"). The icon: the Dock on macOS while Freya runs outside its app bundle (as during
//! development), and the windows' title and task bars on Windows and X11. Wayland desktops
//! show the icon of the installed `.desktop` file instead.

/// Gives Freya its name where macOS shows it: the app menu's items ("Quit Freya") take the
/// process's name, and the menu bar's title the bundle's, which outside an app bundle is made
/// up from the program's file name. Call before the window system starts.
#[cfg(target_os = "macos")]
pub fn set_name() {
    use objc2::runtime::AnyObject;
    use objc2::{ClassType, msg_send};
    use objc2_foundation::{
        NSBundle, NSMutableDictionary, NSObjectProtocol, NSProcessInfo, ns_string,
    };

    NSProcessInfo::processInfo().setProcessName(ns_string!("Freya"));
    if bundled() {
        return;
    }
    let Some(info) = NSBundle::mainBundle().infoDictionary() else {
        return;
    };
    // The info dictionary made up for a program outside a bundle is a mutable one, and AppKit
    // reads the name from it when it sets up the menu bar.
    let mutable = info.isKindOfClass(NSMutableDictionary::<AnyObject, AnyObject>::class());
    if mutable {
        #[allow(
            unsafe_code,
            reason = "a plain message to a dictionary checked to be mutable"
        )]
        // SAFETY: `info` is an NSMutableDictionary, and both arguments are strings.
        unsafe {
            let _: () = msg_send![&*info, setObject: ns_string!("Freya"), forKey: ns_string!("CFBundleName")];
        }
    }
}

/// Whether Freya runs from an app bundle, which names it and gives it its icon.
#[cfg(target_os = "macos")]
fn bundled() -> bool {
    std::env::current_exe().is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"))
}

#[cfg(not(target_os = "macos"))]
pub fn set_name() {}

/// 128 pixels: sharp in the Dock and task bar, and small to keep in the program.
const ICON_PNG: &[u8] = include_bytes!("../../../assets/icons/128x128.png");

/// Shows Freya's icon in the Dock when Freya is not an app bundle, which would give it its
/// own. Call once the event loop runs.
#[cfg(target_os = "macos")]
pub fn show_in_dock() {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if bundled() {
        return;
    }
    let data = NSData::with_bytes(ICON_PNG);
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: a plain setter, given an image made just now, on the main thread.
        #[allow(
            unsafe_code,
            reason = "the binding marks every NSApplication setter unsafe"
        )]
        unsafe {
            app.setApplicationIconImage(Some(&image));
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn show_in_dock() {}

/// Gives a window Freya's icon. Returns false while the window is not open yet. A no-op on
/// macOS, whose windows have none.
pub fn set_window_icon(window: &slint::Window) -> bool {
    #[cfg(not(target_os = "macos"))]
    {
        use slint::winit_030::WinitWindowAccessor;
        use slint::winit_030::winit::window::Icon;

        let icon = match image::load_from_memory_with_format(ICON_PNG, image::ImageFormat::Png) {
            Ok(image) => {
                let image = image.into_rgba8();
                let (width, height) = image.dimensions();
                Icon::from_rgba(image.into_raw(), width, height).ok()
            }
            Err(e) => {
                log::debug!("could not read Freya's icon: {e}");
                None
            }
        };
        window
            .with_winit_window(|w| w.set_window_icon(icon))
            .is_some()
    }
    #[cfg(target_os = "macos")]
    {
        let _ = window;
        true
    }
}
