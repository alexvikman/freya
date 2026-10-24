//! Playlists: the server's playlists.

use super::*;

pub(super) fn bind(b: &Binder) {
    b.bind(
        "Playlists",
        "toggle-favorite",
        Box::new(|app, _| {
            let name = app
                .shown_playlist
                .and_then(|i| app.playlists.get(i))
                .map(|p| p.name.clone());
            if let Some(name) = name {
                app.library.set_playlist_favorite(&name);
            }
        }),
    );
    b.bind(
        "Playlists",
        "show",
        Box::new(|app, args| {
            let index = index(args, 0);
            app.shown_playlist = (index < app.playlists.len()).then_some(index);
            app.playlists_error.clear();
        }),
    );
    b.bind(
        "Playlists",
        "create",
        Box::new(|app, args| app.create_playlist(&text_arg(args, 0), Vec::new())),
    );
    b.bind(
        "Playlists",
        "save-queue",
        Box::new(|app, args| {
            let entries = app.playlist.entries().to_vec();
            app.create_playlist(&text_arg(args, 0), entries);
        }),
    );
    b.bind(
        "Playlists",
        "rename",
        Box::new(|app, args| app.rename_playlist(&text_arg(args, 0))),
    );
    b.bind(
        "Playlists",
        "delete",
        Box::new(|app, _| app.delete_playlist()),
    );
    b.bind(
        "Playlists",
        "play",
        Box::new(|app, args| {
            let entries = app.shown_playlist_entries();
            app.play_entries(entries, index(args, 0));
        }),
    );
    b.bind(
        "Playlists",
        "shuffle",
        Box::new(|app, _| app.shuffle_playlist()),
    );
    b.bind(
        "Playlists",
        "set-sort",
        Box::new(|app, args| {
            app.library.set_playlist_sort(&text_arg(args, 0));
            app.resync_playlist();
        }),
    );
    b.bind(
        "Playlists",
        "edit",
        Box::new(|app, _| app.library.edit_playlist()),
    );
    b.bind(
        "Playlists",
        "clear-picture",
        Box::new(|app, args| app.library.clear_playlist_picture(&text_arg(args, 0))),
    );
    b.bind(
        "Playlists",
        "save-edit",
        Box::new(|app, args| app.save_playlist_edit(&text_arg(args, 0), &text_arg(args, 1))),
    );
    b.bind(
        "Playlists",
        "cancel-edit",
        Box::new(|app, _| app.library.cancel_playlist_edit()),
    );
    b.bind(
        "Playlists",
        "enqueue",
        Box::new(|app, _| {
            let entries = app.shown_playlist_entries();
            app.enqueue_entries(entries);
        }),
    );
    b.bind(
        "Playlists",
        "remove",
        Box::new(|app, args| {
            if let Some(shown) = app.shown_playlist {
                let entry = app.library.playlist_entry(index(args, 0));
                app.playlists.remove_track(shown, entry);
            }
        }),
    );
    b.bind(
        "Playlists",
        "add-queue-selection",
        Box::new(|app, args| {
            let entries = app.queue_selection();
            app.add_to_playlist(index(args, 0), entries);
        }),
    );
    b.bind(
        "Playlists",
        "add-library-row",
        Box::new(|app, args| {
            let entries = app
                .library
                .tracks_of(index(args, 1))
                .iter()
                .map(library::entry)
                .collect();
            app.add_to_playlist(index(args, 0), entries);
        }),
    );
}
