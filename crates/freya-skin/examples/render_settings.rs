//! Renders the built-in skin's settings with a sample server and account, to look at:
//! `cargo run -p freya-skin --example render_settings -- out.png [remote]`.
use freya_skin::SkinSource;
use slint_interpreter::{SharedString, Struct, Value};

fn main() {
    let remote = std::env::args().nth(2).as_deref() == Some("remote");
    let out = std::env::args().nth(1).expect("where to write the picture");
    let image = freya_skin::render::render(&SkinSource::Builtin, 1100, 1400, 1.0, |ui| {
        freya_skin::sample::fill(ui);
        let set = |global: &str, name: &str, value: Value| {
            ui.set_global_property(global, name, value).unwrap();
        };
        let s = |t: &str| Value::String(SharedString::from(t));
        set("App", "view", s("settings"));
        set("Server", "remote", Value::Bool(remote));
        set("Server", "name", s("Mini"));
        set("Server", "address", s("192.168.1.20:47475"));
        set("Server", "state", s("connected"));
        set(
            "Server",
            "fingerprint",
            s("CBE7 A98B 0C8B 2BA6 0D06 0A74 9F09 B8CB 26A5 3B90 0405 2057 F096 3831 8A3A 391E"),
        );
        set("Server", "server-name", s("Mac-mini"));
        set("Server", "network", Value::Bool(true));
        set("Server", "user", s("alexander"));
        set("Server", "has-password", Value::Bool(false));
        let device = |id: &str, name: &str, this: bool| {
            Value::Struct(Struct::from_iter([
                ("id".to_owned(), s(id)),
                ("name".to_owned(), s(name)),
                ("connected".to_owned(), s("2026-10-20")),
                ("used".to_owned(), s("2026-10-22")),
                (
                    "joined".to_owned(),
                    s(if this { "paired" } else { "logged-in" }),
                ),
                ("this".to_owned(), Value::Bool(this)),
            ]))
        };
        set(
            "Server",
            "devices",
            Value::Model(slint::ModelRc::new(slint::VecModel::from(vec![
                device("a", "MacBook-Pro.local", true),
                device("b", "WINDOWS-PC", false),
            ]))),
        );
    })
    .unwrap();
    let picture = image::RgbImage::from_raw(image.width, image.height, image.pixels).unwrap();
    picture.save(out).unwrap();
}
