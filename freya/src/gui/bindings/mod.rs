//! Wiring a skin's callbacks to the app.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use crate::eq_presets::{builtin_presets, read_eqf, write_eqf};
use freya_media::playlist_file;

use crate::playlist::{Playlist, SelectMode};
use slint::Model;
use slint_interpreter::Value;

use super::app::App;
use super::equalizer::{self, EqualizerSettings};
use super::library::AlbumSort;
use super::skin_window::SkinWindow;
use super::{library, skin_state, skins, title_bar, visualizer};
use crate::update;

mod browse;
mod eq;
mod lastfm;
mod player;
mod playlists;
mod queue;
mod radio;
mod server;
mod updates;
mod visuals;
mod window;

/// What a skin callback does to the app, given the callback's arguments.
type Action = Box<dyn Fn(&mut App, &[Value])>;

/// Wires a skin window's callbacks to the app, which it holds weakly, so a replaced skin
/// window does not keep it, or itself, alive.
struct Binder<'a> {
    ui: &'a SkinWindow,
    weak: Weak<RefCell<App>>,
}

impl Binder<'_> {
    /// Has `global`'s callback `name` do `action`, then refresh the skin.
    fn bind(&self, global: &str, name: &str, action: Action) {
        let app = self.weak.clone();
        let result = self.ui.set_global_callback(global, name, move |args| {
            if let Some(app) = app.upgrade()
                && let Ok(mut app) = app.try_borrow_mut()
            {
                action(&mut app, args);
                app.note_place();
                app.refresh();
            }
            Value::Void
        });
        if result.is_err() {
            log::debug!("skin does not use {global}.{name}");
        }
    }

    /// For a callback that opens a file dialog: it picks first and only then borrows the app,
    /// so the dialog's own event handling never finds the app borrowed.
    fn dialog(
        &self,
        global: &str,
        name: &str,
        pick: fn() -> Option<Vec<PathBuf>>,
        action: fn(&mut App, Vec<PathBuf>),
    ) {
        let app = self.weak.clone();
        let _ = self.ui.set_global_callback(global, name, move |_| {
            if let Some(paths) = pick()
                && let Some(app) = app.upgrade()
                && let Ok(mut app) = app.try_borrow_mut()
            {
                action(&mut app, paths);
                app.refresh();
            }
            Value::Void
        });
    }
}

/// Wires `ui`'s callbacks to the app, by the skin API's globals.
pub fn connect(ui: &SkinWindow, app: &Rc<RefCell<App>>) {
    let b = Binder {
        ui,
        weak: Rc::downgrade(app),
    };
    player::bind(&b);
    queue::bind(&b);
    browse::bind(&b);
    playlists::bind(&b);
    radio::bind(&b);
    server::bind(&b);
    lastfm::bind(&b);
    window::bind(&b);
    updates::bind(&b);
    visuals::bind(&b);
    eq::bind(&b);
}

pub fn number(args: &[Value], i: usize) -> f64 {
    match args.get(i) {
        Some(Value::Number(n)) => *n,
        _ => 0.0,
    }
}

pub fn index(args: &[Value], i: usize) -> usize {
    number(args, i).max(0.0) as usize
}

fn flag(args: &[Value], i: usize) -> bool {
    matches!(args.get(i), Some(Value::Bool(true)))
}

/// A text argument, or empty.
fn text_arg(args: &[Value], i: usize) -> String {
    match args.get(i) {
        Some(Value::String(text)) => text.to_string(),
        _ => String::new(),
    }
}

/// A list of numbers passed from the skin.
fn numbers(args: &[Value], i: usize) -> Vec<f32> {
    match args.get(i) {
        Some(Value::Model(model)) => model
            .iter()
            .map(|v| match v {
                Value::Number(n) => n as f32,
                _ => 0.0,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn pick_audio() -> Option<Vec<PathBuf>> {
    // CUE sheets too: they open as the tracks of the disc they cut.
    let extensions = [
        freya_media::files::AUDIO_EXTENSIONS,
        &["cue", "m3u", "m3u8", "pls"],
    ]
    .concat();
    rfd::FileDialog::new()
        .set_title("Open music")
        .add_filter("Audio, CUE sheets and playlists", &extensions)
        .pick_files()
}

fn pick_folder() -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new()
        .set_title("Add folder")
        .pick_folder()
        .map(|p| vec![p])
}

fn pick_playlist() -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new()
        .set_title("Load playlist")
        .add_filter("Playlists", &["m3u8", "m3u", "pls"])
        .pick_file()
        .map(|p| vec![p])
}

fn pick_eqf() -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new()
        .set_title("Load equalizer preset")
        .add_filter("Winamp EQ presets", &["eqf"])
        .pick_file()
        .map(|p| vec![p])
}

fn pick_picture() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Choose a picture")
        .add_filter("Pictures", &["jpg", "jpeg", "png"])
        .pick_file()
}

fn save_eqf_target() -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new()
        .set_title("Save equalizer preset")
        .set_file_name("preset.eqf")
        .add_filter("Winamp EQ presets", &["eqf"])
        .save_file()
        .map(|p| vec![p])
}

fn save_playlist_target() -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new()
        .set_title("Save playlist")
        .set_file_name("playlist.m3u8")
        .add_filter("Playlists", &["m3u8", "m3u", "pls"])
        .save_file()
        .map(|p| vec![p])
}
