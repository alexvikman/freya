//! Renders a skin to a PNG without opening a window.
//!
//! Usage: cargo run -p freya-skin --example render_skin -- <out.png> [skin | builtin | picker]
//!        [scale] [width] [height]
//!        [library | library-search | albums | songs | most-played | stations | playlist | visualizer
//!        | equalizer | radio | directory | directory-search | error]
//!
//! The last argument shows another of the built-in skin's views than Up Next (the library at an
//! album, its search, its albums, songs or most played, the radio directory's first page or a
//! search of it, the visualizer or the equalizer),
//! internet radio playing, or a track that could not be played.

use std::path::PathBuf;

use freya_skin::SkinSource;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = args
        .first()
        .expect("usage: render_skin <out.png> [skin.slint] [scale]");
    let source = args
        .get(1)
        .filter(|s| *s != "builtin")
        .map_or(SkinSource::Builtin, |p| {
            SkinSource::from_path(&PathBuf::from(p))
        });
    let scale: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let width: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1100);
    let height: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(720);

    // "picker" renders Freya's skin picker instead of a skin.
    let image = if args.get(1).is_some_and(|s| s == "picker") {
        freya_skin::render::init_headless().expect("headless platform");
        let picker = freya_skin::skin_picker_window().expect("skin picker");
        freya_skin::sample::fill_picker(&picker);
        freya_skin::render::render_instance(&picker, width, height, scale).expect("render picker")
    } else {
        let panel = args.get(5).cloned().unwrap_or_default();
        freya_skin::render::render(&source, width, height, scale, |ui| {
            match panel.as_str() {
                "library-search" => freya_skin::sample::fill_library_search(ui),
                "albums" => freya_skin::sample::fill_library_albums(ui),
                "songs" => freya_skin::sample::fill_library_songs(ui),
                "most-played" => freya_skin::sample::fill_library_most_played(ui),
                _ => freya_skin::sample::fill(ui),
            }
            let view = match panel.as_str() {
                "library" | "library-search" | "albums" | "songs" | "most-played" => "library",
                "stations" | "directory" | "directory-search" => "radio",
                "playlist" | "visualizer" | "equalizer" => panel.as_str(),
                _ => "queue",
            };
            let _ = ui.set_property("view", slint_interpreter::Value::String(view.into()));
            match panel.as_str() {
                "radio" => freya_skin::sample::fill_radio(ui),
                "directory" => freya_skin::sample::fill_directory(ui),
                "directory-search" => freya_skin::sample::fill_directory_search(ui),
                "error" => freya_skin::sample::fill_error(ui),
                "welcome" => freya_skin::sample::fill_welcome(ui),
                _ => {}
            }
        })
        .expect("render skin")
    };
    image::RgbImage::from_raw(image.width, image.height, image.pixels)
        .expect("buffer size")
        .save(out)
        .expect("write png");
}
