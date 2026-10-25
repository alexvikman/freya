//! Freya's icon in the notification area on Windows, where closing the window leaves Freya
//! playing, as music apps do there: a click opens the window again, and its menu plays, skips
//! and quits. Shown while closing the window keeps Freya running.

use std::cell::RefCell;
use std::rc::Weak;

use slint_interpreter::{ComponentInstance, Value};

use super::app::App;

/// What a click in the icon's menu does.
type Action = fn(&mut App);

pub struct Tray {
    instance: ComponentInstance,
    /// Whether it is shown, and whether something plays, as last told.
    shown: Option<(bool, bool)>,
}

impl Tray {
    /// The icon, not shown yet, doing what it is asked to `app`; on Windows only.
    pub fn new(app: &Weak<RefCell<App>>) -> Option<Self> {
        if !cfg!(windows) {
            return None;
        }
        let instance = match freya_skin::tray_icon() {
            Ok(instance) => instance,
            Err(e) => {
                log::warn!("no icon in the notification area: {e}");
                return None;
            }
        };
        if let Some(icon) = super::app_icon::image() {
            let _ = instance.set_property("icon", Value::Image(icon));
        }
        let _ = instance.set_property("visible", Value::Bool(false));
        let actions: [(&str, Action); 5] = [
            ("open", App::open_window),
            ("toggle-pause", App::toggle_pause),
            ("next", |app| app.step(true)),
            ("previous", |app| app.step(false)),
            ("quit", App::quit),
        ];
        for (name, action) in actions {
            let app = app.clone();
            let _ = instance.set_callback(name, move |_| {
                if let Some(app) = app.upgrade()
                    && let Ok(mut app) = app.try_borrow_mut()
                {
                    action(&mut app);
                    app.refresh();
                }
                Value::Void
            });
        }
        Some(Self {
            instance,
            shown: None,
        })
    }

    /// Shows the icon or not, and whether something plays, for its menu's Play or Pause.
    pub fn update(&mut self, shown: bool, playing: bool) {
        if self.shown == Some((shown, playing)) {
            return;
        }
        self.shown = Some((shown, playing));
        let _ = self.instance.set_property("visible", Value::Bool(shown));
        let _ = self.instance.set_property("playing", Value::Bool(playing));
    }
}
