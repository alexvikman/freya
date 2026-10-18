//! Where the user has been in the window, to go back and forward as in a web browser: from
//! view to view, into an artist or album and out again, through searches, each with how far
//! down it was scrolled.

use super::library::LibraryPlace;

/// Places kept: going back further is rare, and each is small.
const MAX_PLACES: usize = 50;

/// One place the window showed.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    /// The skin's view: "queue", "library", "radio", "playlist", "settings" and so on.
    pub view: String,
    /// In the library: what of it was open.
    pub library: Option<LibraryPlace>,
    /// A playlist shown, by its index.
    pub playlist: Option<usize>,
    /// How far down it was scrolled, in logical pixels.
    pub scroll: f32,
}

impl Place {
    /// The same place, however far it is scrolled.
    fn same(&self, other: &Self) -> bool {
        self.view == other.view && self.library == other.library && self.playlist == other.playlist
    }

    /// `next` goes on with the search this place is: typing a search is one place, not one
    /// for each key.
    fn continued_by(&self, next: &Self) -> bool {
        match (&self.library, &next.library) {
            (Some(a), Some(b)) => self.view == next.view && a.continued_by(b),
            _ => false,
        }
    }
}

#[derive(Debug, Default)]
pub struct History {
    places: Vec<Place>,
    /// The place shown, in `places`.
    at: usize,
}

impl History {
    /// Notes that the window shows `place` now, and that the place shown until now was
    /// scrolled to `scroll`. A new place drops those that going back had left ahead. Returns
    /// whether the place is another than the one shown until now.
    pub fn visit(&mut self, place: Place, scroll: f32) -> bool {
        let Some(current) = self.places.get_mut(self.at) else {
            self.places.push(place);
            self.at = 0;
            return true;
        };
        if current.same(&place) {
            return false;
        }
        if current.continued_by(&place) {
            *current = Place {
                scroll: 0.0,
                ..place
            };
            return true;
        }
        current.scroll = scroll;
        self.places.truncate(self.at + 1);
        self.places.push(Place {
            scroll: 0.0,
            ..place
        });
        if self.places.len() > MAX_PLACES {
            self.places.remove(0);
        }
        self.at = self.places.len() - 1;
        true
    }

    /// The place before the one shown, to show instead; the one shown was scrolled to
    /// `scroll`.
    pub fn back(&mut self, scroll: f32) -> Option<&Place> {
        if !self.can_go_back() {
            return None;
        }
        self.places[self.at].scroll = scroll;
        self.at -= 1;
        self.places.get(self.at)
    }

    /// The place gone back from, to show again.
    pub fn forward(&mut self, scroll: f32) -> Option<&Place> {
        if !self.can_go_forward() {
            return None;
        }
        self.places[self.at].scroll = scroll;
        self.at += 1;
        self.places.get(self.at)
    }

    pub fn can_go_back(&self) -> bool {
        self.at > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.at + 1 < self.places.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(name: &str) -> Place {
        Place {
            view: name.to_owned(),
            library: None,
            playlist: None,
            scroll: 0.0,
        }
    }

    #[test]
    fn goes_back_and_forward_with_the_scroll_of_each_place() {
        let mut history = History::default();
        history.visit(view("queue"), 0.0);
        history.visit(view("radio"), 120.0);
        history.visit(view("settings"), 40.0);
        assert!(!history.can_go_forward());

        let back = history.back(10.0).unwrap();
        assert_eq!((back.view.as_str(), back.scroll), ("radio", 40.0));
        let back = history.back(55.0).unwrap();
        assert_eq!((back.view.as_str(), back.scroll), ("queue", 120.0));
        assert!(history.back(0.0).is_none());

        let forward = history.forward(130.0).unwrap();
        assert_eq!((forward.view.as_str(), forward.scroll), ("radio", 55.0));
        let forward = history.forward(0.0).unwrap();
        assert_eq!((forward.view.as_str(), forward.scroll), ("settings", 10.0));
        assert!(history.forward(0.0).is_none());
    }

    #[test]
    fn a_new_place_after_going_back_drops_those_ahead() {
        let mut history = History::default();
        for name in ["queue", "radio", "settings"] {
            history.visit(view(name), 0.0);
        }
        history.back(0.0);
        history.back(0.0);
        history.visit(view("about"), 0.0);
        assert!(!history.can_go_forward());
        assert_eq!(history.back(0.0).unwrap().view, "queue");
    }

    #[test]
    fn the_same_place_again_is_not_a_new_one() {
        let mut history = History::default();
        history.visit(view("queue"), 0.0);
        history.visit(view("queue"), 300.0);
        assert!(!history.can_go_back());
    }

    #[test]
    fn keeps_only_the_latest_places() {
        let mut history = History::default();
        for i in 0..(MAX_PLACES + 10) {
            history.visit(view(&i.to_string()), 0.0);
        }
        let mut back = 0;
        while history.back(0.0).is_some() {
            back += 1;
        }
        assert_eq!(back, MAX_PLACES - 1);
    }
}
