//! App: the window, its views and settings, and where it has been.

use super::*;
use crate::gui::app::CloseBy;

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
    b.bind(
        "App",
        "set-log-source",
        Box::new(|app, args| {
            if let Some(Value::String(source)) = args.first() {
                app.set_log_source(source);
            }
        }),
    );
    b.bind(
        "App",
        "cancel-activity",
        Box::new(|app, args| {
            if let Some(Value::String(id)) = args.first() {
                app.cancel_activity(id);
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
        "set-stream-cache",
        Box::new(|app, args| {
            if let Some(Value::String(size)) = args.first() {
                let chosen = match size.as_str() {
                    "auto" => None,
                    gb => match gb.parse::<u32>() {
                        Ok(gb @ 1..=1000) => Some(gb),
                        _ => return,
                    },
                };
                app.settings.stream_cache_gb = chosen;
                crate::stream_cache::apply(chosen);
                app.view.stream_cache_shown = None;
            }
        }),
    );
    b.bind(
        "App",
        "set-stream-quality",
        Box::new(|app, args| {
            if let Some(Value::String(quality)) = args.first()
                && crate::stream_cache::set_quality(quality)
            {
                app.settings.stream_quality = quality.to_string();
            }
        }),
    );
    b.bind(
        "App",
        "clear-stream-cache",
        Box::new(|app, _| {
            crate::stream_cache::clear();
            app.view.stream_cache_shown = None;
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
    b.bind(
        "App",
        "close-window",
        Box::new(|app, _| app.close_window(CloseBy::Button)),
    );
    // The app menu's Exit.
    b.bind("App", "quit", Box::new(|app, _| app.quit()));
}
