use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use freya_library::Library;
use freya_library::pictures::Pictures;
use freya_protocol::{
    AlbumOrder, ArtistEdit, LibraryApi, LibraryData, ServerControl, ServerEvents, ServerSettings,
    ServiceEvent,
};
use freya_server::http::Server;
use freya_server::{LocalLibrary, Served, Service, ServiceConfig};

use super::*;

/// A library of the test fixtures, served over HTTP: the server, the library as served, and
/// as it is in this process.
fn served(name: &str) -> (Server, Arc<dyn LibraryApi>, Arc<dyn LibraryApi>, PathBuf) {
    let dir = std::env::temp_dir().join(format!("freya-client-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("music")).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../freya-audio/tests/fixtures");
    for file in ["sine.flac", "sine.mp3"] {
        std::fs::copy(fixtures.join(file), dir.join("music").join(file)).unwrap();
    }
    let path = dir.join("library.db");
    let mut db = Library::open(&path).unwrap();
    freya_library::library::scan(
        &mut db,
        &[dir.join("music")],
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let pictures = || Some(Pictures::new(dir.join("pictures")));
    let local: Arc<dyn LibraryApi> = Arc::new(LocalLibrary::new(db, pictures()));
    let served = Served {
        library: Arc::new(LocalLibrary::new(Library::open(&path).unwrap(), pictures())),
        server: None,
        quit: None,
    };
    let server = Server::start("127.0.0.1:0", freya_server::handler(served)).unwrap();
    let remote = connect(&format!("http://{}", server.address()));
    (server, remote, local, dir)
}

#[test]
fn a_remote_library_answers_as_the_library_itself() {
    let (_server, remote, local, dir) = served("answers");
    assert_eq!(remote.stats().unwrap(), local.stats().unwrap());
    assert_eq!(remote.all_tracks().unwrap(), local.all_tracks().unwrap());
    assert_eq!(remote.all_tracks().unwrap().len(), 2);
    assert_eq!(
        remote.albums_by(AlbumOrder::Name).unwrap(),
        local.albums_by(AlbumOrder::Name).unwrap()
    );
    assert_eq!(
        remote.search("sine", 10).unwrap(),
        local.search("sine", 10).unwrap()
    );
    let track = &local.all_tracks().unwrap()[0];
    assert_eq!(
        remote
            .track_at(&track.path, std::time::Duration::ZERO)
            .unwrap(),
        Some(track.clone())
    );
    // Written over the network, read here.
    let edit = ArtistEdit {
        name: Some("Sines".into()),
        ..ArtistEdit::default()
    };
    remote.set_artist_edit("Freya", &edit).unwrap();
    assert_eq!(local.artist_edit("Freya").unwrap(), edit);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pictures_go_both_ways_and_only_the_librarys_music_is_read() {
    let (_server, remote, local, dir) = served("pictures");
    let mut png = Vec::new();
    image::RgbImage::from_pixel(300, 200, image::Rgb([0, 0, 200]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let kept = remote.put_picture(&png).unwrap();
    assert_eq!(remote.picture(&kept).unwrap(), Some(png.clone()));
    let smaller = remote.take_picture(&png, 100).unwrap();
    let small = image::load_from_memory(&local.picture(&smaller).unwrap().unwrap()).unwrap();
    assert_eq!((small.width(), small.height()), (100, 67));
    assert_eq!(remote.picture(&"0".repeat(64)).unwrap(), None);
    assert_eq!(remote.picture("../../library.db").unwrap(), None);
    // A file outside the library is not read, even one that is there.
    assert_eq!(
        remote.album_art(&dir.join("library.db"), 100).unwrap(),
        None
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_call_says_why() {
    let (_server, remote, _local, dir) = served("errors");
    let http = Http::new(&format!("http://{}", _server.address()));
    let unknown = http.call("no_such_call", serde_json::Value::Null);
    assert!(unknown.unwrap_err().0.contains("no such call"));
    let bad = http.call("search", serde_json::json!(["only one"]));
    assert!(bad.unwrap_err().0.contains("could not read what was sent"));
    drop(remote);
    let gone = connect("http://127.0.0.1:1").stats();
    assert!(gone.unwrap_err().0.contains("could not reach the server"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_server_is_told_what_to_do_and_tells_what_happens() {
    let dir = std::env::temp_dir().join(format!("freya-client-control-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("music")).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../freya-audio/tests/fixtures");
    std::fs::copy(fixtures.join("sine.flac"), dir.join("music/sine.flac")).unwrap();
    let settings = ServerSettings {
        library_folders: vec![dir.join("music")],
        watch_folders: false,
        ..ServerSettings::default()
    };
    // Scanned once, so it is not scanning when the client comes.
    {
        let mut db = Library::open(&dir.join("library.db")).unwrap();
        freya_library::library::scan(
            &mut db,
            &settings.library_folders,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        db.note_full_scan(std::time::SystemTime::now()).unwrap();
    }
    let config = ServiceConfig {
        settings: settings.clone(),
        settings_file: Some(dir.join("settings.toml")),
        scans_allowed: true,
        lastfm: None,
        playlists: Some(dir.join("playlists")),
    };
    let service = Arc::new(Service::start(dir.join("library.db"), None, config).unwrap());
    let served = Served {
        library: service.api(),
        server: Some(Arc::clone(&service) as _),
        quit: None,
    };
    let server = Server::start("127.0.0.1:0", freya_server::handler(served)).unwrap();
    let remote = connect(&format!("http://{}", server.address()));

    assert_eq!(remote.settings().unwrap(), settings);
    let changed = ServerSettings {
        scan_every_hours: 6,
        ..settings
    };
    remote.set_settings(&changed).unwrap();
    assert_eq!(service.settings().unwrap(), changed);
    assert_eq!(
        freya_server::settings::load(&dir.join("settings.toml")).0,
        changed
    );

    let events = remote.events().unwrap();
    let next = || {
        events
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap()
    };
    assert_eq!(next(), ServiceEvent::Connected { scanning: false });
    remote.rescan().unwrap();
    assert_eq!(next(), ServiceEvent::ScanStarted);
    while next() != ServiceEvent::ScanEnded {}
    assert!(!remote.is_scanning().unwrap());
    assert!(
        remote
            .record_play(
                &dir.join("music/sine.flac"),
                None,
                std::time::SystemTime::now()
            )
            .unwrap()
    );

    // Playlists, kept by the server; every client is told of a change.
    let entry = freya_protocol::PlaylistEntry {
        path: dir.join("music/sine.flac"),
        part: None,
        title: Some("Sine".into()),
        artist: None,
        album: None,
        duration: Some(std::time::Duration::from_secs(1)),
    };
    remote
        .save_playlist("Calm", std::slice::from_ref(&entry))
        .unwrap();
    assert_eq!(next(), ServiceEvent::PlaylistsChanged);
    let saved = remote.playlists().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(
        (saved[0].name.as_str(), &saved[0].entries),
        ("Calm", &vec![entry])
    );
    remote
        .rename_saved_playlist("Calm", "Calm evenings")
        .unwrap();
    assert_eq!(remote.playlists().unwrap()[0].name, "Calm evenings");
    remote.delete_playlist("Calm evenings").unwrap();
    assert!(remote.playlists().unwrap().is_empty());
    assert!(remote.save_playlist("../escape", &[]).is_err());
    drop(server);
    drop(service);
    let _ = std::fs::remove_dir_all(&dir);
}
