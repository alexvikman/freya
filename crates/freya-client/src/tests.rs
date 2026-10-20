use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use freya_library::Library;
use freya_library::pictures::Pictures;
use freya_protocol::{
    AlbumOrder, ArtistEdit, LibraryApi, LibraryData, LibraryPictures, ServerControl, ServerEvents,
    ServerSettings, ServiceEvent,
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
        accounts: None,
    };
    let server = Server::start("127.0.0.1:0", freya_server::handler(served)).unwrap();
    let remote = connect(&format!("http://{}", server.address()), None, None);
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
    let gone = connect("http://127.0.0.1:1", None, None).stats();
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
        scrobbles: None,
    };
    let service = Arc::new(Service::start(dir.join("library.db"), None, config).unwrap());
    let served = Served {
        library: service.api(),
        server: Some(Arc::clone(&service) as _),
        quit: None,
        accounts: None,
    };
    let server = Server::start("127.0.0.1:0", freya_server::handler(served)).unwrap();
    let remote = connect(&format!("http://{}", server.address()), None, None);

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

#[test]
fn a_server_elsewhere_has_its_pictures_kept_here() {
    assert!(is_this_computer("http://127.0.0.1:47474"));
    assert!(is_this_computer("http://localhost:47474/"));
    assert!(!is_this_computer("https://mini.local:47474"));

    let dir = std::env::temp_dir().join(format!("freya-client-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = PictureCache::new(dir.join("cache"));
    let checksum = "a".repeat(64);
    assert_eq!(cache.get(&checksum), None);
    cache.put(&checksum, b"picture");
    assert_eq!(cache.get(&checksum).as_deref(), Some(&b"picture"[..]));
    // Over the limit, those used longest ago go first.
    let old = "b".repeat(64);
    cache.put(&old, b"older one");
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .append(true)
        .open(dir.join("cache").join(&old))
        .unwrap()
        .set_modified(long_ago)
        .unwrap();
    tidy(&dir.join("cache"), 10);
    assert_eq!(cache.get(&old), None);
    assert!(cache.get(&checksum).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_tracks_file_is_served_whole_or_in_parts() {
    let (server, _remote, local, dir) = served("audio");
    let address = format!("http://{}", server.address());
    let track = local.all_tracks().unwrap()[0].path.clone();
    let bytes = std::fs::read(&track).unwrap();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let get = |url: &str, range: Option<&str>| {
        let mut request = agent.get(url);
        if let Some(range) = range {
            request = request.header("Range", range);
        }
        let mut answer = request.call().unwrap();
        let status = answer.status().as_u16();
        let said = answer
            .headers()
            .get("content-range")
            .map(|v| v.to_str().unwrap().to_owned());
        let body = answer.body_mut().read_to_vec().unwrap();
        (status, said, body)
    };
    let url = audio_url(&address, &track);
    let (status, said, body) = get(&url, Some("bytes=10-109"));
    assert_eq!(status, 206);
    assert_eq!(said, Some(format!("bytes 10-109/{}", bytes.len())));
    assert_eq!(body, bytes[10..110]);
    let (status, _, body) = get(&url, None);
    assert_eq!((status, body.len()), (200, bytes.len()));
    let (status, said, _) = get(&url, Some(&format!("bytes={}-", bytes.len())));
    assert_eq!(status, 416);
    assert_eq!(said, Some(format!("bytes */{}", bytes.len())));
    // Only the library's music.
    let (status, _, _) = get(&audio_url(&address, &dir.join("library.db")), None);
    assert_eq!(status, 404);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_track_plays_from_the_server() {
    let (server, _remote, local, dir) = served("plays");
    let address = format!("http://{}", server.address());
    let track = local
        .all_tracks()
        .unwrap()
        .into_iter()
        .find(|t| t.path.extension().is_some_and(|e| e == "flac"))
        .unwrap();
    let player = freya_audio::Player::silent(48_000);
    player.play(freya_audio::Media {
        remote: Some(freya_audio::RemoteSource {
            url: audio_url(&address, &track.path),
            token: None,
        }),
        ..freya_audio::Media::from(&track.path)
    });
    let next = || {
        player
            .events()
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap()
    };
    match next() {
        freya_audio::Event::TrackStarted(info) => {
            // Named by its path in the library, though read from the server.
            assert_eq!(info.path, track.path);
            assert!(info.duration.is_some());
        }
        other => panic!("expected the track to start, got {other:?}"),
    }
    loop {
        match next() {
            freya_audio::Event::Finished => break,
            freya_audio::Event::Unplayable { .. } => panic!("it could not be played"),
            _ => {}
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_a_device_with_a_token_is_answered() {
    let dir = std::env::temp_dir().join(format!("freya-client-tokens-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("music")).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../freya-audio/tests/fixtures");
    std::fs::copy(fixtures.join("sine.flac"), dir.join("music/sine.flac")).unwrap();
    let path = dir.join("library.db");
    let mut db = Library::open(&path).unwrap();
    freya_library::library::scan(
        &mut db,
        &[dir.join("music")],
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let config = ServiceConfig {
        settings: ServerSettings {
            watch_folders: false,
            ..ServerSettings::default()
        },
        ..ServiceConfig::default()
    };
    let service = Arc::new(Service::start(path, None, config).unwrap());
    let pairing_key = dir.join("pairing.key");
    let accounts = Arc::new(freya_server::accounts::Accounts::open(
        Some(dir.join("accounts.json")),
        Some(pairing_key.clone()),
    ));
    let served = Served {
        library: service.api(),
        server: Some(Arc::clone(&service) as _),
        quit: None,
        accounts: Some(accounts),
    };
    let server = Server::start("127.0.0.1:0", freya_server::handler(served)).unwrap();
    let address = format!("http://{}", server.address());

    // Without a token, only the version.
    let stranger = connect(&address, None, None);
    assert!(stranger.0.version().is_ok());
    assert!(stranger.stats().is_err());
    assert!(stranger.events().is_err());
    assert!(stranger.picture(&"0".repeat(64)).is_err());
    assert_eq!(stranger.0.account().unwrap(), None);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let track = dir.join("music/sine.flac");
    let audio = agent.get(&audio_url(&address, &track)).call().unwrap();
    assert_eq!(audio.status().as_u16(), 401);

    // Paired with the key in the file, once.
    let key = std::fs::read_to_string(&pairing_key).unwrap();
    let token = Http::new(&address).pair(&key, "Mac").unwrap();
    assert!(Http::new(&address).pair(&key, "Mac again").is_err());
    let mac = connect(&address, Some(token.clone()), None);
    assert_eq!(mac.stats().unwrap().tracks, 1);
    assert!(mac.events().is_ok());
    let audio = agent
        .get(&audio_url(&address, &track))
        .header("Authorization", format!("Bearer {token}"))
        .call()
        .unwrap();
    assert_eq!(audio.status().as_u16(), 200);

    // Another device logs in once the user has a password.
    let user = mac.0.account().unwrap().unwrap().user;
    assert!(
        Http::new(&address)
            .login(&user, "correct horse", "PC")
            .is_err()
    );
    mac.0.set_password("correct horse").unwrap();
    let pc = Http::new(&address)
        .login(&user, "correct horse", "PC")
        .unwrap();
    let pc = connect(&address, Some(pc), None);
    let devices = pc.0.account().unwrap().unwrap().devices;
    assert_eq!(devices.len(), 2);
    // The Mac takes the PC's token back.
    let pc_device = devices.iter().find(|d| d.this).unwrap().id.clone();
    mac.0.revoke(&pc_device).unwrap();
    assert!(pc.stats().is_err());
    assert!(mac.stats().is_ok());
    drop(server);
    drop(service);
    let _ = std::fs::remove_dir_all(&dir);
}
