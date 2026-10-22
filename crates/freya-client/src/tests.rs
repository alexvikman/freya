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
        certificate: None,
        callback_port: freya_protocol::CALLBACK_PORT,
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
        certificate: None,
        callback_port: freya_protocol::CALLBACK_PORT,
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
        accounts: Some(Arc::clone(&accounts)),
        certificate: None,
        callback_port: freya_protocol::CALLBACK_PORT,
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
    let token = Http::new(&address).pair(&key, "Mac").unwrap().access;
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

    // A device with only its refresh token renews, keeping the new one; one whose access token
    // the server does not take renews and asks again. A refresh token used up, coming again, was
    // copied: the device is logged out.
    let store = TokenStore::new(dir.join("servers.json"), false);
    let key = std::fs::read_to_string(&pairing_key).unwrap();
    let tablet = Http::new(&address).pair(&key, "Tablet").unwrap();
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

    // Another device logs in in the browser once the user has a password, set on the page a
    // ticket opens: without the password now, from the server's own computer.
    let user = mac.0.account().unwrap().unwrap().user;
    let pages = Http::new(&address).pages();
    let login = Login::new("http://127.0.0.1:47477/callback");
    assert_eq!(log_in(&pages, &login, &user, "correct horse").status, 401);
    let ticket = mac.0.password_ticket().unwrap();
    let page = format!("/web/password?ticket={ticket}");
    let shown = pages.ask("GET", &page, None, Vec::new()).unwrap();
    assert_eq!(shown.status, 200);
    assert!(!String::from_utf8_lossy(&shown.body).contains(r#"name="current""#));
    let changed = pages
        .ask(
            "POST",
            "/web/password",
            Some(FORM),
            format!("ticket={ticket}&password=correct+horse&again=correct+horse").into_bytes(),
        )
        .unwrap();
    assert_eq!(changed.status, 200);

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
    assert!(phone.most_played(10).is_err());
    assert!(phone.playlists().is_err());
    assert!(phone.save_playlist("Kim's", &[]).is_err());
    let refused = phone.settings().unwrap_err();
    assert!(refused.0.contains("admin"), "{refused:?}");
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
        quit: None,
        accounts: None,
        certificate: Some(identity.certificate.clone()),
        callback_port: freya_protocol::CALLBACK_PORT,
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
    let (base, agent) = remote.0.audio(freya_audio::REMOTE_TIMEOUT);
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

#[test]
fn a_new_device_knows_the_server_by_its_connection_code() {
    let (_plain, _remote, local, dir) = served("code");
    let identity = freya_server::tls::Identity::make().unwrap();
    let accounts = Arc::new(freya_server::accounts::Accounts::open(None, None));
    let served = Served {
        library: Arc::clone(&local),
        server: None,
        quit: None,
        accounts: Some(Arc::clone(&accounts)),
        certificate: Some(identity.certificate.clone()),
        callback_port: freya_protocol::CALLBACK_PORT,
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
    let proof = Http::pinned(pin.clone()).enroll(&nonce).unwrap();
    let other = freya_server::tls::Identity::make().unwrap().certificate;
    let digest = ring::digest::digest(&ring::digest::SHA256, &other);
    let derived = freya_protocol::connection_key(&code, digest.as_ref());
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &derived);
    let message = freya_protocol::connection_message(&nonce, digest.as_ref());
    assert!(ring::hmac::verify(&key, &message, &proof).is_err());
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_program_in_the_local_servers_place_is_given_nothing() {
    let (_plain, _remote, local, dir) = served("impostor");
    // This user's server keeps its certificate in their folder; another program listens in its
    // place, with a certificate of its own.
    let ours = freya_server::tls::Identity::make().unwrap();
    let certificate = dir.join("tls-certificate.der");
    std::fs::write(&certificate, &ours.certificate).unwrap();
    let pairing_key = dir.join("pairing.key");
    std::fs::write(&pairing_key, "the key").unwrap();
    let theirs = freya_server::tls::Identity::make().unwrap();
    let served = Served {
        library: Arc::clone(&local),
        server: None,
        quit: None,
        accounts: None,
        certificate: Some(theirs.certificate.clone()),
        callback_port: freya_protocol::CALLBACK_PORT,
    };
    let impostor = Server::start_tls(
        "127.0.0.1:0",
        freya_server::handler(served),
        theirs.server_config().unwrap(),
    )
    .unwrap();
    let store = TokenStore::new(dir.join("servers.json"), false);
    store.set(&format!("http://{}", impostor.address()), "a refresh token");
    let refused = connect_locally(
        impostor.address(),
        &dir.join("no such program"),
        &pairing_key,
        &certificate,
        &store,
        "Mac",
    )
    .err()
    .unwrap();
    assert!(refused.contains("Another program answers"), "{refused}");
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
