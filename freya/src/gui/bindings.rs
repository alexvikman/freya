//! Wiring a skin's callbacks to the app.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use freya_library::eq_presets::{builtin_presets, read_eqf, write_eqf};
use freya_library::{Playlist, SelectMode, playlist_file};
use slint::Model;
use slint_interpreter::Value;

use super::app::App;
use super::equalizer::{self, EqualizerSettings};
use super::library::AlbumSort;
use super::skin_window::SkinWindow;
use super::{library, skin_state, skins, title_bar, visualizer};
use crate::update;

/// What a skin callback does to the app, given the callback's arguments.
type Action = Box<dyn Fn(&mut App, &[Value])>;

/// Wires `ui`'s callbacks to the app. Callbacks hold the app weakly, so a replaced skin window
/// does not keep it, or itself, alive.
pub fn connect(ui: &SkinWindow, app: &Rc<RefCell<App>>) {
    let weak: Weak<RefCell<App>> = Rc::downgrade(app);
    let bind = |global: &str, name: &str, action: Action| {
        let app = weak.clone();
        let result = ui.set_global_callback(global, name, move |args| {
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
    };
    // Callbacks that open a file dialog pick first and only then borrow the app, so the
    // dialog's own event handling never finds the app borrowed.
    let bind_dialog = |global: &str,
                       name: &str,
                       pick: fn() -> Option<Vec<PathBuf>>,
                       action: fn(&mut App, Vec<PathBuf>)| {
        let app = weak.clone();
        let _ = ui.set_global_callback(global, name, move |_| {
            if let Some(paths) = pick()
                && let Some(app) = app.upgrade()
                && let Ok(mut app) = app.try_borrow_mut()
            {
                action(&mut app, paths);
                app.refresh();
            }
            Value::Void
        });
    };

    bind("Player", "play", Box::new(|app, _| app.play()));
    bind(
        "Player",
        "pause",
        Box::new(|app, _| app.player.set_paused(true)),
    );
    bind(
        "Player",
        "toggle-pause",
        Box::new(|app, _| app.toggle_pause()),
    );
    bind("Player", "stop", Box::new(|app, _| app.stop()));
    bind("Player", "next", Box::new(|app, _| app.step(true)));
    bind("Player", "previous", Box::new(|app, _| app.step(false)));
    bind(
        "Player",
        "seek",
        Box::new(|app, args| app.seek(number(args, 0))),
    );
    bind(
        "Player",
        "seek-by",
        Box::new(|app, args| app.seek_by(number(args, 0))),
    );
    bind(
        "Player",
        "set-volume",
        Box::new(|app, args| {
            app.player
                .set_volume((number(args, 0) as f32).clamp(0.0, 1.0))
        }),
    );
    bind_dialog("Player", "open-files", pick_audio, |app, paths| {
        app.replace_and_play(&paths);
    });

    bind(
        "Playlist",
        "activate",
        Box::new(|app, args| app.play_index(index(args, 0))),
    );
    bind(
        "Playlist",
        "select",
        Box::new(|app, args| {
            let mode = if flag(args, 1) {
                SelectMode::Range
            } else if flag(args, 2) {
                SelectMode::Toggle
            } else {
                SelectMode::Single
            };
            app.playlist.click(index(args, 0), mode);
            app.playlist_dirty = true;
        }),
    );
    bind(
        "Playlist",
        "move-cursor",
        Box::new(|app, args| {
            app.playlist
                .move_cursor(number(args, 0) as isize, flag(args, 1));
            app.playlist_dirty = true;
        }),
    );
    bind(
        "Playlist",
        "select-all",
        Box::new(|app, _| app.edit(Playlist::select_all)),
    );
    bind(
        "Playlist",
        "clear-selection",
        Box::new(|app, _| app.edit(Playlist::select_none)),
    );
    bind(
        "Playlist",
        "remove-selected",
        Box::new(|app, _| app.edit(Playlist::remove_selected)),
    );
    bind(
        "Playlist",
        "move-selected",
        Box::new(|app, args| {
            let to = index(args, 0);
            app.edit(|p| p.move_selected(to));
        }),
    );
    bind(
        "Playlist",
        "clear",
        Box::new(|app, _| app.edit(Playlist::clear)),
    );
    bind(
        "Playlist",
        "toggle-shuffle",
        Box::new(|app, _| app.toggle_shuffle()),
    );
    bind(
        "Playlist",
        "toggle-repeat",
        Box::new(|app, _| app.toggle_repeat()),
    );
    bind(
        "Playlist",
        "add-url",
        Box::new(|app, args| {
            if let Some(Value::String(url)) = args.first() {
                app.add_url(url);
            }
        }),
    );
    bind_dialog("Playlist", "add-files", pick_audio, |app, paths| {
        app.add_paths(&paths)
    });
    bind_dialog("Playlist", "add-folder", pick_folder, |app, paths| {
        app.add_paths(&paths)
    });
    bind_dialog("Playlist", "load", pick_playlist, |app, paths| {
        app.edit(Playlist::clear);
        app.add_paths(&paths);
    });
    bind_dialog("Playlist", "save", save_playlist_target, |app, paths| {
        if let Some(path) = paths.first()
            && let Err(e) = playlist_file::save(path, app.playlist.entries())
        {
            log::warn!("could not save playlist: {e}");
        }
    });

    bind(
        "Library",
        "search",
        Box::new(|app, args| {
            if let Some(Value::String(text)) = args.first() {
                app.library.search(text);
            }
        }),
    );
    bind(
        "Library",
        "activate",
        Box::new(|app, args| {
            if let Some((tracks, start)) = app.library.activate(index(args, 0)) {
                app.play_tracks(&tracks, start);
            }
        }),
    );
    bind(
        "Library",
        "play",
        Box::new(|app, args| {
            let tracks = app.library.tracks_of(index(args, 0));
            app.play_tracks(&tracks, 0);
        }),
    );
    bind(
        "Library",
        "enqueue",
        Box::new(|app, args| {
            let tracks = app.library.tracks_of(index(args, 0));
            app.enqueue_tracks(&tracks);
        }),
    );
    bind(
        "Library",
        "play-all",
        Box::new(|app, _| {
            let tracks = app.library.all_tracks();
            app.play_tracks(&tracks, 0);
        }),
    );
    bind(
        "Library",
        "enqueue-all",
        Box::new(|app, _| {
            let tracks = app.library.all_tracks();
            app.enqueue_tracks(&tracks);
        }),
    );
    bind(
        "Library",
        "show",
        Box::new(|app, args| {
            if let Some(Value::String(section)) = args.first() {
                app.library.show(section);
            }
        }),
    );
    bind("Library", "back", Box::new(|app, _| app.library.back()));
    bind(
        "Library",
        "set-album-sort",
        Box::new(|app, args| {
            if let Some(Value::String(name)) = args.first()
                && let Some(sort) = AlbumSort::from_name(name)
            {
                app.settings.album_sort = sort.name().to_owned();
                app.library.set_album_sort(sort);
            }
        }),
    );
    bind(
        "Library",
        "set-album-view",
        Box::new(|app, args| {
            if let Some(Value::String(view)) = args.first()
                && (view == "grid" || view == "list")
            {
                app.settings.album_view = view.to_string();
                app.library.set_album_list(view == "list");
            }
        }),
    );

    bind(
        "Playlists",
        "show",
        Box::new(|app, args| {
            let index = index(args, 0);
            app.shown_playlist = (index < app.playlists.len()).then_some(index);
            app.playlists_error.clear();
        }),
    );
    bind(
        "Playlists",
        "create",
        Box::new(|app, args| app.create_playlist(&text_arg(args, 0), Vec::new())),
    );
    bind(
        "Playlists",
        "save-queue",
        Box::new(|app, args| {
            let entries = app.playlist.entries().to_vec();
            app.create_playlist(&text_arg(args, 0), entries);
        }),
    );
    bind(
        "Playlists",
        "rename",
        Box::new(|app, args| app.rename_playlist(&text_arg(args, 0))),
    );
    bind(
        "Playlists",
        "delete",
        Box::new(|app, _| app.delete_playlist()),
    );
    bind(
        "Playlists",
        "play",
        Box::new(|app, args| {
            let entries = app.shown_playlist_entries();
            app.play_entries(entries, index(args, 0));
        }),
    );
    bind(
        "Playlists",
        "enqueue",
        Box::new(|app, _| {
            let entries = app.shown_playlist_entries();
            app.enqueue_entries(entries);
        }),
    );
    bind(
        "Playlists",
        "remove",
        Box::new(|app, args| {
            if let Some(shown) = app.shown_playlist {
                app.playlists.remove_track(shown, index(args, 0));
            }
        }),
    );
    bind(
        "Playlists",
        "add-queue-selection",
        Box::new(|app, args| {
            let entries = app.queue_selection();
            app.playlists.add(index(args, 0), entries);
        }),
    );
    bind(
        "Playlists",
        "add-library-row",
        Box::new(|app, args| {
            let entries = app
                .library
                .tracks_of(index(args, 1))
                .iter()
                .map(library::entry)
                .collect();
            app.playlists.add(index(args, 0), entries);
        }),
    );

    bind(
        "Radio",
        "play",
        Box::new(|app, args| app.play_station(index(args, 0))),
    );
    bind(
        "Radio",
        "add",
        Box::new(|app, args| {
            if let (Some(Value::String(name)), Some(Value::String(url))) =
                (args.first(), args.get(1))
            {
                app.radio_error = app.stations.add(name, url).err().unwrap_or_default();
            }
        }),
    );
    bind(
        "Radio",
        "remove",
        Box::new(|app, args| app.stations.remove(index(args, 0))),
    );
    // Pure, and asked for while the window draws: it only looks the cover up, and never
    // refreshes the app.
    let app = weak.clone();
    let _ = ui.set_global_callback("Library", "cover", move |args| {
        let image = match (args.first(), app.upgrade()) {
            (Some(Value::String(key)), Some(app)) => app
                .try_borrow_mut()
                .map(|mut app| app.library.cover(key))
                .unwrap_or_default(),
            _ => slint::Image::default(),
        };
        Value::Image(image)
    });
    bind_dialog("Library", "add-folder", pick_music_folder, |app, paths| {
        for path in paths {
            app.add_library_folder(path);
        }
    });
    bind(
        "Library",
        "remove-folder",
        Box::new(|app, args| app.remove_library_folder(index(args, 0))),
    );
    bind(
        "Library",
        "edit-folders",
        Box::new(|app, _| app.edit_folders()),
    );
    bind_dialog(
        "Library",
        "add-draft-folder",
        pick_music_folder,
        |app, paths| {
            for path in paths {
                app.add_draft_folder(path);
            }
        },
    );
    bind(
        "Library",
        "remove-draft-folder",
        Box::new(|app, args| app.remove_draft_folder(index(args, 0))),
    );
    bind(
        "Library",
        "save-folders",
        Box::new(|app, _| app.save_folders()),
    );
    bind(
        "Library",
        "rescan",
        Box::new(|app, _| {
            let folders = app.settings.library_folders.clone();
            app.library.rescan(&folders);
        }),
    );

    bind("App", "show-about", Box::new(|app, _| app.show_about()));
    bind("App", "go-back", Box::new(|app, _| app.go_back()));
    bind("App", "go-forward", Box::new(|app, _| app.go_forward()));
    // The skin changed its view; noted after every callback.
    bind("App", "navigated", Box::new(|_, _| {}));
    bind("LastFm", "connect", Box::new(|app, _| app.lastfm.connect()));
    bind("LastFm", "cancel", Box::new(|app, _| app.lastfm.cancel()));
    bind(
        "LastFm",
        "disconnect",
        Box::new(|app, _| app.lastfm.disconnect()),
    );
    bind(
        "LastFm",
        "set-scrobbling",
        Box::new(|app, args| {
            if let Some(Value::Bool(on)) = args.first() {
                app.settings.lastfm_scrobbling = *on;
            }
        }),
    );
    bind(
        "App",
        "open-link",
        Box::new(|_, args| {
            if let Some(Value::String(name)) = args.first()
                && let Some(url) = crate::update::page(name)
            {
                crate::update::open_page(&url);
            }
        }),
    );

    bind(
        "Updates",
        "check",
        Box::new(|app, _| app.updater.check(true, None)),
    );
    bind(
        "Updates",
        "install",
        Box::new(|app, _| app.updater.install()),
    );
    bind("Updates", "later", Box::new(|app, _| app.updater.later()));
    bind(
        "Updates",
        "skip",
        Box::new(|app, _| {
            if let update::State::Available(release) = app.updater.state() {
                app.settings.skipped_update = Some(release.version().to_owned());
            }
            app.updater.later();
        }),
    );
    bind(
        "Updates",
        "set-check-automatically",
        Box::new(|app, args| {
            if let Some(Value::Bool(on)) = args.first() {
                app.settings.check_for_updates = *on;
            }
        }),
    );
    {
        // Opening the skin picker needs the shared app, to wire up the picker's callbacks.
        let app = weak.clone();
        let _ = ui.set_global_callback("App", "show-skins", move |_| {
            if let Some(app) = app.upgrade() {
                skins::show_picker(&app);
            }
            Value::Void
        });
    }
    bind(
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
    bind(
        "App",
        "toggle-equalizer",
        Box::new(|app, _| app.settings.equalizer_visible = !app.settings.equalizer_visible),
    );
    {
        // Moving the window runs until the mouse is let go: the app is not borrowed meanwhile,
        // so it keeps playing and updating.
        let app = weak.clone();
        let _ = ui.set_global_callback("App", "drag-window", move |_| {
            let ui = app
                .upgrade()
                .and_then(|app| app.try_borrow().ok().map(|app| app.view.ui.clone()));
            if let Some(ui) = ui {
                title_bar::drag(ui.window());
            }
            Value::Void
        });
    }

    bind(
        "Visualizer",
        "cycle-mode",
        Box::new(|app, _| {
            let without_hologram = app.view.visualizer.without_hologram();
            app.settings.visualizer_mode =
                visualizer::next_mode(&app.settings.visualizer_mode, without_hologram).to_owned();
        }),
    );
    bind(
        "Visualizer",
        "set-mode",
        Box::new(|app, args| {
            if let Some(Value::String(mode)) = args.first()
                && visualizer::is_mode(mode)
            {
                app.settings.visualizer_mode = mode.to_string();
            }
        }),
    );
    bind(
        "Visualizer",
        "set-fps",
        Box::new(|app, args| {
            if let Some(fps) = visualizer::fps(number(args, 0)) {
                app.settings.visualizer_fps = fps;
            }
        }),
    );
    // Pure: these compute paths from their arguments only.
    let _ = ui.set_global_callback("Visualizer", "bars-path", |args| {
        skin_state::string(freya_skin::paths::bars_path(
            number(args, 0) as f32,
            number(args, 1) as f32,
            &numbers(args, 2),
        ))
    });
    let _ = ui.set_global_callback("Visualizer", "peaks-path", |args| {
        skin_state::string(freya_skin::paths::peaks_path(
            number(args, 0) as f32,
            number(args, 1) as f32,
            &numbers(args, 2),
            number(args, 3) as f32,
        ))
    });
    let _ = ui.set_global_callback("Visualizer", "wave", |args| {
        skin_state::string(freya_skin::paths::wave_path(
            number(args, 0) as f32,
            number(args, 1) as f32,
            &numbers(args, 2),
        ))
    });

    bind(
        "Equalizer",
        "toggle",
        Box::new(|app, _| app.edit_eq(|eq| eq.enabled = !eq.enabled)),
    );
    bind(
        "Equalizer",
        "set-preamp",
        Box::new(|app, args| {
            let db = number(args, 0) as f32;
            app.edit_eq(|eq| eq.set_preamp(db));
        }),
    );
    bind(
        "Equalizer",
        "set-band",
        Box::new(|app, args| {
            let (band, db) = (index(args, 0), number(args, 1) as f32);
            app.edit_eq(|eq| eq.set_band(band, db));
        }),
    );
    bind(
        "Equalizer",
        "load-preset",
        Box::new(|app, args| {
            if let Some(preset) = builtin_presets().get(index(args, 0)) {
                app.edit_eq(|eq| eq.apply_preset(preset));
            }
        }),
    );
    bind(
        "Equalizer",
        "reset",
        Box::new(|app, _| app.edit_eq(EqualizerSettings::reset)),
    );
    // Pure: computes the curve from its arguments only.
    let _ = ui.set_global_callback("Equalizer", "curve", |args| {
        skin_state::string(equalizer::curve_path(
            number(args, 0) as f32,
            number(args, 1) as f32,
            &numbers(args, 2),
        ))
    });
    bind_dialog("Equalizer", "load-file", pick_eqf, |app, paths| {
        let Some(path) = paths.first() else { return };
        let result = std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| read_eqf(&bytes));
        match result {
            // A library file can hold several presets; like Winamp, load the first.
            Ok(presets) => app.edit_eq(|eq| eq.apply_preset(&presets[0])),
            Err(e) => log::warn!("could not load {}: {e}", path.display()),
        }
    });
    bind_dialog("Equalizer", "save-file", save_eqf_target, |app, paths| {
        let Some(path) = paths.first() else { return };
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let preset = app.settings.equalizer.as_preset(&name);
        if let Err(e) = std::fs::write(path, write_eqf(&[preset])) {
            log::warn!("could not save {}: {e}", path.display());
        }
    });
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
        freya_library::files::AUDIO_EXTENSIONS,
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

/// Starts in the user's music folder, where the music usually is.
fn pick_music_folder() -> Option<Vec<PathBuf>> {
    let mut dialog = rfd::FileDialog::new().set_title("Add music folder");
    if let Some(music) = directories::UserDirs::new().and_then(|d| d.audio_dir().map(Into::into)) {
        dialog = dialog.set_directory::<PathBuf>(music);
    }
    dialog.pick_folder().map(|p| vec![p])
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
