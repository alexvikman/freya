//! Library: the library's sections, pages, searches and edits.

use super::*;

pub(super) fn bind(b: &Binder) {
    b.bind(
        "Library",
        "search",
        Box::new(|app, args| {
            if let Some(Value::String(text)) = args.first() {
                app.library.search(text);
            }
        }),
    );
    b.bind(
        "Library",
        "activate",
        Box::new(|app, args| {
            if let Some((tracks, start)) = app.library.activate(index(args, 0)) {
                app.play_tracks(&tracks, start);
            }
            if let Some(name) = app.library.take_open_playlist() {
                app.show_playlist_named(&name);
            }
        }),
    );
    b.bind(
        "Library",
        "set-albums-sort",
        Box::new(|app, args| {
            let sort = text_arg(args, 0);
            if library::albums_order(&sort).is_some() {
                app.library.set_albums_order(&sort);
                app.settings.albums_sort = sort;
            }
        }),
    );
    b.bind(
        "Library",
        "set-playlists-sort",
        Box::new(|app, args| {
            let sort = text_arg(args, 0);
            if sort == "added" || sort == "name" {
                app.library.set_playlists_order(&sort);
                app.settings.playlists_sort = sort;
            }
        }),
    );
    b.bind(
        "Library",
        "show-album-cover",
        Box::new(|app, _| app.library.show_album_cover()),
    );
    b.bind(
        "Library",
        "toggle-favorite",
        Box::new(|app, args| app.library.toggle_favorite(index(args, 0))),
    );
    b.bind(
        "Library",
        "toggle-album-favorite",
        Box::new(|app, _| app.library.toggle_page_favorite()),
    );
    b.bind(
        "Library",
        "edit-genre",
        Box::new(|app, _| app.library.edit_genre()),
    );
    b.bind(
        "Library",
        "clear-genre-picture",
        Box::new(|app, args| app.library.clear_genre_picture(&text_arg(args, 0))),
    );
    b.bind(
        "Library",
        "save-genre",
        Box::new(|app, args| app.library.save_genre(&text_arg(args, 0))),
    );
    b.bind(
        "Library",
        "cancel-genre",
        Box::new(|app, _| app.library.cancel_genre()),
    );
    b.bind(
        "Library",
        "set-search-filter",
        Box::new(|app, args| app.library.set_search_filter(&text_arg(args, 0))),
    );
    b.bind(
        "Library",
        "suggest",
        Box::new(|app, args| app.library.suggest(&text_arg(args, 0))),
    );
    b.bind(
        "Library",
        "open-suggestion",
        Box::new(
            |app, args| match app.library.open_suggestion(index(args, 0)) {
                Some(library::Opened::Play(tracks, start)) => app.play_tracks(&tracks, start),
                Some(library::Opened::Library) => app.show_view("library"),
                Some(library::Opened::Playlist(name)) => app.show_playlist_named(&name),
                None => {}
            },
        ),
    );
    b.bind(
        "Library",
        "play",
        Box::new(|app, args| {
            let tracks = app.library.tracks_of(index(args, 0));
            app.play_tracks(&tracks, 0);
        }),
    );
    b.bind(
        "Library",
        "enqueue",
        Box::new(|app, args| {
            let tracks = app.library.tracks_of(index(args, 0));
            app.enqueue_tracks(&tracks);
        }),
    );
    b.bind(
        "Library",
        "play-all",
        Box::new(|app, _| {
            let tracks = app.library.all_tracks();
            app.play_tracks(&tracks, 0);
        }),
    );
    b.bind(
        "Library",
        "shuffle-all",
        Box::new(|app, _| {
            let tracks = app.library.all_tracks();
            app.shuffle_tracks(&tracks);
        }),
    );
    b.bind(
        "Library",
        "open-album-artist",
        Box::new(|app, _| app.library.open_album_artist()),
    );
    for (callback, album) in [("open-playing-artist", false), ("open-playing-album", true)] {
        b.bind(
            "Library",
            callback,
            Box::new(move |app, _| {
                if app.library.open_playing(album) {
                    app.show_view("library");
                }
            }),
        );
    }
    b.bind(
        "Library",
        "enqueue-all",
        Box::new(|app, _| {
            let tracks = app.library.all_tracks();
            app.enqueue_tracks(&tracks);
        }),
    );
    b.bind(
        "Library",
        "show",
        Box::new(|app, args| {
            if let Some(Value::String(section)) = args.first() {
                app.library.show(section);
            }
        }),
    );
    b.bind("Library", "back", Box::new(|app, _| app.library.back()));
    b.bind(
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
    b.bind(
        "Library",
        "set-artist-sort",
        Box::new(|app, args| {
            if let Some(Value::String(name)) = args.first()
                && let Some(order) = library::artist_order(name)
            {
                app.settings.artist_sort = name.to_string();
                app.library.set_artist_order(order);
            }
        }),
    );
    b.bind(
        "Library",
        "set-artist-view",
        Box::new(|app, args| {
            if let Some(Value::String(view)) = args.first()
                && (view == "grid" || view == "list")
            {
                app.settings.artist_view = view.to_string();
                app.library.set_artist_list(view == "list");
            }
        }),
    );
    b.bind(
        "Library",
        "play-popular",
        Box::new(|app, args| {
            let tracks = app.library.popular_tracks();
            app.play_tracks(&tracks, index(args, 0));
        }),
    );
    b.bind(
        "Library",
        "more-popular",
        Box::new(|app, _| app.library.toggle_more_popular()),
    );
    b.bind(
        "Library",
        "set-artist-bios-online",
        Box::new(|app, args| {
            if let Some(Value::Bool(online)) = args.first() {
                app.library.set_online_bios(*online);
            }
        }),
    );
    b.bind(
        "Library",
        "set-artist-wikipedia-online",
        Box::new(|app, args| {
            if let Some(Value::Bool(online)) = args.first() {
                app.library.set_online_wikipedia(*online);
            }
        }),
    );
    b.bind(
        "Library",
        "set-radio-directory-online",
        Box::new(|app, args| {
            if let Some(Value::Bool(online)) = args.first() {
                app.library.set_radio_directory_online(*online);
            }
        }),
    );
    b.bind(
        "Library",
        "edit-artist",
        Box::new(|app, _| app.library.edit_artist()),
    );
    b.bind(
        "Library",
        "cancel-artist",
        Box::new(|app, _| app.library.cancel_artist()),
    );
    b.bind(
        "Library",
        "choose-picture",
        Box::new(|app, args| {
            if let (Some(Value::String(kind)), Some(Value::Number(index))) =
                (args.first(), args.get(1))
            {
                app.library.choose_picture(kind, *index as usize);
            }
        }),
    );
    b.bind(
        "Library",
        "match-artist",
        Box::new(|app, _| app.library.start_artist_match()),
    );
    b.bind(
        "Library",
        "save-artist-match",
        Box::new(|app, args| {
            if let Some(Value::String(mbid)) = args.first() {
                app.library.save_artist_match(mbid);
            }
        }),
    );
    b.bind(
        "Library",
        "cancel-artist-match",
        Box::new(|app, _| app.library.cancel_artist_match()),
    );
    b.bind(
        "Library",
        "match-album",
        Box::new(|app, _| app.library.start_album_match()),
    );
    b.bind(
        "Library",
        "find-album-matches",
        Box::new(|app, args| {
            if let (Some(Value::String(title)), Some(Value::String(artist))) =
                (args.first(), args.get(1))
            {
                app.library.find_album_matches(title, artist);
            }
        }),
    );
    b.bind(
        "Library",
        "save-album-match",
        Box::new(|app, args| {
            if let Some(Value::String(group)) = args.first() {
                app.library.save_album_match(group);
            }
        }),
    );
    b.bind(
        "Library",
        "cancel-album-match",
        Box::new(|app, _| app.library.cancel_album_match()),
    );
    b.bind(
        "Library",
        "find-matches",
        Box::new(|app, args| {
            if let Some(Value::String(query)) = args.first() {
                app.library.find_matches(query);
            }
        }),
    );
    b.bind(
        "Library",
        "picture-from-url",
        Box::new(|app, args| {
            if let (Some(Value::String(kind)), Some(Value::String(url))) =
                (args.first(), args.get(1))
            {
                app.library.picture_from_url(kind, url);
            }
        }),
    );
    // The file is picked before the app is borrowed: the dialog runs the event loop.
    {
        let app = b.weak.clone();
        let genre_app = app.clone();
        let _ =
            b.ui.set_global_callback("Library", "pick-genre-picture", move |args| {
                let Some(Value::String(kind)) = args.first() else {
                    return Value::Void;
                };
                if let Some(file) = pick_picture()
                    && let Some(app) = genre_app.upgrade()
                    && let Ok(mut app) = app.try_borrow_mut()
                {
                    app.library.pick_genre_picture(kind, &file);
                    app.refresh();
                }
                Value::Void
            });
        let _ =
            b.ui.set_global_callback("Playlists", "pick-picture", move |args| {
                let Some(Value::String(kind)) = args.first() else {
                    return Value::Void;
                };
                if let Some(file) = pick_picture()
                    && let Some(app) = app.upgrade()
                    && let Ok(mut app) = app.try_borrow_mut()
                {
                    app.library.pick_playlist_picture(kind, &file);
                    app.refresh();
                }
                Value::Void
            });
    }
    {
        let app = b.weak.clone();
        let _ =
            b.ui.set_global_callback("Library", "pick-picture-file", move |args| {
                let Some(Value::String(kind)) = args.first() else {
                    return Value::Void;
                };
                if let Some(file) = pick_picture()
                    && let Some(app) = app.upgrade()
                    && let Ok(mut app) = app.try_borrow_mut()
                {
                    app.library.add_picture_file(kind, &file);
                    app.refresh();
                }
                Value::Void
            });
    }
    b.bind(
        "Library",
        "save-artist",
        Box::new(|app, args| {
            let text = |i: usize| match args.get(i) {
                Some(Value::String(s)) => Some(s.to_string()),
                _ => None,
            };
            let edit = freya_protocol::ArtistEdit {
                name: text(0),
                sort_name: text(1),
                mbid: text(2),
                biography: text(3),
                // Of the match: the library knows it.
                note: None,
                header: text(4)
                    .as_deref()
                    .and_then(freya_protocol::Header::from_name),
            };
            app.library.save_artist(&edit);
        }),
    );
    b.bind(
        "Library",
        "set-fanart-key",
        Box::new(|app, args| {
            if let Some(Value::String(key)) = args.first() {
                let key = Some(key.trim().to_owned()).filter(|k| !k.is_empty());
                app.library.set_fanart_key(key);
            }
        }),
    );
    b.bind(
        "Library",
        "set-watch-folders",
        Box::new(|app, args| {
            if let Some(Value::Bool(watch)) = args.first() {
                app.library.set_watch_folders(*watch);
            }
        }),
    );
    b.bind(
        "Library",
        "set-scan-every-hours",
        Box::new(|app, args| {
            let hours = index(args, 0) as u32;
            app.library.set_scan_every_hours(hours);
        }),
    );
    b.bind(
        "Library",
        "set-scan-at-start",
        Box::new(|app, args| {
            if let Some(Value::Bool(scan)) = args.first() {
                app.library.set_scan_at_start(*scan);
            }
        }),
    );
    b.bind(
        "Library",
        "set-artist-pictures-online",
        Box::new(|app, args| {
            if let Some(Value::Bool(online)) = args.first() {
                app.library.set_online_pictures(*online);
            }
        }),
    );
    b.bind(
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
    // Pure, and asked for while the window draws: it only looks the cover up, and never
    // refreshes the app.
    let app = b.weak.clone();
    let _ = b.ui.set_global_callback("Library", "cover", move |args| {
        let image = match (args.first(), app.upgrade()) {
            (Some(Value::String(key)), Some(app)) => app
                .try_borrow_mut()
                .map(|mut app| app.library.cover(key))
                .unwrap_or_default(),
            _ => slint::Image::default(),
        };
        Value::Image(image)
    });
    b.bind(
        "Library",
        "edit-folders",
        Box::new(|app, _| app.edit_folders()),
    );
    b.bind(
        "Library",
        "remove-draft-folder",
        Box::new(|app, args| app.remove_draft_folder(index(args, 0))),
    );
    b.bind(
        "Library",
        "save-folders",
        Box::new(|app, _| app.save_folders()),
    );
    b.bind(
        "Library",
        "rescan",
        Box::new(|app, _| {
            let folders = app.library.folders();
            app.library.rescan(&folders);
        }),
    );
    b.bind(
        "Library",
        "rescan-files",
        Box::new(|app, _| {
            let folders = app.library.folders();
            app.library.rescan_files(&folders);
        }),
    );
    b.bind(
        "Library",
        "scan-one-folder",
        Box::new(|app, args| {
            let folders = app.library.folders();
            app.library.scan_folder(&folders, index(args, 0));
        }),
    );
    b.bind(
        "Library",
        "stop-scan",
        Box::new(|app, _| app.library.stop_scan()),
    );
}
