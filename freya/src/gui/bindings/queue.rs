//! Playlist: Up Next, its rows and files.

use super::*;

pub(super) fn bind(b: &Binder) {
    b.bind(
        "Playlist",
        "activate",
        Box::new(|app, args| app.play_index(index(args, 0))),
    );
    b.bind(
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
    b.bind(
        "Playlist",
        "move-cursor",
        Box::new(|app, args| {
            app.playlist
                .move_cursor(number(args, 0) as isize, flag(args, 1));
            app.playlist_dirty = true;
        }),
    );
    b.bind(
        "Playlist",
        "select-all",
        Box::new(|app, _| app.edit(Playlist::select_all)),
    );
    b.bind(
        "Playlist",
        "clear-selection",
        Box::new(|app, _| app.edit(Playlist::select_none)),
    );
    b.bind(
        "Playlist",
        "remove-selected",
        Box::new(|app, _| app.edit(Playlist::remove_selected)),
    );
    b.bind(
        "Playlist",
        "move-selected",
        Box::new(|app, args| {
            let to = index(args, 0);
            app.edit(|p| p.move_selected(to));
        }),
    );
    b.bind(
        "Playlist",
        "clear",
        Box::new(|app, _| app.edit(Playlist::clear)),
    );
    b.bind(
        "Playlist",
        "toggle-shuffle",
        Box::new(|app, _| app.toggle_shuffle()),
    );
    b.bind(
        "Playlist",
        "toggle-repeat",
        Box::new(|app, _| app.toggle_repeat()),
    );
    b.bind(
        "Playlist",
        "add-url",
        Box::new(|app, args| {
            if let Some(Value::String(url)) = args.first() {
                app.add_url(url);
            }
        }),
    );
    b.dialog("Playlist", "add-files", pick_audio, |app, paths| {
        app.add_paths(&paths)
    });
    b.dialog("Playlist", "add-folder", pick_folder, |app, paths| {
        app.add_paths(&paths)
    });
    b.dialog("Playlist", "load", pick_playlist, |app, paths| {
        app.edit(Playlist::clear);
        app.add_paths(&paths);
    });
    b.dialog("Playlist", "save", save_playlist_target, |app, paths| {
        if let Some(path) = paths.first()
            && let Err(e) = playlist_file::save(path, app.playlist.entries())
        {
            log::warn!("could not save playlist: {e}");
        }
    });
}
