//! App: the window, its views and settings, and where it has been.

use super::*;

pub(super) fn bind(b: &Binder) {
    b.bind("App", "show-about", Box::new(|app, _| app.show_about()));
    b.bind(
        "App",
        "set-log-level",
        Box::new(|app, args| {
            if let Some(Value::String(level)) = args.first() {
                app.set_log_level(level);
            }
        }),
    );
    b.bind("App", "go-back", Box::new(|app, _| app.go_back()));
    b.bind("App", "go-forward", Box::new(|app, _| app.go_forward()));
    // The skin changed its view; noted after every callback.
    b.bind("App", "navigated", Box::new(|_, _| {}));
    b.bind(
        "App",
        "open-link",
        Box::new(|_, args| {
            let Some(Value::String(name)) = args.first() else {
                return;
            };
            // Built into Freya, not a page on the web.
            if name.as_str() == "licenses" {
                crate::update::open_licenses();
            } else if let Some(url) = crate::update::page(name) {
                crate::update::open_page(&url);
            }
        }),
    );
    {
        // Opening the skin picker needs the shared app, to wire up the picker's callbacks.
        let app = b.weak.clone();
        let _ = b.ui.set_global_callback("App", "show-skins", move |_| {
            if let Some(app) = app.upgrade() {
                skins::show_picker(&app);
            }
            Value::Void
        });
    }
    b.bind(
        "App",
        "set-theme",
        Box::new(|app, args| {
            if let Some(Value::String(theme)) = args.first()
                && ["system", "light", "dark"].contains(&theme.as_str())
            {
                app.settings.theme = theme.to_string();
            }
        }),
    );
    b.bind(
        "App",
        "set-close-minimizes",
        Box::new(|app, args| {
            if let Some(Value::Bool(minimizes)) = args.first() {
                app.settings.close_minimizes = *minimizes;
            }
        }),
    );
    b.bind(
        "App",
        "toggle-equalizer",
        Box::new(|app, _| app.settings.equalizer_visible = !app.settings.equalizer_visible),
    );
    {
        // Moving the window runs until the mouse is let go: the app is not borrowed meanwhile,
        // so it keeps playing and updating.
        let app = b.weak.clone();
        let _ = b.ui.set_global_callback("App", "drag-window", move |_| {
            let ui = app
                .upgrade()
                .and_then(|app| app.try_borrow().ok().map(|app| app.view.ui.clone()));
            if let Some(ui) = ui {
                title_bar::drag(ui.window());
            }
            Value::Void
        });
    }
    // The window buttons a frameless skin draws. Minimizing and maximizing resize the window
    // at once, which the skin follows: not while the app is borrowed.
    for (name, act) in [
        (
            "minimize-window",
            (|window: &slint::Window| window.set_minimized(true)) as fn(&slint::Window),
        ),
        ("toggle-maximize-window", |window| {
            window.set_maximized(!window.is_maximized());
        }),
    ] {
        let app = b.weak.clone();
        let _ = b.ui.set_global_callback("App", name, move |_| {
            let ui = app
                .upgrade()
                .and_then(|app| app.try_borrow().ok().map(|app| app.view.ui.clone()));
            if let Some(ui) = ui {
                act(ui.window());
            }
            Value::Void
        });
    }
    // As the system's close button.
    {
        let app = b.weak.clone();
        let _ = b.ui.set_global_callback("App", "close-window", move |_| {
            let hide = app.upgrade().and_then(|app| {
                let mut app = app.try_borrow_mut().ok()?;
                let response = app.close_window();
                Some((response, app.view.ui.clone()))
            });
            match hide {
                Some((slint::CloseRequestResponse::HideWindow, ui)) => {
                    let _ = ui.hide();
                }
                Some(_) => {}
                None => {
                    let _ = slint::quit_event_loop();
                }
            }
            Value::Void
        });
    }
    // The app menu's Exit.
    let _ = b.ui.set_global_callback("App", "quit", |_| {
        let _ = slint::quit_event_loop();
        Value::Void
    });
}
