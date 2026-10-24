//! The library panel against a library and its server in this process.

use freya_library::Library;
use freya_media::pictures::Pictures;

use super::*;

/// `db` as a library to show, with its pictures in `pictures`.
pub(super) fn local(db: Library, pictures: Option<PathBuf>) -> Arc<dyn LibraryApi> {
    Arc::new(freya_server::LocalLibrary::new(
        db,
        pictures.map(Pictures::new),
    ))
}

#[test]
fn shows_what_a_scan_kept_of_an_artist() {
    let mut panel = LibraryPanel::new(None, None);
    let db = Library::open_in_memory().unwrap();
    db.set_artist_picture(
        "Shining",
        PictureKind::Portrait,
        "deezer",
        Some("square"),
        SystemTime::now(),
    )
    .unwrap();
    db.set_artist_picture(
        "Shining",
        PictureKind::Backdrop,
        "fanart.tv",
        None,
        SystemTime::now(),
    )
    .unwrap();
    db.set_artist_edit(
        "Shining",
        &ArtistEdit {
            mbid: Some("swedish".into()),
            ..ArtistEdit::default()
        },
    )
    .unwrap();
    let article = freya_protocol::Article {
        title: "Shining (Swedish band)".into(),
        summary: "A Swedish band.".into(),
        biography: "A Swedish band.".into(),
        url: String::new(),
    };
    db.set_artist_article("Shining", "swedish", Some(&article), SystemTime::now())
        .unwrap();
    panel.open(local(db, None));
    panel.reload();
    let source = panel.artist_source(Some("Shining"));
    assert_eq!(source.picture.as_deref(), Some("square"));
    assert!(panel.backdrops.is_empty(), "fanart.tv had none");
    let (summary, _, _, from) = panel.biography_of("Shining");
    assert_eq!(
        (summary.as_str(), from.as_str()),
        ("A Swedish band.", "Wikipedia")
    );

    // Matched to another artist of the name: not theirs.
    let db = panel.db.as_ref().unwrap();
    db.set_artist_edit(
        "Shining",
        &ArtistEdit {
            mbid: Some("norwegian".into()),
            ..ArtistEdit::default()
        },
    )
    .unwrap();
    panel.reload();
    assert_eq!(panel.biography_of("Shining").3, "");
}

