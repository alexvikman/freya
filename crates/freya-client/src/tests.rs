use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use freya_library::Library;
use freya_media::pictures::Pictures;
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
        accounts: None,
        certificate: None,
        callback_port: freya_protocol::CALLBACK_PORT,
        updates: None,
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
    // Small, as a grid shows it.
    let mut large = Vec::new();
    image::RgbImage::from_pixel(1200, 600, image::Rgb([0, 200, 0]))
        .write_to(
            &mut std::io::Cursor::new(&mut large),
            image::ImageFormat::Png,
        )
        .unwrap();
    let kept = remote.put_picture(&large).unwrap();
    let small = remote.small_picture(&kept).unwrap().unwrap();
    assert_eq!(
        image::guess_format(&small).unwrap(),
        image::ImageFormat::Jpeg
    );
    let small = image::load_from_memory(&small).unwrap();
    assert_eq!((small.width(), small.height()), (300, 150));
    assert_eq!(remote.small_picture(&"0".repeat(64)).unwrap(), None);
    assert_eq!(remote.small_picture("../../library.db").unwrap(), None);
    // Several at once, each in its place.
    let asked = [
        kept.clone(),
        "0".repeat(64),
        "../../library.db".to_owned(),
        kept.clone(),
    ];
    let smalls = remote.small_pictures(&asked).unwrap();
    assert_eq!(smalls.len(), 4);
    assert!(smalls[0].is_some() && smalls[3].is_some());
    assert!(smalls[1].is_none() && smalls[2].is_none());
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
    let unknown = http.call("no_such_call", b"null".to_vec());
    assert!(unknown.unwrap_err().0.contains("no such call"));
    let bad = http.call("search", br#"["only one"]"#.to_vec());
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
        stations: Some(dir.join("stations.m3u8")),
        scrobbles: None,
    };
    let service = Arc::new(Service::start(dir.join("library.db"), None, config).unwrap());
    let served = Served {
        library: service.api(),
        server: Some(Arc::clone(&service) as _),
        accounts: None,
        certificate: None,
        callback_port: freya_protocol::CALLBACK_PORT,
        updates: None,
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
    // What the library holds changes as the server works: the events of interest are others.
    let next = || loop {
        let event = events
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap();
        if event != ServiceEvent::Changed {
            break event;
        }
    };
    assert!(matches!(
        next(),
        ServiceEvent::Connected {
            scanning: false,
            ..
        }
    ));
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

    // The server's folders, to choose the music's from another computer.
    let listing = remote.server_folders(Some(&dir)).unwrap();
    assert!(listing.folders.contains(&dir.join("music")));
    assert!(remote.server_folders(None).unwrap().folder.is_none());

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

    // Radio stations, kept by the server, which starts with some; every client is told of a
    // change.
    let mut stations = remote.stations().unwrap();
    assert!(!stations.is_empty());
    stations.push(freya_protocol::RadioStation {
        name: "Night Jazz".into(),
        url: "https://jazz.example.org/".into(),
    });
    remote.save_stations(&stations).unwrap();
    while next() != ServiceEvent::StationsChanged {}
    assert_eq!(remote.stations().unwrap(), stations);
    drop(server);
    drop(service);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn calls_asked_at_once_are_answered_by_the_calls_that_follow() {
    let (server, remote, local, dir) = served("at-once");
    let track = local.all_tracks().unwrap()[0].clone();
    remote.prefetch(&[
        ("stats", freya_protocol::args(&())),
        // Asked twice, it is asked once.
        ("stats", freya_protocol::args(&())),
        ("all_tracks", freya_protocol::args(&())),
        (
            "album_tracks",
            freya_protocol::args(&(None::<&str>, Some("Sines"))),
        ),
        (
            "track_at",
            freya_protocol::args(&(&track.path, std::time::Duration::ZERO)),
        ),
        // Not one to ask at once, and not a call: neither keeps the others from coming.
        (
            "set_artist_edit",
            freya_protocol::args(&("Freya", ArtistEdit::default())),
        ),
        ("no_such_call", freya_protocol::args(&())),
    ]);
    // With the server gone, they are answered as they were.
    drop(server);
    assert_eq!(remote.stats().unwrap(), local.stats().unwrap());
    assert_eq!(remote.stats().unwrap(), local.stats().unwrap());
    assert_eq!(remote.all_tracks().unwrap(), local.all_tracks().unwrap());
    assert_eq!(
        remote
            .track_at(&track.path, std::time::Duration::ZERO)
            .unwrap(),
        Some(track)
    );
    assert!(remote.album_tracks(None, Some("Sines")).is_ok());
    assert!(remote.artist_edits().is_err());
    // For a moment only: then they are asked again.
    std::thread::sleep(std::time::Duration::from_millis(2100));
    assert!(remote.stats().is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn answers_kept_are_never_out_of_date() {
    let dir = std::env::temp_dir().join(format!("freya-client-kept-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("music")).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../freya-audio/tests/fixtures");
    std::fs::copy(fixtures.join("sine.flac"), dir.join("music/sine.flac")).unwrap();
    let config = ServiceConfig {
        settings: ServerSettings {
            library_folders: vec![dir.join("music")],
            watch_folders: false,
            ..ServerSettings::default()
        },
        ..ServiceConfig::default()
    };
    let service = Arc::new(Service::start(dir.join("library.db"), None, config).unwrap());
    let served = Served {
        library: service.api(),
        server: Some(Arc::clone(&service) as _),
        accounts: None,
        certificate: None,
        callback_port: freya_protocol::CALLBACK_PORT,
        updates: None,
    };
    let server = Server::start("127.0.0.1:0", freya_server::handler(served)).unwrap();
    let address = format!("http://{}", server.address());
    let (here, there) = (connect(&address, None, None), connect(&address, None, None));
    let edit = |name: &str| ArtistEdit {
        name: Some(name.into()),
        ..ArtistEdit::default()
    };
    let named = |remote: &Remote<Http>| remote.artist_edit("Freya").unwrap().name;

    // Not following the server's events, a client asks each time, and is told when what it
    // has is current.
    assert_eq!(named(&here), None);
    there.set_artist_edit("Freya", &edit("Sines")).unwrap();
    assert_eq!(named(&here).as_deref(), Some("Sines"));
    // Its own change is read at once.
    here.set_artist_edit("Freya", &edit("Waves")).unwrap();
    assert_eq!(named(&here).as_deref(), Some("Waves"));

    // Following them, it asks again once the server says the library changed.
    let events = here.events().unwrap();
    let wait_for = |wanted: fn(&ServiceEvent) -> bool| loop {
        let event = events
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap();
        if wanted(&event) {
            break;
        }
    };
    wait_for(|event| matches!(event, ServiceEvent::Connected { changes: true, .. }));
    assert_eq!(named(&here).as_deref(), Some("Waves"));
    there.set_artist_edit("Freya", &edit("Tides")).unwrap();
    wait_for(|event| *event == ServiceEvent::Changed);
    assert_eq!(named(&here).as_deref(), Some("Tides"));
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
    let checksum = checksum_of(b"picture");
    assert_eq!(cache.get(&checksum), None);
    cache.put(&checksum, b"picture");
    assert_eq!(cache.get(&checksum).as_deref(), Some(&b"picture"[..]));
    // Over the limit, those used longest ago go first.
    let old = checksum_of(b"older one");
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
            bearer: None,
            agent: None,
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

/// An update of the server always available, counting how often it was asked for anew.
#[derive(Default)]
struct Offered(std::sync::atomic::AtomicUsize);

impl freya_server::Updating for Offered {
    fn status(&self) -> freya_protocol::ServerUpdate {
        freya_protocol::ServerUpdate {
            state: "available".into(),
            version: "9.0.0".into(),
            ..Default::default()
        }
    }

    fn check(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn install(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The tokens of the admin Alex's device `device`, logged in with their password as the
/// browser's login does.
fn logged_in(
    accounts: &freya_server::accounts::Accounts,
    device: &str,
) -> freya_server::accounts::Granted {
    let (redirect, verifier) = (
        "http://127.0.0.1:47477/callback",
        "a secret only the client knows, long enough",
    );
    let challenge = freya_server::accounts::challenge(verifier);
    let code = accounts
        .grant("Alex", "correct horse", &challenge, redirect, None)
        .unwrap();
    accounts.redeem(&code, verifier, redirect, device).unwrap()
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
            library_folders: vec![dir.join("music")],
            ..ServerSettings::default()
        },
        stations: Some(dir.join("stations.m3u8")),
        ..ServiceConfig::default()
    };
    let service = Arc::new(Service::start(path, None, config).unwrap());
    let accounts = Arc::new(freya_server::accounts::Accounts::open(Some(
        dir.join("accounts.json"),
    )));
    accounts
        .add_user("Alex", freya_protocol::Role::Admin, "correct horse")
        .unwrap();
    let offered = Arc::new(Offered::default());
    let served = Served {
        library: service.api(),
        server: Some(Arc::clone(&service) as _),
        accounts: Some(Arc::clone(&accounts)),
        certificate: None,
        callback_port: freya_protocol::CALLBACK_PORT,
        updates: Some(Arc::clone(&offered) as _),
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
    // As the scan names it, on Windows too.
    let track = dir.join("music").join("sine.flac");
    let audio = agent.get(&audio_url(&address, &track)).call().unwrap();
    assert_eq!(audio.status().as_u16(), 401);

    // Logged in with a password: there is no other way in.
    let token = logged_in(&accounts, "Mac").access;
    let mac = connect(&address, Some(token.clone()), None);
    assert_eq!(mac.stats().unwrap().tracks, 1);
    // An admin sees the server's update and asks for one.
    assert_eq!(mac.0.server_update().unwrap().version, "9.0.0");
    mac.0.check_server_update().unwrap();
    assert_eq!(offered.0.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(stranger.0.server_update().is_err());
    assert!(mac.events().is_ok());
    let audio = agent
        .get(&audio_url(&address, &track))
        .header("Authorization", format!("Bearer {token}"))
        .call()
        .unwrap();
    assert_eq!(audio.status().as_u16(), 200);

    // A device with only its refresh token renews, keeping the new one; one whose access token
    // the server does not take renews and asks again. A refresh token used up, coming again, was
    // copied: the device is logged out.
    let store = TokenStore::new(dir.join("servers.json"), false);
    let tablet = logged_in(&accounts, "Tablet");
    let kept_in = || Some((store.clone(), address.clone()));
    let renewing =
        Remote(Http::new(&address).with_session(tablet.refresh.clone(), None, kept_in()));
    assert_eq!(renewing.stats().unwrap().tracks, 1);
    let kept = store.get(&address).unwrap();
    assert_ne!(kept, tablet.refresh);
    let stale = Granted {
        access: "not.a.token".into(),
        refresh: kept.clone(),
        lasts: Duration::from_secs(3600),
    };
    let refused = Remote(Http::new(&address).with_session(kept.clone(), Some(&stale), kept_in()));
    assert_eq!(refused.stats().unwrap().tracks, 1);
    assert_ne!(store.get(&address).unwrap(), kept);
    let copied = Remote(Http::new(&address).with_session(tablet.refresh.clone(), None, None));
    assert!(copied.stats().is_err());
    assert!(refused.stats().is_err());
    store.remove(&address);
    assert_eq!(store.get(&address), None);

    // Another device logs in in the browser, with the password; the page a ticket opens to
    // change it asks for the one now first.
    let user = mac.0.account().unwrap().unwrap().user;
    let pages = Http::new(&address).pages();
    let login = Login::new("http://127.0.0.1:47477/callback");
    assert_eq!(log_in(&pages, &login, &user, "wrong horse").status, 401);
    let ticket = mac.0.password_ticket().unwrap();
    let page = format!("/web/password?ticket={ticket}");
    let shown = pages.ask("GET", &page, None, Vec::new()).unwrap();
    assert_eq!(shown.status, 200);
    assert!(String::from_utf8_lossy(&shown.body).contains(r#"name="current""#));

    // The page sends the browser back with a code, which only this login trades for a token,
    // once.
    let sent = log_in(&pages, &login, &user, "correct horse");
    assert_eq!(sent.status, 303);
    let location = &sent
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("location"))
        .unwrap()
        .1;
    assert!(location.starts_with("http://127.0.0.1:47477/callback?code="));
    assert!(location.ends_with(&format!("&state={}", login.state)));
    let code = location["http://127.0.0.1:47477/callback?code=".len()..]
        .split('&')
        .next()
        .unwrap();
    let other = Login::new("http://127.0.0.1:47477/callback");
    assert!(Http::new(&address).redeem(&other, code, "PC").is_err());
    let sent = log_in(&pages, &login, &user, "correct horse");
    let location = &sent
        .headers
        .iter()
        .find(|(n, _)| n == "location")
        .unwrap()
        .1;
    let code = location
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    let pc = Http::new(&address).redeem(&login, code, "PC").unwrap();
    assert!(Http::new(&address).redeem(&login, code, "PC").is_err());
    let pc = connect(&address, Some(pc.access), None);
    let devices = pc.0.account().unwrap().unwrap().devices;
    assert_eq!(devices.len(), 2);
    // The Mac takes the PC's token back.
    let pc_device = devices.iter().find(|d| d.this).unwrap().id.clone();
    mac.0.revoke(&pc_device).unwrap();
    assert!(pc.stats().is_err());
    assert!(mac.stats().is_ok());
    // A listener plays the library, but neither changes the server, reads its settings, nor
    // reads or changes the owner's plays, favourites and playlists.
    accounts
        .add_user("Kim", freya_protocol::Role::Listener, "battery staple")
        .unwrap();
    let login = Login::new("http://127.0.0.1:47477/callback");
    let sent = log_in(&pages, &login, "Kim", "battery staple");
    let location = &sent
        .headers
        .iter()
        .find(|(n, _)| n == "location")
        .unwrap()
        .1;
    let code = location
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    let kim = Http::new(&address).redeem(&login, code, "Phone").unwrap();
    let audio = agent
        .get(&audio_url(&address, &track))
        .header("Authorization", format!("Bearer {}", kim.access))
        .call()
        .unwrap();
    assert_eq!(audio.status().as_u16(), 200);
    let phone = connect(&address, Some(kim.access), None);
    assert_eq!(phone.stats().unwrap().tracks, 1);
    assert_eq!(phone.all_tracks().unwrap().len(), 1);
    assert!(phone.events().is_ok());
    assert_eq!(
        phone.0.account().unwrap().unwrap().role,
        freya_protocol::Role::Listener
    );
    let at = std::time::SystemTime::now();
    assert!(phone.set_artist_favorite("Sine", true, at).is_err());
    // It plays the radio stations, but does not change them.
    let stations = phone.stations().unwrap();
    assert!(!stations.is_empty());
    assert!(phone.save_stations(&[]).is_err());
    // Asked at once with what it may ask, it is still refused what it may not.
    phone.prefetch(&[
        ("most_played", freya_protocol::args(&(10usize,))),
        ("stats", freya_protocol::args(&())),
    ]);
    assert!(phone.most_played(10).is_err());
    assert!(phone.playlists().is_err());
    assert!(phone.save_playlist("Kim's", &[]).is_err());
    let refused = phone.settings().unwrap_err();
    assert!(refused.0.contains("admin"), "{refused:?}");
    assert!(phone.0.server_update().is_err());
    assert!(phone.0.install_server_update().is_err());
    assert_eq!(offered.0.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(phone.set_settings(&ServerSettings::default()).is_err());
    assert!(phone.rescan().is_err());
    assert!(phone.server_folders(None).is_err());
    assert!(
        phone
            .set_artist_edit("Sine", &ArtistEdit::default())
            .is_err()
    );
    assert!(phone.put_picture(b"not a picture").is_err());

    // And logs itself out.
    mac.0.log_out().unwrap();
    assert!(mac.stats().is_err());
    drop(server);
    drop(service);
    let _ = std::fs::remove_dir_all(&dir);
}

const FORM: &str = "application/x-www-form-urlencoded";

/// Sends the login page's form as a browser does: what it was opened with, and what was typed.
fn log_in(pages: &Pages, login: &Login, user: &str, password: &str) -> Page {
    let page = login.page();
    assert_eq!(
        pages.ask("GET", &page, None, Vec::new()).unwrap().status,
        200
    );
    let opened = page.split_once('?').unwrap().1;
    let typed = format!("username={user}&password={}", password.replace(' ', "+"));
    pages
        .ask(
            "POST",
            "/web/login",
            Some(FORM),
            format!("{opened}&{typed}").into_bytes(),
        )
        .unwrap()
}

#[test]
fn a_server_elsewhere_is_reached_over_tls_with_its_certificate_pinned() {
    let (_plain, _remote, local, dir) = served("tls");
    let identity = freya_server::tls::Identity::make().unwrap();
    let served = Served {
        library: Arc::clone(&local),
        server: None,
        accounts: None,
        certificate: Some(identity.certificate.clone()),
        callback_port: freya_protocol::CALLBACK_PORT,
        updates: None,
    };
    let server = Server::start_tls(
        "127.0.0.1:0",
        freya_server::handler(served),
        identity.server_config().unwrap(),
    )
    .unwrap();
    let address = server.address();
    // The first time, its certificate is taken unchecked, to pin.
    let certificate = certificate_of(address).unwrap();
    assert_eq!(certificate, identity.certificate);
    let pin = Pin {
        address,
        certificate,
    };
    let remote = Arc::new(Remote(Http::pinned(pin.clone())));
    assert_eq!(remote.stats().unwrap(), local.stats().unwrap());
    // Another certificate is not taken.
    let other = Pin {
        certificate: freya_server::tls::Identity::make().unwrap().certificate,
        ..pin.clone()
    };
    assert!(Remote(Http::pinned(other)).stats().is_err());
    // Nor plain HTTP.
    assert!(
        connect(&format!("http://{address}"), None, None)
            .stats()
            .is_err()
    );

    // A track plays over it too.
    let track = local.all_tracks().unwrap()[0].path.clone();
    let (base, agent) = remote.0.audio();
    let player = freya_audio::Player::silent(48_000);
    player.play(freya_audio::Media {
        remote: Some(freya_audio::RemoteSource {
            url: audio_url_at(&base, &track),
            bearer: None,
            agent: Some(Arc::new(agent)),
        }),
        ..freya_audio::Media::from(&track)
    });
    let event = player
        .events()
        .recv_timeout(std::time::Duration::from_secs(20))
        .unwrap();
    assert!(
        matches!(event, freya_audio::Event::TrackStarted(_)),
        "{event:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn servers_on_the_network_are_found() {
    let found = freya_protocol::ServerFound {
        name: "Mini".into(),
        port: 47475,
        fingerprint: "AB CD".into(),
        version: "0".into(),
    };
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let at = socket.local_addr().unwrap();
    let answer = found.clone();
    std::thread::spawn(move || {
        let mut question = [0; 64];
        let (read, from) = socket.recv_from(&mut question).unwrap();
        assert_eq!(&question[..read], freya_protocol::DISCOVERY_QUESTION);
        let json = serde_json::to_vec(&answer).unwrap();
        // Twice, as one on two networks would: told once.
        socket.send_to(&json, from).unwrap();
        socket.send_to(&json, from).unwrap();
    });
    let servers = ask_for_servers(at, std::time::Duration::from_millis(500)).unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].server, found);
    assert_eq!(servers[0].address, "127.0.0.1:47475".parse().unwrap());
}

/// Adopts or joins the server through `pages` with the claim `ticket`, as `name` with
/// `password`: the code the page sends the browser back with.
fn claim(pages: &Pages, login: &Login, ticket: &str, name: &str, password: &str) -> String {
    let page = login.join_page();
    let shown = pages
        .ask("GET", &format!("{page}&ticket={ticket}"), None, Vec::new())
        .unwrap();
    assert_eq!(
        shown.status,
        200,
        "{}",
        String::from_utf8_lossy(&shown.body)
    );
    let opened = page.split_once('?').unwrap().1;
    let password = password.replace(' ', "+");
    let typed = format!("ticket={ticket}&username={name}&password={password}&again={password}");
    let sent = pages
        .ask(
            "POST",
            "/web/join",
            Some(FORM),
            format!("{opened}&{typed}").into_bytes(),
        )
        .unwrap();
    assert_eq!(sent.status, 303, "{}", String::from_utf8_lossy(&sent.body));
    let location = &sent
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("location"))
        .unwrap()
        .1;
    location["http://127.0.0.1:47477/callback?code=".len()..]
        .split('&')
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn a_server_is_adopted_and_joined_with_codes() {
    let (_plain, _remote, local, dir) = served("adopt");
    let identity = freya_server::tls::Identity::make().unwrap();
    let accounts = Arc::new(freya_server::accounts::Accounts::open(None));
    let served = Served {
        library: Arc::clone(&local),
        server: None,
        accounts: Some(Arc::clone(&accounts)),
        certificate: Some(identity.certificate.clone()),
        callback_port: freya_protocol::CALLBACK_PORT,
        updates: None,
    };
    let server = Server::start_tls(
        "127.0.0.1:0",
        freya_server::handler(served),
        identity.server_config().unwrap(),
    )
    .unwrap();
    let pin = Pin {
        address: server.address(),
        certificate: certificate_of(server.address()).unwrap(),
    };
    let http = Http::pinned(pin.clone());
    let pages = http.pages();
    let callback = "http://127.0.0.1:47477/callback";

    // Not adopted: the code it keeps lets a device adopt it, choosing the owner's name.
    assert!(!accounts.adopted());
    let (code, _) = accounts.adoption_code().unwrap();
    let (ticket, claimed) = verify_server(&pin, &code).unwrap().unwrap();
    assert_eq!(claimed, freya_protocol::Claim::Adopt);
    let login = Login::new(callback);
    let given = claim(&pages, &login, &ticket, "Ada", "correct horse");
    let laptop = http.redeem(&login, &given, "Laptop").unwrap();
    let owner = Http::pinned(pin.clone()).with_token(Some(laptop.access.clone()));
    let account = owner.account().unwrap().unwrap();
    assert_eq!(
        (account.user.as_str(), account.role),
        ("Ada", freya_protocol::Role::Admin)
    );
    assert!(accounts.adopted());
    assert_eq!(accounts.adoption_code(), None);
    // The ticket was used up, and a code now only logs in.
    let again = Login::new(callback);
    let used = pages
        .ask(
            "GET",
            &format!("{}&ticket={ticket}", again.join_page()),
            None,
            Vec::new(),
        )
        .unwrap();
    assert_eq!(used.status, 404);
    let (code, _) = accounts.connection_code();
    assert_eq!(verify_server(&pin, &code).unwrap(), None);

    // Kim is invited as a listener: the code proves the server to them, and lets them choose
    // their password.
    let (invitation, _) = owner
        .invite_user("Kim", freya_protocol::Role::Listener)
        .unwrap();
    let (ticket, claimed) = verify_server(&pin, &invitation).unwrap().unwrap();
    assert_eq!(claimed, freya_protocol::Claim::Join { user: "Kim".into() });
    let login = Login::new(callback);
    // The name stays the one invited, whatever is sent.
    let given = claim(&pages, &login, &ticket, "Mallory", "battery staple");
    let phone = http.redeem(&login, &given, "Phone").unwrap();
    let kim = Http::pinned(pin.clone()).with_token(Some(phone.access.clone()));
    let account = kim.account().unwrap().unwrap();
    assert_eq!(
        (account.user.as_str(), account.role),
        ("Kim", freya_protocol::Role::Listener)
    );
    // The invitation is used up.
    assert!(verify_server(&pin, &invitation).is_err());
    // A listener sees no users, and invites nobody.
    assert!(kim.users().is_err());
    assert!(kim.invite_user("Eve", freya_protocol::Role::Admin).is_err());

    let users = owner.users().unwrap();
    let names: Vec<(&str, bool, bool)> = users
        .iter()
        .map(|u| (u.name.as_str(), u.owner, u.joined))
        .collect();
    assert_eq!(names, [("Ada", true, true), ("Kim", false, true)]);
    assert_eq!(users[1].devices, 1);

    // The owner stays; Kim goes, with her devices.
    assert!(owner.remove_user("Ada").is_err());
    owner.remove_user("Kim").unwrap();
    assert!(kim.account().unwrap().is_none());
    assert_eq!(owner.users().unwrap().len(), 1);
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_new_device_knows_the_server_by_its_connection_code() {
    let (_plain, _remote, local, dir) = served("code");
    let identity = freya_server::tls::Identity::make().unwrap();
    let accounts = Arc::new(freya_server::accounts::Accounts::open(None));
    let served = Served {
        library: Arc::clone(&local),
        server: None,
        accounts: Some(Arc::clone(&accounts)),
        certificate: Some(identity.certificate.clone()),
        callback_port: freya_protocol::CALLBACK_PORT,
        updates: None,
    };
    let server = Server::start_tls(
        "127.0.0.1:0",
        freya_server::handler(served),
        identity.server_config().unwrap(),
    )
    .unwrap();
    let pin = Pin {
        address: server.address(),
        certificate: certificate_of(server.address()).unwrap(),
    };
    assert!(
        verify_server(&pin, "K7Q2M-9XD4T").is_err(),
        "no code shown yet"
    );
    let (code, _) = accounts.connection_code();
    assert!(verify_server(&pin, "K7Q2").is_err(), "too short to be one");
    let typed = freya_protocol::show_code(&code).to_lowercase();
    verify_server(&pin, &typed).unwrap();
    verify_server(&pin, &code).unwrap();
    // A wrong code does not match.
    let wrong = if code.starts_with('0') { "1" } else { "0" }.to_owned() + &code[1..];
    let mismatch = verify_server(&pin, &wrong).unwrap_err();
    assert!(mismatch.contains("does not match"), "{mismatch}");
    // A proof for another certificate does not match the one pinned.
    let (code, _) = accounts.connection_code();
    let nonce = [1; 32];
    let proofs = Http::pinned(pin.clone()).enroll(&nonce).unwrap();
    let other = freya_server::tls::Identity::make().unwrap().certificate;
    let digest = ring::digest::digest(&ring::digest::SHA256, &other);
    let derived = freya_protocol::connection_key(&code, digest.as_ref());
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &derived);
    let message = freya_protocol::connection_message(&nonce, digest.as_ref());
    assert!(
        proofs
            .iter()
            .all(|proof| ring::hmac::verify(&key, &message, proof).is_err())
    );
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_program_in_the_local_servers_place_is_given_nothing() {
    let (_plain, _remote, local, dir) = served("impostor");
    // This user's server keeps its certificate in their folder; another program listens in its
    // place, with a certificate of its own.
    let ours = freya_server::tls::Identity::make().unwrap();
    let theirs = freya_server::tls::Identity::make().unwrap();
    let served = Served {
        library: Arc::clone(&local),
        server: None,
        accounts: None,
        certificate: Some(theirs.certificate.clone()),
        callback_port: freya_protocol::CALLBACK_PORT,
        updates: None,
    };
    let impostor = Server::start_tls(
        "127.0.0.1:0",
        freya_server::handler(served),
        theirs.server_config().unwrap(),
    )
    .unwrap();
    // Freya there pins the certificate in the server's folder: the program is given nothing.
    let pin = Pin {
        address: impostor.address(),
        certificate: ours.certificate.clone(),
    };
    assert!(Http::pinned(pin).version().is_err());
    assert!(!freya_server::tls::answers_as(
        impostor.address(),
        &ours.certificate
    ));
    assert!(freya_server::tls::answers_as(
        impostor.address(),
        &theirs.certificate
    ));
    drop(impostor);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_picture_cache_takes_only_what_a_checksum_names() {
    let dir = std::env::temp_dir().join(format!("freya-picture-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = PictureCache::new(dir.clone());
    let picture = b"a picture".as_slice();
    let checksum = checksum_of(picture);
    // Other bytes under its name are not kept, nor taken from the cache when found there.
    cache.put(&checksum, b"another picture");
    assert_eq!(cache.get(&checksum), None);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(&checksum), b"changed since").unwrap();
    assert_eq!(cache.get(&checksum), None);
    assert!(!dir.join(&checksum).exists(), "and goes");
    cache.put(&checksum, picture);
    assert_eq!(cache.get(&checksum).as_deref(), Some(picture));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The checksum a picture holding `bytes` is kept by.
fn checksum_of(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Passes bytes between a client and `target`, each `delay` late on its way, as a network
/// between two computers would: its address.
fn delayed(target: std::net::SocketAddr, delay: std::time::Duration) -> std::net::SocketAddr {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let pass = move |mut from: TcpStream, mut to: TcpStream| {
        let (send, receive) = std::sync::mpsc::channel::<(std::time::Instant, Vec<u8>)>();
        std::thread::spawn(move || {
            let mut buffer = vec![0; 64 * 1024];
            while let Ok(read @ 1..) = from.read(&mut buffer) {
                let due = std::time::Instant::now() + delay;
                if send.send((due, buffer[..read].to_vec())).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            for (due, bytes) in receive {
                std::thread::sleep(due.saturating_duration_since(std::time::Instant::now()));
                if to.write_all(&bytes).is_err() {
                    break;
                }
            }
            let _ = to.shutdown(std::net::Shutdown::Write);
        });
    };
    std::thread::spawn(move || {
        for client in listener.incoming().flatten() {
            let Ok(server) = TcpStream::connect(target) else {
                continue;
            };
            let _ = client.set_nodelay(true);
            let _ = server.set_nodelay(true);
            pass(client.try_clone().unwrap(), server.try_clone().unwrap());
            pass(server, client);
        }
    });
    address
}

/// Measures the calls a client makes against a real library over TLS, a network's delay
/// between: `FREYA_BENCH_LIBRARY` names a copy of a server's folder, `FREYA_BENCH_DELAY_MS`
/// the delay each way (1 by default).
/// `FREYA_BENCH_LIBRARY=… cargo test --release -p freya-client measure -- --ignored --nocapture`
#[test]
#[ignore = "measures a real library"]
fn measure_a_real_library() {
    use std::time::{Duration, Instant};
    let Some(folder) = std::env::var_os("FREYA_BENCH_LIBRARY").map(PathBuf::from) else {
        return;
    };
    let delay = std::env::var("FREYA_BENCH_DELAY_MS")
        .ok()
        .and_then(|d| d.parse().ok())
        .unwrap_or(1);
    let pictures = || Some(Pictures::new(folder.join("pictures")));
    let open = || Library::open(&folder.join("library.sqlite3")).unwrap();
    let local: Arc<dyn LibraryApi> = Arc::new(LocalLibrary::new(open(), pictures()));
    let identity = freya_server::tls::Identity::make().unwrap();
    // As many connections as a server answers on, unless `FREYA_BENCH_READERS` says.
    let readers = std::env::var("FREYA_BENCH_READERS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(4);
    let served = Served {
        library: Arc::new(
            LocalLibrary::with_connections((0..readers).map(|_| open()).collect(), pictures())
                .watched_by(open()),
        ),
        server: None,
        accounts: None,
        certificate: Some(identity.certificate.clone()),
        callback_port: freya_protocol::CALLBACK_PORT,
        updates: None,
    };
    let server = Server::start_tls(
        "127.0.0.1:0",
        freya_server::handler(served),
        identity.server_config().unwrap(),
    )
    .unwrap();
    let address = delayed(server.address(), Duration::from_millis(delay));
    let cache = std::env::temp_dir().join(format!("freya-bench-{}", std::process::id()));
    let mut http = Http::pinned(Pin {
        address,
        certificate: identity.certificate.clone(),
    });
    http.cache = Some(PictureCache::new(cache.clone()));
    let (audio, agent) = http.audio();
    let agent = Arc::new(agent);
    let remote: Arc<dyn LibraryApi> = Arc::new(Remote(http));

    let tracks = local.all_tracks().unwrap();
    let albums = local.all_albums().unwrap();
    let album = albums.iter().max_by_key(|a| a.tracks).unwrap().clone();
    let artist = album.artist.clone();
    println!(
        "{} tracks, {} albums, {delay} ms each way\n{:<22}{:>10}{:>10}{:>10}{:>11}",
        tracks.len(),
        albums.len(),
        "call",
        "here ms",
        "first ms",
        "again ms",
        "JSON KB"
    );
    let time = |run: &dyn Fn(&dyn LibraryApi) -> usize| {
        let started = Instant::now();
        run(&*local);
        let here = started.elapsed();
        let started = Instant::now();
        let bytes = run(&*remote);
        let first = started.elapsed();
        let started = Instant::now();
        run(&*remote);
        (here, first, started.elapsed(), bytes)
    };
    let size = |value: &dyn erased::Sized| value.json_len();
    macro_rules! measure {
        ($name:expr, |$api:ident| $call:expr) => {{
            let (here, first, again, bytes) = time(&|$api: &dyn LibraryApi| size(&$call.unwrap()));
            println!(
                "{:<22}{:>10.1}{:>10.1}{:>10.1}{:>11.1}",
                $name,
                here.as_secs_f64() * 1000.0,
                first.as_secs_f64() * 1000.0,
                again.as_secs_f64() * 1000.0,
                bytes as f64 / 1024.0
            );
        }};
    }
    measure!("stats", |api| api.stats());
    measure!("artists", |api| api.artists());
    measure!("artists_by", |api| api
        .artists_by(freya_protocol::ArtistOrder::Name));
    measure!("all_albums", |api| api.all_albums());
    measure!("albums_by", |api| api.albums_by(AlbumOrder::Name));
    measure!("all_tracks", |api| api.all_tracks());
    measure!("album_covers", |api| api.album_covers());
    measure!("artist_edits", |api| api.artist_edits());
    measure!("genres", |api| api.genres());
    measure!("genre_infos", |api| api.genre_infos());
    measure!("recently_added", |api| api.recently_added(50));
    measure!("recently_played", |api| api.recently_played(50));
    measure!("most_played", |api| api.most_played(50));
    measure!("search", |api| api.search("the", 50));
    measure!("album_tracks", |api| api
        .album_tracks(artist.as_deref(), album.name.as_deref()));
    measure!("artist_tracks", |api| api.artist_tracks(artist.as_deref()));
    measure!("artist_summary", |api| api
        .artist_summary(artist.as_deref(), 3));
    measure!("album_favorites", |api| api.album_favorites());
    measure!("artist_favorites", |api| api.artist_favorites());

    // A page: an artist's, as the window reads it, one call after another or asked at once;
    // and a playlist's 200 tracks, looked up.
    let name = artist.clone().unwrap_or_default();
    let page = |remote: &dyn LibraryApi| {
        let artist = Some(name.as_str());
        remote.albums(artist).unwrap();
        remote.artist_summary(artist, 3).unwrap();
        remote.artist_popular(artist, 10).unwrap();
        remote.artist_mbid(artist).unwrap();
        remote.searched_artist_id(&name).unwrap();
        remote.artist_article(&name).unwrap();
        remote.artist_text(&name).unwrap();
    };
    let artist_page_calls = || {
        let artist = Some(name.as_str());
        vec![
            ("albums", freya_protocol::args(&(artist,))),
            ("artist_summary", freya_protocol::args(&(artist, 3usize))),
            ("artist_popular", freya_protocol::args(&(artist, 10usize))),
            ("artist_mbid", freya_protocol::args(&(artist,))),
            (
                "searched_artist_id",
                freya_protocol::args(&(name.as_str(),)),
            ),
            ("artist_article", freya_protocol::args(&(name.as_str(),))),
            ("artist_text", freya_protocol::args(&(name.as_str(),))),
        ]
    };
    let started = Instant::now();
    page(&*remote);
    let one_by_one = started.elapsed();
    let started = Instant::now();
    remote.prefetch(&artist_page_calls());
    page(&*remote);
    println!(
        "an artist's page: {:.1} ms a call at a time, {:.1} ms asked at once",
        one_by_one.as_secs_f64() * 1000.0,
        started.elapsed().as_secs_f64() * 1000.0
    );
    let listed: Vec<_> = tracks
        .iter()
        .step_by(tracks.len() / 200)
        .take(200)
        .collect();
    let started = Instant::now();
    for track in &listed {
        remote.track_at(&track.path, Duration::ZERO).unwrap();
    }
    let one_by_one = started.elapsed();
    let started = Instant::now();
    let each: Vec<_> = listed
        .iter()
        .map(|t| {
            (
                "track_at",
                freya_protocol::args(&(&t.path, Duration::from_millis(1))),
            )
        })
        .collect();
    remote.prefetch(&each);
    for track in &listed {
        remote
            .track_at(&track.path, Duration::from_millis(1))
            .unwrap();
    }
    println!(
        "a playlist's 200 tracks: {:.1} ms a call at a time, {:.1} ms asked at once",
        one_by_one.as_secs_f64() * 1000.0,
        started.elapsed().as_secs_f64() * 1000.0
    );

    // A small call while another client asks for the whole library, again and again.
    let busy = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let other = {
        let (busy, remote) = (Arc::clone(&busy), Arc::clone(&remote));
        std::thread::spawn(move || {
            while busy.load(std::sync::atomic::Ordering::Relaxed) {
                remote.all_tracks().unwrap();
            }
        })
    };
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    for _ in 0..20 {
        remote.stats().unwrap();
    }
    println!(
        "stats while another asks for all tracks: {:.1} ms",
        started.elapsed().as_secs_f64() * 1000.0 / 20.0
    );
    busy.store(false, std::sync::atomic::Ordering::Relaxed);
    other.join().unwrap();

    // Covers: the first albums of a grid, each its own request.
    let covers = local.album_covers().unwrap();
    let checksums: Vec<String> = covers
        .values()
        .filter_map(|c| c.cover.clone())
        .take(60)
        .collect();
    let started = Instant::now();
    let mut bytes = 0;
    for checksum in &checksums {
        bytes += remote.picture(checksum).unwrap().map_or(0, |p| p.len());
    }
    println!(
        "{} covers: {:.1} ms, {:.1} KB, from the cache {:.1} ms",
        checksums.len(),
        started.elapsed().as_secs_f64() * 1000.0,
        bytes as f64 / 1024.0,
        {
            let started = Instant::now();
            for checksum in &checksums {
                remote.picture(checksum).unwrap();
            }
            started.elapsed().as_secs_f64() * 1000.0
        }
    );
    let _ = std::fs::remove_dir_all(&cache);
    // As the grid has them: small, on four threads, sixteen asked at once.
    let bytes = std::sync::atomic::AtomicUsize::new(0);
    let started = Instant::now();
    std::thread::scope(|threads| {
        for some in checksums.chunks(16) {
            let (remote, bytes) = (&remote, &bytes);
            threads.spawn(move || {
                for small in remote.small_pictures(some).unwrap().into_iter().flatten() {
                    bytes.fetch_add(small.len(), std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
    });
    println!(
        "{} covers small, sixteen at once on four threads: {:.1} ms, {:.1} KB",
        checksums.len(),
        started.elapsed().as_secs_f64() * 1000.0,
        bytes.into_inner() as f64 / 1024.0
    );

    // Sound: the first of it, after a seek, and between two tracks, for a few files of each
    // kind, each of another album, so none is read warm.
    let player = freya_audio::Player::silent(48_000);
    let media = |track: &freya_protocol::Track, start: Duration| freya_audio::Media {
        start,
        remote: Some(freya_audio::RemoteSource {
            url: audio_url_at(&audio, &track.path),
            bearer: None,
            agent: Some(Arc::clone(&agent)),
        }),
        ..freya_audio::Media::from(&track.path)
    };
    let started = |player: &freya_audio::Player| loop {
        match player.events().recv_timeout(Duration::from_secs(30)) {
            Ok(freya_audio::Event::TrackStarted(_)) => break,
            Ok(_) => {}
            Err(e) => panic!("no sound: {e}"),
        }
    };
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!(
        "{:<6}{:>12}{:>14}{:>12}",
        "file", "first ms", "seek silent", "gap ms"
    );
    for kind in ["mp3", "flac", "m4a"] {
        let mut albums_seen = std::collections::HashSet::new();
        let picked: Vec<_> = tracks
            .iter()
            .filter(|t| {
                t.part.is_none()
                    && t.duration.is_some_and(|d| d > Duration::from_secs(60))
                    && t.path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case(kind))
                    && t.path.exists()
                    && albums_seen.insert(t.album.clone())
            })
            .take(4)
            .collect();
        // Read once, so what is measured is the way between client and server, not the disk.
        for track in &picked {
            let _ = std::fs::read(&track.path);
        }
        for pair in picked.chunks(2) {
            let [first, second] = pair else { break };
            let duration = first.duration.unwrap();
            let asked = Instant::now();
            player.play(media(first, Duration::ZERO));
            started(&player);
            let first_sound = asked.elapsed();
            std::thread::sleep(Duration::from_millis(500));
            let before = player.starved();
            let target = duration.mul_f64(0.7);
            player.seek(target);
            let waited = Instant::now();
            while player.status().position < target + Duration::from_millis(500)
                && waited.elapsed() < Duration::from_secs(30)
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            let seek_silent = player.starved() - before;
            // The last seconds of one, the next queued after it; measured from a second in,
            // after what starting it took.
            player.play(media(first, duration - Duration::from_secs(3)));
            started(&player);
            player.set_next(Some(media(second, Duration::ZERO)));
            std::thread::sleep(Duration::from_secs(1));
            let before = player.starved();
            started(&player);
            std::thread::sleep(Duration::from_millis(500));
            let gap = player.starved() - before;
            println!(
                "{kind:<6}{:>12.1}{:>14.1}{:>12.1}",
                ms(first_sound),
                ms(seek_silent),
                ms(gap)
            );
        }
    }
    player.stop();
}

/// The length of a value as JSON, for the measurements.
mod erased {
    pub trait Sized {
        fn json_len(&self) -> usize;
    }
    impl<T: serde::Serialize> Sized for T {
        fn json_len(&self) -> usize {
            serde_json::to_vec(self).map_or(0, |v| v.len())
        }
    }
}

#[test]
fn only_a_server_of_freyas_own_version_is_used() {
    assert!(same_version(env!("CARGO_PKG_VERSION")).is_ok());
    let older = same_version("0.0.1").unwrap_err();
    assert!(older.contains("Update Freya Server"), "{older}");
    let newer = same_version("999.0.0").unwrap_err();
    assert!(newer.contains("Update Freya to"), "{newer}");
}
