use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use freya_library::Library;
use freya_library::pictures::Pictures;
use freya_protocol::{AlbumOrder, ArtistEdit, LibraryApi};
use freya_server::LocalLibrary;
use freya_server::http::Server;

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
    let server = Server::start(
        "127.0.0.1:0",
        freya_server::handler(Arc::new(LocalLibrary::new(
            Library::open(&path).unwrap(),
            pictures(),
        ))),
    )
    .unwrap();
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