#[test]
fn opens_the_album_of_the_track_playing_at_its_row() {
    let dir = std::env::temp_dir().join(format!("freya-open-playing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
    for name in ["sine.flac", "sine.ogg"] {
        std::fs::copy(fixtures.join(name), dir.join(name)).unwrap();
    }
    let mut db = Library::open_in_memory().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
    let mut panel = LibraryPanel::new(None, None);
    panel.open(local(db, None));
    panel.reload();

    // Not in the library: nothing opens.
    panel.set_playing(Some((Path::new("/elsewhere.mp3"), Duration::ZERO)));
    assert!(!panel.open_playing(true));

    let playing = dir.join("sine.ogg");
    panel.set_playing(Some((&playing, Duration::ZERO)));
    assert!(panel.open_playing(true));
    assert!(matches!(panel.level(), Level::AlbumTracks { .. }));
    assert_eq!(panel.opened.len(), 3, "back goes to the artist");
    assert!(panel.playing_row >= 0);
    assert_eq!((panel.reveal_row, panel.reveals), (panel.playing_row, 1));
    let Item::Track(track) = &panel.items[panel.playing_row as usize] else {
        panic!("a track row");
    };
    assert_eq!(track.path, playing);

    assert!(panel.open_playing(false));
    assert!(matches!(panel.level(), Level::ArtistAlbums { .. }));
    assert_eq!((panel.reveal_row, panel.reveals), (-1, 2));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_server_is_asked_off_the_windows_thread() {
    let dir = std::env::temp_dir().join(format!("freya-asked-off-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
    for name in ["sine.flac", "sine.ogg"] {
        std::fs::copy(fixtures.join(name), dir.join(name)).unwrap();
    }
    let mut db = Library::open(&dir.join("library.db")).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
    let served = freya_server::Served {
        library: local(db, None),
        server: None,
        accounts: None,
        certificate: None,
        callback_port: freya_protocol::CALLBACK_PORT,
        updates: None,
    };
    let server =
        freya_server::http::Server::start("127.0.0.1:0", freya_server::handler(served)).unwrap();
    let remote = freya_client::connect(&format!("http://{}", server.address()), None, None);
    let mut panel = LibraryPanel::new(None, None);
    panel.remote = Some(Arc::clone(&remote));
    panel.open(remote);
    // Asked on a thread: the rows come once the answers do.
    assert!(panel.asking.is_some());
    let answered = |panel: &mut LibraryPanel| {
        let waited = Instant::now();
        while panel.asking.is_some()
            || panel.asking_pictures.is_some()
            || panel.asking_playing.is_some()
        {
            assert!(waited.elapsed() < Duration::from_secs(10), "no answer");
            std::thread::sleep(Duration::from_millis(5));
            panel.poll();
        }
    };
    answered(&mut panel);
    panel.show("songs");
    assert!(
        panel.asking.is_some(),
        "the rows shown stay until the new ones come"
    );
    answered(&mut panel);
    let tracks = panel.db.as_ref().unwrap().all_tracks().unwrap();
    assert!(!tracks.is_empty());
    assert_eq!(panel.items.len(), tracks.len());

    // A favourite made is kept on the errands' thread, then the rows read again.
    panel.show("albums");
    answered(&mut panel);
    let Some(Item::Album(album)) = panel.items.first().cloned() else {
        panic!("an album row");
    };
    panel.toggle_favorite(0);
    assert!(panel.asking.is_some(), "the window does not wait");
    answered(&mut panel);
    assert!(panel.is_favorite(album.artist.as_deref(), album.name.as_deref()));

    // What is done once the rows are read waits for them.
    let playing = dir.join("sine.ogg");
    panel.set_playing(Some((&playing, Duration::ZERO)));
    answered(&mut panel);
    assert!(panel.open_playing(true));
    answered(&mut panel);
    assert!(panel.playing_row >= 0);
    assert_eq!(panel.reveal_row, panel.playing_row);
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shows_an_artist_by_what_the_user_changed() {
    let dir = std::env::temp_dir().join(format!("freya-edited-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
    std::fs::copy(fixtures.join("sine.mp3"), dir.join("sine.mp3")).unwrap();
    let mut db = Library::open_in_memory().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
    let artist = db.artists().unwrap()[0].name.clone();
    assert_eq!(artist.as_deref(), Some("Freya"));
    let edit = ArtistEdit {
        name: Some("Freya Herself".into()),
        mbid: Some("0000-mbid".into()),
        biography: Some("First part.\n\nSecond part.".into()),
        ..ArtistEdit::default()
    };
    db.set_artist_edit(artist.as_deref().unwrap(), &edit)
        .unwrap();

    let mut panel = LibraryPanel::new(None, None);
    panel.open(local(db, None));
    panel.settings.artist_bios_online = false;
    panel.reload();
    let Value::Model(rows) = panel.rows.model_value() else {
        panic!("a model");
    };
    let Some(Value::Struct(row)) = slint::Model::row_data(&rows, 0) else {
        panic!("an artist row");
    };
    assert_eq!(row.get_field("title"), Some(&string("Freya Herself")));

    panel.opened.push(Level::ArtistAlbums { artist });
    panel.reload();
    assert_eq!(panel.title, "Freya Herself");
    let page = panel.artist_page.as_ref().unwrap();
    assert_eq!(page.shown_name, "Freya Herself");
    assert!(page.own_biography);
    assert_eq!(page.summary, "First part.");
    assert_eq!(
        panel.known_mbid(page.name.as_deref()).as_deref(),
        Some("0000-mbid")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn changes_an_artist_from_the_form() {
    let dir = std::env::temp_dir().join(format!("freya-form-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
    std::fs::copy(fixtures.join("sine.mp3"), dir.join("sine.mp3")).unwrap();
    let mut db = Library::open_in_memory().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
    db.set_artist_picture(
        "Freya",
        PictureKind::Backdrop,
        "fanart.tv",
        None,
        SystemTime::now(),
    )
    .unwrap();
    db.set_searched_artist_id("Freya", Some("found-id"), SystemTime::now())
        .unwrap();
    let mut panel = LibraryPanel::new(None, None);
    panel.open(local(db, None));
    panel.settings.artist_bios_online = false;
    panel.opened = vec![
        Level::Artists,
        Level::ArtistAlbums {
            artist: Some("Freya".into()),
        },
    ];
    panel.reload();

    // Nothing to save without the form open.
    panel.save_artist(&ArtistEdit::default());
    panel.edit_artist();
    let Value::Struct(draft) = panel.draft_value() else {
        panic!("a struct");
    };
    assert_eq!(draft.get_field("artist"), Some(&string("Freya")));
    assert_eq!(draft.get_field("name"), Some(&string("")));
    assert_eq!(draft.get_field("found-mbid"), Some(&string("found-id")));

    panel.save_artist(&ArtistEdit {
        name: Some("Freya Herself".into()),
        mbid: Some("own-id".into()),
        ..ArtistEdit::default()
    });
    assert_eq!(panel.title, "Freya Herself");
    assert!(panel.draft.is_none(), "the form is done");
    // Another id: fanart.tv is asked again.
    let db = panel.db.as_ref().unwrap();
    assert_eq!(
        db.artist_picture("Freya", PictureKind::Backdrop).unwrap(),
        None
    );
    let page = panel.artist_page.as_ref().unwrap();
    assert_eq!(
        panel.known_mbid(page.name.as_deref()).as_deref(),
        Some("own-id")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn chooses_and_keeps_an_artists_pictures() {
    let dir = std::env::temp_dir().join(format!("freya-choose-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let album = dir.join("music").join("Freya").join("Sines");
    std::fs::create_dir_all(&album).unwrap();
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
    std::fs::copy(fixtures.join("sine.mp3"), album.join("sine.mp3")).unwrap();
    image::RgbImage::from_pixel(1920, 1080, image::Rgb([0, 0, 200]))
        .save(dir.join("music").join("Freya").join("fanart.jpg"))
        .unwrap();
    let mut db = Library::open_in_memory().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let options = freya_library::library::ScanOptions {
        pictures: Some(Pictures::new(dir.join("pictures"))),
        ..freya_library::library::ScanOptions::default()
    };
    freya_library::library::scan_with(&mut db, &[dir.join("music")], &options, &cancel, |_| {})
        .unwrap();
    let folder = db
        .artist_picture("Freya", PictureKind::Backdrop)
        .unwrap()
        .and_then(|row| row.picture)
        .expect("the folder's, taken in by the scan");
    let mut panel = LibraryPanel::new(None, None);
    panel.open(local(db, Some(dir.join("pictures"))));
    panel.settings.artist_pictures_online = false;
    panel.settings.artist_bios_online = false;
    panel.opened = vec![
        Level::Artists,
        Level::ArtistAlbums {
            artist: Some("Freya".into()),
        },
    ];
    panel.reload();

    let finish = |panel: &mut LibraryPanel| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while panel.finder.searching() {
            assert!(Instant::now() < deadline, "the pictures were not found");
            panel.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    panel.edit_artist();
    finish(&mut panel);
    // The folder's wide picture, as the scan took it in; none square.
    assert_eq!(panel.picked[&PictureKind::Backdrop], Pick::Kept(folder));
    assert_eq!(panel.picked[&PictureKind::Portrait], Pick::Nothing);
    let chosen = |rows: &ModelRows| -> Vec<bool> {
        let Value::Model(model) = rows.model_value() else {
            panic!("a model");
        };
        (0..slint::Model::row_count(&model))
            .map(|i| match slint::Model::row_data(&model, i) {
                Some(Value::Struct(row)) => row.get_field("chosen") == Some(&Value::Bool(true)),
                _ => false,
            })
            .collect()
    };
    assert_eq!(
        chosen(&panel.backdrop_rows),
        [false, true],
        "None, then the folder's"
    );

    // None wide, and a picture of the user's own square.
    panel.choose_picture("backdrop", 0);
    let own = dir.join("own.png");
    image::RgbImage::from_pixel(500, 500, image::Rgb([0, 200, 0]))
        .save(&own)
        .unwrap();
    panel.add_picture_file("portrait", &own);
    assert!(matches!(
        panel.picked[&PictureKind::Portrait],
        Pick::Kept(_)
    ));
    assert_eq!(chosen(&panel.portrait_rows), [false, true]);
    panel.save_artist(&ArtistEdit::default());

    let db = panel.db.as_ref().unwrap();
    let wide = db
        .artist_picture("Freya", PictureKind::Backdrop)
        .unwrap()
        .unwrap();
    assert!(wide.chosen() && wide.picture.is_none());
    let square = db
        .artist_picture("Freya", PictureKind::Portrait)
        .unwrap()
        .unwrap();
    assert!(square.chosen() && square.picture.is_some());
    assert!(panel.choices.is_empty(), "the dialog is done");
    // Shown at once, not only after Freya starts again.
    assert_eq!(panel.artist_source(Some("Freya")).picture, square.picture);
    assert!(!panel.backdrops.contains_key("Freya"));

    // Chosen: it stays.
    panel.edit_artist();
    finish(&mut panel);
    assert_eq!(panel.picked[&PictureKind::Backdrop], Pick::Nothing);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn files_an_album_under_the_artist_it_is_matched_to() {
    let dir = std::env::temp_dir().join(format!("freya-album-match-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
    std::fs::copy(fixtures.join("sine.mp3"), dir.join("sine.mp3")).unwrap();
    let mut db = Library::open_in_memory().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
    let album = db.all_albums().unwrap()[0].name.clone();
    let mut panel = LibraryPanel::new(None, None);
    panel.open(local(db, None));
    panel.settings.artist_bios_online = false;
    let candidate = |mbid: &str| freya_protocol::AlbumCandidate {
        release_group: format!("rg-{mbid}"),
        title: "Sines".into(),
        artist: freya_protocol::ArtistMatch {
            mbid: mbid.into(),
            name: "Freya".into(),
            disambiguation: format!("the {mbid} one"),
            ..Default::default()
        },
        ..Default::default()
    };
    let matching = |panel: &mut LibraryPanel, artist: &str, mbid: &str| {
        panel.show_album(Some(artist.to_owned()), album.clone());
        panel.album_draft = Some((Some(artist.to_owned()), album.clone()));
        panel.album_candidates = vec![candidate(mbid)];
        panel.choose_album_match(0);
    };

    // Freya had no id for the artist: the first match is theirs, and nothing splits.
    matching(&mut panel, "Freya", "first");
    assert_eq!(panel.edits["Freya"].mbid.as_deref(), Some("first"));
    assert_eq!(panel.edits["Freya"].note.as_deref(), Some("the first one"));
    assert!(matches!(panel.level(), Level::AlbumTracks { artist: Some(a), .. } if a == "Freya"));
    assert!(panel.album_draft.is_none(), "matching is done");

    // Matched to another artist of the name: filed under them, shown as "Freya".
    matching(&mut panel, "Freya", "other");
    let split = split_artist("Freya", "other");
    assert!(matches!(panel.level(), Level::AlbumTracks { artist: Some(a), .. } if *a == split));
    assert_eq!(panel.edits[&split].name.as_deref(), Some("Freya"));
    assert_eq!(panel.edits[&split].note.as_deref(), Some("the other one"));
    assert_eq!(panel.title, album_name(album.as_deref()));
    assert_eq!(panel.album_page.as_ref().unwrap().artist, "Freya");

    // Back as the tags say.
    panel.album_draft = Some((Some(split.clone()), album.clone()));
    panel.unmatch_album();
    assert!(matches!(panel.level(), Level::AlbumTracks { artist: Some(a), .. } if a == "Freya"));
    let db = panel.db.as_ref().unwrap();
    assert_eq!(db.albums(Some(&split)).unwrap().len(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn matches_an_artist_from_the_form() {
    let dir = std::env::temp_dir().join(format!("freya-artist-match-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
    std::fs::copy(fixtures.join("sine.mp3"), dir.join("sine.mp3")).unwrap();
    let mut db = Library::open_in_memory().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
    db.set_searched_artist_id("Freya", Some("se"), SystemTime::now())
        .unwrap();
    let mut panel = LibraryPanel::new(None, None);
    panel.open(local(db, None));
    panel.settings.artist_bios_online = false;
    panel.opened = vec![
        Level::Artists,
        Level::ArtistAlbums {
            artist: Some("Freya".into()),
        },
    ];
    panel.reload();
    panel.edit_artist();
    panel.matches = vec![freya_protocol::ArtistMatch {
        mbid: "no".into(),
        name: "Freya".into(),
        disambiguation: "Norwegian group".into(),
        ..Default::default()
    }];
    panel.save_artist(&ArtistEdit {
        mbid: Some("no".into()),
        ..ArtistEdit::default()
    });
    assert_eq!(panel.edits["Freya"].mbid.as_deref(), Some("no"));
    assert_eq!(
        panel.edits["Freya"].note.as_deref(),
        Some("Norwegian group")
    );
    let Value::Struct(page) = panel.artist_page_value() else {
        panic!("a struct");
    };
    assert_eq!(page.get_field("note"), Some(&string("Norwegian group")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tells_what_sets_an_artist_apart() {
    let artist = |kind: &str, area: &str, begin: &str, end: &str| freya_protocol::ArtistMatch {
        kind: kind.into(),
        area: area.into(),
        begin: begin.into(),
        end: end.into(),
        ..Default::default()
    };
    assert_eq!(
        match_facts(&artist("Group", "Norway", "1999", "")),
        "Group · Norway · 1999–"
    );
    assert_eq!(
        match_facts(&artist("Person", "", "1943-02-25", "2001-11-29")),
        "Person · 1943–2001"
    );
    assert_eq!(match_facts(&artist("", "Sweden", "", "")), "Sweden");
}

#[test]
fn names_a_folder_by_its_last_two() {
    assert_eq!(
        folder_name(Path::new("/Volumes/Music/Peter Gabriel/So")),
        "Peter Gabriel / So"
    );
    assert_eq!(folder_name(Path::new("/Music")), "Music");
    assert_eq!(folder_name(Path::new("/")), "");
}

#[test]
fn formats_counts() {
    assert_eq!(count(1, "track"), "1 track");
    assert_eq!(count(0, "album"), "0 albums");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(1_000), "1,000");
    assert_eq!(thousands(1_234_567), "1,234,567");
}

#[test]
fn an_artists_page_shows_the_picture_the_user_wants_at_its_top() {
    let backdrops = HashMap::from([("Dido".to_owned(), "wide".to_owned())]);
    let mut edits = HashMap::new();
    assert_eq!(
        shown_backdrop(&backdrops, &edits, "Dido").map(String::as_str),
        Some("wide")
    );
    for (header, shown) in [(Header::Wide, Some("wide")), (Header::Picture, None)] {
        edits.insert(
            "Dido".to_owned(),
            ArtistEdit {
                header: Some(header),
                ..ArtistEdit::default()
            },
        );
        assert_eq!(
            shown_backdrop(&backdrops, &edits, "Dido").map(String::as_str),
            shown
        );
    }
    assert_eq!(shown_backdrop(&backdrops, &edits, "Air"), None);
}

#[test]
fn an_album_is_made_a_favourite_and_ordered_as_the_user_picks() {
    let dir = std::env::temp_dir().join(format!("freya-favourite-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
    std::fs::copy(fixtures.join("sine.mp3"), dir.join("sine.mp3")).unwrap();
    let mut db = Library::open_in_memory().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let mut panel = LibraryPanel::new(None, None);
    panel.open(local(db, None));
    panel.opened = vec![Level::Albums];
    panel.reload();
    let favorite = |panel: &LibraryPanel| {
        let Value::Model(rows) = panel.rows.model_value() else {
            panic!("a model");
        };
        match slint::Model::row_data(&rows, 0) {
            Some(Value::Struct(row)) => row.get_field("favorite") == Some(&Value::Bool(true)),
            _ => false,
        }
    };
    assert!(!favorite(&panel));
    panel.toggle_favorite(0);
    assert!(favorite(&panel), "shown at once");
    assert_eq!(panel.favorites.len(), 1);
    panel.toggle_favorite(0);
    assert!(!favorite(&panel));

    panel.set_albums_order("played");
    assert_eq!(panel.albums_order, AlbumOrder::Played);
    panel.set_albums_order("nonsense");
    assert_eq!(panel.albums_order, AlbumOrder::Played);
}

#[test]
fn jumps_to_the_first_row_of_each_letter() {
    let names = ["2Pac", "ABBA", "Air", "Cream", "Ólafur Arnalds", "Åsa"];
    let jumps = letter_jumps(names.iter().map(|n| Some(*n)));
    let at = |letter: &str| {
        let (_, row, present) = jumps.iter().find(|(l, ..)| l == letter).unwrap();
        (*row, *present)
    };
    assert_eq!(at("#"), (0, true));
    assert_eq!(at("A"), (1, true));
    assert_eq!(at("B"), (3, false), "absent: where C starts");
    assert_eq!(at("C"), (3, true));
    assert_eq!(at("Z"), (4, false), "absent: where the next letter starts");
    assert_eq!(at("Ó"), (4, true));
    assert_eq!(at("Å"), (5, true));
    assert_eq!(jumps.len(), 29);
    assert!(letter_jumps(std::iter::empty()).is_empty());
}

#[test]
fn says_how_long_ago() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    let ago = |seconds: u64| ago(now - Duration::from_secs(seconds), now);
    assert_eq!(ago(5), "Just now");
    assert_eq!(ago(60), "1 minute ago");
    assert_eq!(ago(3 * 3600 + 5), "3 hours ago");
    assert_eq!(ago(86_400), "1 day ago");
    assert_eq!(ago(20 * 86_400), "2 weeks ago");
    assert_eq!(ago(100 * 86_400), "3 months ago");
    assert_eq!(ago(800 * 86_400), "2 years ago");
}

fn album(name: Option<&str>, year: Option<u32>) -> Album {
    Album {
        artist: Some("Shores of Null".into()),
        name: name.map(Into::into),
        year,
        tracks: 2,
        track: PathBuf::from("/music/x.flac"),
    }
}

fn track(album: &str, number: u32) -> Track {
    Track {
        id: 0,
        path: PathBuf::from(format!("/music/{album}/{number}.flac")),
        part: None,
        title: Some(format!("{album} {number}")),
        artist: Some("Shores of Null".into()),
        album: Some(album.into()),
        album_artist: Some("Shores of Null".into()),
        year: None,
        track: Some(number),
        disc: None,
        duration: None,
    }
}

#[test]
fn albums_sort_by_release_date_or_name() {
    let names = |albums: &[Album]| -> Vec<Option<String>> {
        albums.iter().map(|a| a.name.clone()).collect()
    };
    let mut albums = vec![
        album(Some("quiescence"), Some(2014)),
        album(None, Some(2030)),
        album(Some("Homesick"), Some(2026)),
        album(Some("Undated"), None),
        album(Some("Beyond the Shores"), Some(2020)),
    ];
    // Newest first; without a year last.
    AlbumSort::ReleaseDate.sort(&mut albums);
    assert_eq!(
        names(&albums),
        [
            None,
            Some("Homesick".into()),
            Some("Beyond the Shores".into()),
            Some("quiescence".into()),
            Some("Undated".into()),
        ]
    );
    // By name whatever the case; without a name last.
    AlbumSort::Name.sort(&mut albums);
    assert_eq!(
        names(&albums),
        [
            Some("Beyond the Shores".into()),
            Some("Homesick".into()),
            Some("quiescence".into()),
            Some("Undated".into()),
            None,
        ]
    );
    assert_eq!(AlbumSort::from_name("name"), Some(AlbumSort::Name));
    assert_eq!(
        AlbumSort::from_name(AlbumSort::ReleaseDate.name()),
        Some(AlbumSort::ReleaseDate)
    );
    assert_eq!(AlbumSort::from_name("year"), None);
}

#[test]
fn a_track_in_a_discography_plays_its_own_album() {
    let mut panel = LibraryPanel::new(None, None);
    panel.opened = vec![Level::ArtistAlbums {
        artist: Some("Shores of Null".into()),
    }];
    panel.album_list = true;
    panel.items = vec![
        Item::Album(album(Some("Homesick"), Some(2026))),
        Item::Track(track("Homesick", 1)),
        Item::Track(track("Homesick", 2)),
        Item::Album(album(Some("Quiescence"), Some(2014))),
        Item::Track(track("Quiescence", 1)),
        Item::Track(track("Quiescence", 2)),
    ];
    assert_eq!(panel.values()[1].1, string("discography"));
    let (tracks, start) = panel.activate(5).unwrap();
    let titles: Vec<_> = tracks.iter().filter_map(|t| t.title.clone()).collect();
    assert_eq!(titles, ["Quiescence 1", "Quiescence 2"]);
    assert_eq!(start, 1);
    let (tracks, start) = panel.activate(1).unwrap();
    assert_eq!(tracks.len(), 2);
    assert_eq!(start, 0);

    // As a grid, the same level is covers.
    panel.album_list = false;
    assert_eq!(panel.values()[1].1, string("grid"));
}

#[test]
fn the_track_playing_is_marked_by_its_file_and_part() {
    let mut panel = LibraryPanel::new(None, None);
    let disc = |n: u64| Track {
        path: PathBuf::from("/music/disc.flac"),
        part: Some(Part {
            start: Duration::from_secs(n * 200),
            end: None,
        }),
        ..track("Disc", n as u32)
    };
    panel.items = vec![
        Item::Track(track("Homesick", 1)),
        Item::Track(disc(0)),
        Item::Track(disc(1)),
    ];
    let playing_row = |panel: &LibraryPanel| match panel
        .values()
        .into_iter()
        .find(|(name, _)| *name == "playing-row")
    {
        Some((_, Value::Number(row))) => row,
        other => panic!("{other:?}"),
    };
    assert_eq!(playing_row(&panel), -1.0);
    // The second track of a disc ripped as one file: the same file, a later start.
    panel.set_playing(Some((
        Path::new("/music/disc.flac"),
        Duration::from_secs(200),
    )));
    assert_eq!(playing_row(&panel), 2.0);
    panel.set_playing(Some((Path::new("/music/Homesick/1.flac"), Duration::ZERO)));
    assert_eq!(playing_row(&panel), 0.0);
    panel.set_playing(None);
    assert_eq!(playing_row(&panel), -1.0);
}

#[test]
fn an_albums_page_has_its_heading_and_each_tracks_plays() {
    let dir = std::env::temp_dir().join(format!("freya-album-page-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
    for name in ["sine.flac", "sine.ogg"] {
        std::fs::copy(fixtures.join(name), dir.join(name)).unwrap();
    }
    let mut db = Library::open_in_memory().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
    for _ in 0..1203 {
        db.record_play(&dir.join("sine.flac"), None, SystemTime::now())
            .unwrap();
    }
    let album = db
        .all_albums()
        .unwrap()
        .into_iter()
        .next()
        .expect("an album");

    let mut panel = LibraryPanel::new(None, None);
    panel.open(local(db, None));
    panel.opened = vec![
        Level::Albums,
        Level::AlbumTracks {
            artist: album.artist.clone(),
            album: album.name.clone(),
        },
    ];
    panel.reload();
    let Value::Struct(page) = panel.album_page_value() else {
        panic!("a struct");
    };
    let text = |name: &str| match page.get_field(name) {
        Some(Value::String(s)) => s.to_string(),
        other => panic!("{name}: {other:?}"),
    };
    assert_eq!(page.get_field("shown"), Some(&Value::Bool(true)));
    assert_eq!(text("title"), album_name(album.name.as_deref()));
    assert_eq!(text("artist"), artist_name(album.artist.as_deref()));
    assert!(text("detail").contains("2 tracks"), "{}", text("detail"));
    assert!(!text("cover").is_empty());

    use slint::Model;
    let Value::Model(rows) = panel.rows.model_value() else {
        panic!("rows are a model");
    };
    let plays: Vec<String> = rows
        .iter()
        .map(|row| match row {
            Value::Struct(row) => match row.get_field("plays") {
                Some(Value::String(s)) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        })
        .collect();
    let mut sorted = plays.clone();
    sorted.sort();
    assert_eq!(sorted, ["", "1,203"], "{plays:?}");

    // Searching, or anywhere else, has no page.
    panel.search("sine");
    assert_eq!(
        match panel.album_page_value() {
            Value::Struct(page) => page.get_field("shown").cloned(),
            _ => None,
        },
        Some(Value::Bool(false))
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_artists_page_has_their_summary_and_most_played_tracks() {
    let dir = std::env::temp_dir().join(format!("freya-artist-page-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
    for name in ["sine.flac", "sine.ogg"] {
        std::fs::copy(fixtures.join(name), dir.join(name)).unwrap();
    }
    let mut db = Library::open_in_memory().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
    for _ in 0..3 {
        db.record_play(&dir.join("sine.ogg"), None, SystemTime::now())
            .unwrap();
    }
    let artist = db
        .artists()
        .unwrap()
        .into_iter()
        .next()
        .expect("an artist")
        .name;

    let mut panel = LibraryPanel::new(None, None);
    panel.open(local(db, None));
    panel.settings.artist_bios_online = false;
    panel.opened = vec![
        Level::Artists,
        Level::ArtistAlbums {
            artist: artist.clone(),
        },
    ];
    panel.reload();
    let Value::Struct(page) = panel.artist_page_value() else {
        panic!("a struct");
    };
    assert_eq!(page.get_field("shown"), Some(&Value::Bool(true)));
    assert_eq!(
        page.get_field("name"),
        Some(&string(artist_name(artist.as_deref())))
    );
    let Some(Value::String(detail)) = page.get_field("detail") else {
        panic!("a detail");
    };
    assert!(detail.contains("1 album · 2 tracks"), "{detail}");
    // Only what was played is popular, numbered from 1, with its plays.
    let tracks = panel.popular_tracks();
    assert_eq!(tracks.len(), 1);
    assert!(tracks[0].path.ends_with("sine.ogg"));
    let Value::Model(popular) = panel.popular_rows.model_value() else {
        panic!("a model");
    };
    use slint::Model;
    let Some(Value::Struct(row)) = popular.row_data(0) else {
        panic!("a row");
    };
    assert_eq!(row.get_field("number"), Some(&string("1")));
    assert_eq!(row.get_field("plays"), Some(&string("3")));

    // Elsewhere, no page.
    panel.opened = vec![Level::Artists];
    panel.reload();
    assert!(panel.popular_tracks().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn albums_are_as_long_as_their_tracks_in_words() {
    assert_eq!(long_time(Duration::from_secs(47 * 60 + 34)), "47 min 34 s");
    assert_eq!(
        long_time(Duration::from_secs(3600 + 5 * 60 + 27)),
        "1 h 5 min"
    );
}

#[test]
fn typing_a_search_is_one_place() {
    let mut panel = LibraryPanel::new(None, None);
    let browsing = panel.place();
    panel.search("sho");
    let searching = panel.place();
    panel.search("shores");
    let typed_on = panel.place();
    assert!(
        !browsing.continued_by(&searching),
        "a search starts a place"
    );
    assert!(searching.continued_by(&typed_on));
    panel.search("");
    assert!(
        !typed_on.continued_by(&panel.place()),
        "ending it is another"
    );
    // Back where the search was.
    panel.go_to(&typed_on);
    assert_eq!(panel.place(), typed_on);
}

#[test]
fn sections_have_their_own_layouts() {
    for name in [
        "artists",
        "albums",
        "songs",
        "genres",
        "recently-added",
        "recently-played",
        "most-played",
    ] {
        assert_eq!(Level::section(name).unwrap().section_name(), name);
    }
    assert_eq!(Level::section("podcasts"), None);
    assert_eq!(Level::Albums.layout(), "grid");
    assert_eq!(Level::GenreAlbums { genre: None }.layout(), "grid");
    assert_eq!(Level::Songs.layout(), "table");
    assert_eq!(Level::Genres.layout(), "grid");
    assert_eq!(Level::RecentlyAdded.layout(), "grid");
    assert_eq!(Level::MostPlayed.layout(), "table");
}
