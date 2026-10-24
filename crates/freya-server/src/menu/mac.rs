//! The server's icon in the menu bar on macOS. AppKit wants the main thread; what the menu
//! shows is looked at again each time it opens.

#![allow(
    unsafe_code,
    reason = "a menu item's target and action, and the menu's delegate, are plain Objective-C \
              calls, made on the main thread for items this owns"
)]

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSControlStateValueOff, NSControlStateValueOn,
    NSImage, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSVariableStatusItemLength,
    NSWorkspace,
};
use objc2_foundation::{NSObject, NSString};

use super::Actions;

/// Freya's app, found by its identifier wherever it is installed.
const FREYA: &str = "io.github.alexvikman.freya";

/// The items that change.
struct Items {
    open_freya: Retained<NSMenuItem>,
    code: Retained<NSMenuItem>,
    login: Retained<NSMenuItem>,
    update: Retained<NSMenuItem>,
}

struct Ivars {
    actions: Actions,
    items: OnceCell<Items>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `MenuTarget` has no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FreyaServerMenuTarget"]
    #[ivars = Ivars]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    unsafe impl NSMenuDelegate for MenuTarget {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            self.refresh();
        }
    }

    impl MenuTarget {
        #[unsafe(method(openFreya:))]
        fn open_freya(&self, _sender: Option<&AnyObject>) {
            if let Err(e) = std::process::Command::new("open").args(["-b", FREYA]).spawn() {
                log::warn!("could not open Freya: {e}");
            }
        }

        #[unsafe(method(showCode:))]
        fn show_code(&self, _sender: Option<&AnyObject>) {
            if let Some(items) = self.ivars().items.get() {
                let code = self.ivars().actions.connection_code();
                items.code.setTitle(&NSString::from_str(&code));
                items.code.setHidden(false);
            }
        }

        #[unsafe(method(toggleLogin:))]
        fn toggle_login(&self, _sender: Option<&AnyObject>) {
            self.ivars().actions.toggle_start_at_login();
        }

        #[unsafe(method(update:))]
        fn update(&self, _sender: Option<&AnyObject>) {
            self.ivars().actions.update();
        }

        #[unsafe(method(showLog:))]
        fn show_log(&self, _sender: Option<&AnyObject>) {
            self.ivars().actions.show_log();
        }

        #[unsafe(method(stop:))]
        fn stop(&self, _sender: Option<&AnyObject>) {
            self.ivars().actions.stop();
        }
    }
);

impl MenuTarget {
    /// Shows the menu as things are now: Freya to open only when it is installed, whether the
    /// server starts at login, and an update when one is available.
    fn refresh(&self) {
        let Some(items) = self.ivars().items.get() else {
            return;
        };
        let actions = &self.ivars().actions;
        let freya = NSWorkspace::sharedWorkspace()
            .URLForApplicationWithBundleIdentifier(&NSString::from_str(FREYA));
        items.open_freya.setHidden(freya.is_none());
        items.login.setState(if actions.starts_at_login() {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        match actions.updates.offered() {
            Some(offered) => {
                items.update.setTitle(&NSString::from_str(&offered));
                items.update.setHidden(false);
            }
            None => items.update.setHidden(true),
        }
    }
}

/// Shows the icon and runs AppKit on this thread, the main one, for as long as the server
/// runs; none when this is not the main thread, and the server goes on without it.
pub fn run(actions: Actions) -> Option<()> {
    let mtm = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(mtm);
    // An icon in the menu bar, not in the Dock.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let target = MenuTarget::alloc(mtm).set_ivars(Ivars {
        actions,
        items: OnceCell::new(),
    });
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
        item
    };
    item("Freya Server is running", None);
    item(concat!("Version ", env!("CARGO_PKG_VERSION")), None);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let open_freya = item("Open Freya", Some(sel!(openFreya:)));
    item("Show Connection Code", Some(sel!(showCode:)));
    let code = item("", None);
    code.setHidden(true);
    let login = item("Start at Login", Some(sel!(toggleLogin:)));
    let update = item("Update…", Some(sel!(update:)));
    item("Show Log", Some(sel!(showLog:)));
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    item("Stop Server", Some(sel!(stop:)));
    let _ = target.ivars().items.set(Items {
        open_freya,
        code,
        login,
        update,
    });
    target.refresh();
    menu.setDelegate(Some(ProtocolObject::from_ref(&*target)));

    let status = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = status.button(mtm) {
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("music.note.house"),
            Some(&NSString::from_str("Freya Server")),
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
