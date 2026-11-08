//! Renders the built-in skin's Organize view with sample folders, to look at:
//! `cargo run -p freya-skin --example render_organize -- out.png`.
use freya_skin::SkinSource;
use slint_interpreter::{SharedString, Struct, Value};

fn s(text: &str) -> Value {
    Value::String(SharedString::from(text))
}

fn row(fields: &[(&str, Value)]) -> Value {
    Value::Struct(Struct::from_iter(
        fields
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone())),
    ))
}

fn list(rows: Vec<Value>) -> Value {
    Value::Model(slint::ModelRc::new(slint::VecModel::from(rows)))
}

fn folder(title: &str, state: &str, selected: bool) -> Value {
    row(&[
        ("kind", s("folder")),
        ("title", s(title)),
        ("detail", s("8 files")),
        ("state", s(state)),
        ("key", s(title)),
        ("selected", Value::Bool(selected)),
    ])
}

fn field(label: &str, current: &str, draft: &str, several: bool) -> Value {
    row(&[
        ("field", s(label)),
        ("label", s(label)),
        ("current", s(current)),
        ("draft", s(draft)),
        ("changed", Value::Bool(current != draft)),
        ("several", Value::Bool(several)),
    ])
}

fn file(number: &str, title: &str, new_title: &str) -> Value {
    row(&[
        ("key", s(title)),
        ("number", s(number)),
        ("title", s(title)),
        ("new-title", s(new_title)),
        ("file", s(&format!("{number} {title}.flac"))),
        ("length", s("5:12")),
        ("changed", Value::Bool(title != new_title)),
        ("selected", Value::Bool(false)),
        ("note", s("")),
    ])
}

fn main() {
    let out = std::env::args().nth(1).expect("where to write the picture");
    let image = freya_skin::render::render(&SkinSource::Builtin, 1280, 860, 1.0, |ui| {
        freya_skin::sample::fill(ui);
        let set = |name: &str, value: Value| {
            ui.set_global_property("Organize", name, value).unwrap();
        };
        ui.set_global_property("App", "view", s("organize"))
            .unwrap();
        ui.set_global_property("Server", "admin", Value::Bool(true))
            .unwrap();
        set(
            "summary",
            s("212 folders · 38 not identified · 3 to review"),
        );
        set(
            "rows",
            list(vec![
                row(&[
                    ("kind", s("group")),
                    ("title", s("Metallica")),
                    ("detail", s("")),
                    ("state", s("")),
                    ("key", s("")),
                    ("selected", Value::Bool(false)),
                ]),
                folder(
                    "(1983) Kill 'Em All (Remastered Deluxe Edition)",
                    "unidentified",
                    false,
                ),
                folder("(1986) Master Of Puppets (2016 Remastered)", "ready", true),
                folder(
                    "(1986) Master Of Puppets (2017 Remastered Deluxe Edition)",
                    "review",
                    false,
                ),
                folder("(1991) Metallica", "identified", false),
            ]),
        );
        set("title", s("(1986) Master Of Puppets (2016 Remastered)"));
        set(
            "detail",
            s("/Volumes/Data/music/Metallica/(1986) Master Of Puppets (2016 Remastered)"),
        );
        set("can-save", Value::Bool(true));
        set("can-undo", Value::Bool(true));
        set("scope", s("All 8 files"));
        set(
            "fields",
            list(vec![
                field(
                    "Album",
                    "Master Of Puppets (Remastered)",
                    "Master of Puppets",
                    false,
                ),
                field("Album artist", "Metallica", "Metallica", false),
                field("Edition", "", "2016 Remastered", false),
                field("Genre", "", "", true),
                field("Date", "2016", "2016", false),
                field("Original date", "", "1986", false),
            ]),
        );
        set(
            "files",
            list(vec![
                file("1", "Battery", "Battery"),
                file("2", "Master Of Puppets (Remastered)", "Master of Puppets"),
                file(
                    "3",
                    "The Thing That Should Not Be",
                    "The Thing That Should Not Be",
                ),
            ]),
        );
    })
    .unwrap();
    let picture = image::RgbImage::from_raw(image.width, image.height, image.pixels).unwrap();
    picture.save(out).unwrap();
}
