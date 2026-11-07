//! The built-in skin's library pages scroll back exactly where they were, going back: on an
//! artist's page, whose heading is taller than its rows, and in a plain list.

use freya_skin::SkinSource;
use slint::{Model, ModelRc, SharedString, VecModel};
use slint_interpreter::{ComponentInstance, Struct, Value};

fn text(s: &str) -> Value {
    Value::String(SharedString::from(s))
}

/// The library's first row as the sample has it, as a model of `n` rows of `kind`; in a
/// discography each album has three tracks after it.
fn rows(ui: &ComponentInstance, kind: &str, n: usize) -> Value {
    let Value::Model(sample) = ui.get_global_property("Library", "rows").unwrap() else {
        panic!("the sample has rows");
    };
    let Value::Struct(template) = sample.row_data(0).unwrap() else {
        panic!("a row is a struct");
    };
    let rows: Vec<Value> = (0..n)
        .map(|i| {
            let mut row = template.clone();
            let (kind, albums_before) = match kind {
                "discography" if i % 4 == 0 => ("album", i / 4),
                "discography" => ("track", i / 4 + 1),
                kind => (kind, 0),
            };
            row.set_field("kind".into(), text(kind));
            row.set_field("title".into(), text(&format!("{kind} {i}")));
            row.set_field("albums-before".into(), Value::Number(albums_before as f64));
            Value::Struct(row)
        })
        .collect();
    Value::Model(ModelRc::new(VecModel::from(rows)))
}

struct Pages {
    ui: ComponentInstance,
    artist_page: Struct,
    album_page: Struct,
    opens: f64,
    restores: f64,
}

impl Pages {
    fn new() -> Self {
        freya_skin::render::init_headless().unwrap();
        let ui = freya_skin::load(&SkinSource::Builtin).unwrap();
        freya_skin::sample::fill_library_album(&ui);
        ui.set_global_property("App", "view", text("library"))
            .unwrap();
        let page = |name| match ui.get_global_property("Library", name).unwrap() {
            Value::Struct(page) => page,
            _ => panic!("{name} is a struct"),
        };
        let (artist_page, album_page) = (page("artist-page"), page("album-page"));
        Self {
            ui,
            artist_page,
            album_page,
            opens: 0.0,
            restores: 0.0,
        }
    }

    /// Shows `rows` as the app does a place it opened, or went back to, scrolled to `back`.
    fn open(&mut self, rows: Value, layout: &str, artist: bool, back: Option<f64>) {
        let set = |name: &str, value| self.ui.set_global_property("Library", name, value).unwrap();
        set("rows", rows);
        set("layout", text(layout));
        let mut page = self.artist_page.clone();
        page.set_field("shown".into(), Value::Bool(artist));
        set("artist-page", Value::Struct(page));
        let mut page = self.album_page.clone();
        page.set_field("shown".into(), Value::Bool(layout == "list" && !artist));
        set("album-page", Value::Struct(page));
        let app = |name: &str, value| self.ui.set_global_property("App", name, value).unwrap();
        match back {
            Some(scroll) => {
                self.restores += 1.0;
                app("restore-view", text("library"));
                app("restore-scroll", Value::Number(scroll));
                app("restores", Value::Number(self.restores));
            }
            None => app("restore-view", text("")),
        }
        self.opens += 1.0;
        set("opens", Value::Number(self.opens));
        // Laid out, its timers run, and laid out again, as frames go by.
        freya_skin::render::render_instance(&self.ui, 1100, 720, 1.0).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        slint::platform::update_timers_and_animations();
        freya_skin::render::render_instance(&self.ui, 1100, 720, 1.0).unwrap();
    }

    fn scroll(&self) -> f64 {
        match self.ui.get_global_property("App", "scroll").unwrap() {
            Value::Number(scroll) => scroll,
            other => panic!("App.scroll is {other:?}"),
        }
    }
}

#[test]
fn library_pages_scroll_back_where_they_were() {
    let mut pages = Pages::new();
    for (kind, layout, artist) in [
        ("album", "grid", true),
        ("discography", "list", true),
        ("artist", "list", false),
    ] {
        for scrolled in [1500.0, 5000.0] {
            let place = |pages: &Pages| rows(&pages.ui, kind, 400);
            let first = place(&pages);
            pages.open(first, layout, artist, None);
            // Scrolled down, as the history restores it.
            let again = place(&pages);
            pages.open(again, layout, artist, Some(scrolled));
            assert_eq!(pages.scroll(), scrolled, "{kind} scrolled down");
            // An album opened from it, shown from its top.
            let tracks = rows(&pages.ui, "track", 10);
            pages.open(tracks, "list", false, None);
            assert_eq!(pages.scroll(), 0.0, "an album from {kind}");
            // And back.
            let back = place(&pages);
            pages.open(back, layout, artist, Some(scrolled));
            assert_eq!(pages.scroll(), scrolled, "back to {kind}");
        }
    }
}
