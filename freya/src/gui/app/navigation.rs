//! Back and forward, through the views and places the window has shown.

use super::*;

impl App {
    /// The place the window shows, by the skin's `App.view` and what the app has open; `None`
    /// for skins without views.
    pub(super) fn place(&self) -> Option<Place> {
        let Ok(Value::String(view)) = self.view.ui.get_global_property("App", "view") else {
            return None;
        };
        let view = view.to_string();
        if view.is_empty() {
            return None;
        }
        Some(Place {
            library: (view == "library").then(|| self.library.place()),
            playlist: self.shown_playlist.filter(|_| view == "playlist"),
            view,
            scroll: 0.0,
        })
    }

    /// How far down the skin says the place shown is scrolled.
    pub(super) fn shown_scroll(&self) -> f32 {
        match self.view.ui.get_global_property("App", "scroll") {
            Ok(Value::Number(scroll)) => scroll as f32,
            _ => 0.0,
        }
    }

    /// Notes the place shown in the history, after anything the skin asked for.
    pub fn note_place(&mut self) {
        let Some(place) = self.place() else {
            return;
        };
        let scroll = self.shown_scroll();
        if self.history.visit(place, scroll) {
            // A place reached anew starts at its top, not where a restored one was.
            let _ = self
                .view
                .ui
                .set_global_property("App", "restore-view", skin_state::string(""));
        }
    }

    pub fn go_back(&mut self) {
        let scroll = self.shown_scroll();
        if let Some(place) = self.history.back(scroll).cloned() {
            self.show_place(&place);
        }
    }

    pub fn go_forward(&mut self) {
        let scroll = self.shown_scroll();
        if let Some(place) = self.history.forward(scroll).cloned() {
            self.show_place(&place);
        }
    }

    /// Shows `place` again: the library or playlist as it was, the skin's view, and how far
    /// down it was scrolled.
    /// Shows the skin's `view`, as the user would by choosing it.
    pub fn show_view(&mut self, view: &str) {
        let _ = self
            .view
            .ui
            .set_global_property("App", "view", skin_state::string(view));
    }

    pub(super) fn show_place(&mut self, place: &Place) {
        if let Some(library) = &place.library {
            self.library.go_to(library);
        }
        if place.view == "playlist" {
            self.shown_playlist = place.playlist;
        }
        self.restores += 1;
        let ui = &self.view.ui;
        let scroll = Value::Number(f64::from(place.scroll));
        let _ = ui.set_global_property("App", "view", skin_state::string(&place.view));
        let _ = ui.set_global_property("App", "scroll", scroll.clone());
        let _ = ui.set_global_property("App", "restore-scroll", scroll);
        let _ = ui.set_global_property("App", "restore-view", skin_state::string(&place.view));
        let _ = ui.set_global_property("App", "restores", Value::Number(f64::from(self.restores)));
    }
}
