//! The server's icon in the menu bar on macOS: that it runs, and its version, with Freya to
//! open, its log to show, and stopping it. AppKit wants the main thread, so the server's work
//! goes on the others while this runs there.

#![allow(
    unsafe_code,
    reason = "a menu item's target and action are plain Objective-C calls, made on the main \
              thread for items this owns"
)]

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSStatusBar,
    NSVariableStatusItemLength,
};
use objc2_foundation::{NSObject, NSString};

/// What the menu's items do.
pub struct Actions {
    /// Stops the server, as Ctrl-C does.
    pub stop: Sender<()>,
    pub log: PathBuf,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `MenuTarget` has no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FreyaServerMenuTarget"]
    #[ivars = Actions]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(openFreya:))]
        fn open_freya(&self, _sender: Option<&AnyObject>) {
            open_freya();
        }

        #[unsafe(method(showLog:))]
        fn show_log(&self, _sender: Option<&AnyObject>) {
            let _ = std::process::Command::new("open")
                .arg(&self.ivars().log)
                .spawn();
        }

        #[unsafe(method(stop:))]
        fn stop(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().stop.send(());
        }
    }
);

/// Shows the icon and runs AppKit on this thread, the main one, for as long as the server
/// runs; none when this is not the main thread, and the server goes on without it.
pub fn run(actions: Actions) -> Option<()> {
    let mtm = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(mtm);
    // An icon in the menu bar, not in the Dock.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let target = MenuTarget::alloc(mtm).set_ivars(actions);
    // SAFETY: NSObject's own initializer.
    let target: Retained<MenuTarget> = unsafe { msg_send![super(target), init] };

    let menu = NSMenu::new(mtm);
    // Items without an action show greyed out, as headings.
    let item = |title: &str, action: Option<Sel>| {
        // SAFETY: a new item with a title, and an action `MenuTarget` answers.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                action,
                &NSString::from_str(""),
            )
        };
        if action.is_some() {
            // SAFETY: the target lives as long as the menu, until the server stops.
            unsafe { item.setTarget(Some(&target)) };
        }
        menu.addItem(&item);
    };
    item("Freya's server is running", None);
    item(concat!("Version ", env!("CARGO_PKG_VERSION")), None);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    item("Open Freya", Some(sel!(openFreya:)));
    item("Show Log", Some(sel!(showLog:)));
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    item("Stop Server", Some(sel!(stop:)));

    let status = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = status.button(mtm) {
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("music.note.house"),
            Some(&NSString::from_str("Freya's server")),
        );
        if let Some(image) = image {
            // Drawn in the menu bar's own colors, light or dark.
            image.setTemplate(true);
            button.setImage(Some(&image));
        }
    }
    status.setMenu(Some(&menu));
    app.run();
    Some(())
}

/// Opens Freya: the app the server is in, or the program beside it.
fn open_freya() {
    let Ok(server) = std::env::current_exe() else {
        return;
    };
    // …/Freya.app/Contents/MacOS/freya-server
    let bundle = server
        .ancestors()
        .nth(3)
        .filter(|b| b.extension().is_some_and(|e| e == "app"));
    let opened = match bundle {
        Some(bundle) => std::process::Command::new("open").arg(bundle).spawn(),
        None => std::process::Command::new(server.with_file_name("freya")).spawn(),
    };
    if let Err(e) = opened {
        log::warn!("could not open Freya: {e}");
    }
}
