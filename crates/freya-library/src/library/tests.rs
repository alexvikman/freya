use std::sync::atomic::AtomicBool;
use std::time::Instant;

use super::*;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../freya-audio/tests/fixtures")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("freya-library-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn scan_now(library: &mut Library, folders: &[PathBuf]) -> ScanSummary {
    scan(library, folders, &AtomicBool::new(false), |_| {}).unwrap()
}

fn tags(artist: &str, album: &str, title: &str, track: u32) -> TagInfo {
    TagInfo {
        title: Some(title.into()),
        artist: Some(artist.into()),
        album: Some(album.into()),
        track: Some(track),
        duration: Some(Duration::from_secs(180)),
        ..Default::default()
    }
}

const STAMP: FileStamp = FileStamp {
    size: 1,
    modified: 1,
};

fn whole_file(path: &str, tags: &TagInfo) -> NewTrack {
    NewTrack {
        path: path.into(),
        part: None,
        tags: tags.clone(),
        fingerprint: None,
    }
}

fn add(library: &mut Library, path: &str, tags: &TagInfo) {
    add_at(library, path, tags, 0);
}

/// Adds a track that came into the library at `added`.
fn add_at(library: &mut Library, path: &str, tags: &TagInfo, added: i64) {
    let tx = library.transaction().unwrap();
    put(&tx, Path::new(path), STAMP, added, &whole_file(path, tags)).unwrap();
    tx.commit().unwrap();
}

/// A time `seconds` after 1970.
fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
}

fn added_of(library: &Library, path: &str) -> i64 {
    library
        .db
        .query_row("SELECT added FROM tracks WHERE path = ?1", [path], |row| {
            row.get(0)
        })
        .unwrap()
}

fn titles(tracks: &[Track]) -> Vec<String> {
    tracks.iter().map(Track::display_title).collect()
}

#[test]
fn scans_add_update_and_remove_files() {
    let music = temp_dir("scan");
    for name in ["sine.mp3", "sine.flac", "sine.wav"] {
        std::fs::copy(fixtures().join(name), music.join(name)).unwrap();
    }
    // Not audio, and hidden: both ignored.
    std::fs::write(music.join("notes.txt"), "x").unwrap();
    std::fs::write(music.join("._sine.mp3"), "x").unwrap();
    let mut library = Library::open(&music.join("library.sqlite3")).unwrap();
    let folders = [music.clone()];

    let first = scan_now(&mut library, &folders);
    assert_eq!((first.added, first.updated, first.removed), (3, 0, 0));
    let mp3 = library.search("sine test", 10).unwrap();
    assert_eq!(titles(&mp3), ["Sine Test"]);
    assert_eq!(mp3[0].artist.as_deref(), Some("Freya"));
    assert!(mp3[0].duration.is_some());

    let again = scan_now(&mut library, &folders);
    assert_eq!((again.added, again.updated, again.unchanged), (0, 0, 3));
    // Found on the library's first scan, so dated by the file.
    let flac = music.join("sine.flac");
    let metadata = std::fs::metadata(&flac).unwrap();
    let file_time = metadata.created().or_else(|_| metadata.modified()).unwrap();
    let flac = flac.to_str().unwrap();
    let first_added = added_of(&library, flac);
    assert_eq!(first_added, epoch_millis(file_time));

    std::fs::copy(fixtures().join("sine.ogg"), music.join("sine.flac")).unwrap();
    std::fs::remove_file(music.join("sine.wav")).unwrap();
    let changed = scan_now(&mut library, &folders);
    assert_eq!(
        (
            changed.added,
            changed.updated,
            changed.removed,
            changed.unchanged
        ),
        (0, 1, 1, 1)
    );
    assert_eq!(library.stats().unwrap().tracks, 2);
    assert_eq!(
        added_of(&library, flac),
        first_added,
        "a changed file keeps when it came"
    );
    // Closed first: Windows does not delete a database that is open.
    drop(library);
    std::fs::remove_dir_all(&music).unwrap();
}

/// A sheet that cuts `sine.wav`, one second long, into three tracks.
fn write_sheet(path: &Path, second_title: &str) {
    let sheet = format!(
        "PERFORMER \"Freya Ensemble\"\nTITLE \"Test Disc\"\nREM DATE 2001\n\
         FILE \"Album.wav\" WAVE\n\
         TRACK 01 AUDIO\n TITLE \"One\"\n INDEX 01 00:00:00\n\
         TRACK 02 AUDIO\n TITLE \"{second_title}\"\n PERFORMER \"Guest\"\n INDEX 01 00:00:30\n\
         TRACK 03 AUDIO\n TITLE \"Three\"\n INDEX 01 00:00:60\n"
    );
    std::fs::write(path, sheet).unwrap();
}

#[test]
fn a_cue_sheet_lists_its_file_as_tracks() {
    let music = temp_dir("cue");
    std::fs::copy(fixtures().join("sine.wav"), music.join("Album.wav")).unwrap();
    std::fs::copy(fixtures().join("sine.mp3"), music.join("sine.mp3")).unwrap();
    let sheet = music.join("Album.cue");
    write_sheet(&sheet, "Two");
    let mut library = Library::open(&music.join("library.sqlite3")).unwrap();
    let folders = [music.clone()];

    let first = scan_now(&mut library, &folders);
    assert_eq!((first.added, first.updated, first.removed), (2, 0, 0));
    assert_eq!(library.stats().unwrap().tracks, 4);
    let disc = library
        .album_tracks(Some("Freya Ensemble"), Some("Test Disc"))
        .unwrap();
    assert_eq!(titles(&disc), ["One", "Two", "Three"]);
    assert_eq!(disc[1].artist.as_deref(), Some("Guest"));
    assert_eq!((disc[1].track, disc[1].year), (Some(2), Some(2001)));
    assert!(disc.iter().all(|t| t.path == music.join("Album.wav")));
    let ms = Duration::from_millis;
    assert_eq!(
        disc[1].part,
        Some(Part {
            start: ms(400),
            end: Some(ms(800))
        })
    );
    assert_eq!(disc[1].duration, Some(ms(400)));
    // The last track runs to the end of the file, which is a second long.
    assert_eq!(disc[2].part.unwrap().end, None);
    let last = disc[2].duration.unwrap().as_secs_f64();
    assert!((last - 0.2).abs() < 0.01, "{last}");
    // Found by what the sheet says, not by the file's name.
    assert_eq!(titles(&library.search("guest", 10).unwrap()), ["Two"]);
    assert!(library.search("album", 10).unwrap().is_empty());

    let again = scan_now(&mut library, &folders);
    assert_eq!((again.added, again.updated, again.unchanged), (0, 0, 2));

    // Editing the sheet reads the disc again.
    write_sheet(&sheet, "Second");
    let later = std::time::SystemTime::now() + Duration::from_secs(10);
    std::fs::File::options()
        .write(true)
        .open(&sheet)
        .and_then(|f| f.set_modified(later))
        .unwrap();
    let edited = scan_now(&mut library, &folders);
    assert_eq!((edited.added, edited.updated, edited.unchanged), (0, 1, 1));
    assert_eq!(library.stats().unwrap().tracks, 4);
    assert_eq!(titles(&library.search("guest", 10).unwrap()), ["Second"]);

    // Without its sheet, the file is a single track again.
    std::fs::remove_file(&sheet).unwrap();
    let removed = scan_now(&mut library, &folders);
    assert_eq!((removed.added, removed.removed), (1, 1));
    assert_eq!(library.stats().unwrap().tracks, 2);
    assert_eq!(titles(&library.search("album", 10).unwrap()), ["Album"]);
    // Closed first: Windows does not delete a database that is open.
    drop(library);
    std::fs::remove_dir_all(&music).unwrap();
}

#[test]
fn files_in_missing_folders_are_kept_and_others_dropped() {
    let mut library = Library::open_in_memory().unwrap();
    add(&mut library, "/unplugged/drive/a.mp3", &TagInfo::default());
    add(
        &mut library,
        "/no/longer/a/folder/b.mp3",
        &TagInfo::default(),
    );
    let summary = scan_now(&mut library, &[PathBuf::from("/unplugged/drive")]);
    assert_eq!(summary.removed, 1);
    let sources = library.sources().unwrap();
    assert!(sources.contains_key(Path::new("/unplugged/drive/a.mp3")));
}

#[test]
fn files_in_empty_or_unreadable_folders_are_kept() {
    // A mount point with nothing mounted on it is an empty folder: its tracks are kept, as for
    // an unplugged drive, rather than all removed and read again later as new.
    let mount = std::env::temp_dir().join(format!("freya-mount-{}", std::process::id()));
    std::fs::create_dir_all(&mount).unwrap();
    let mut library = Library::open_in_memory().unwrap();
    let track = mount.join("a.mp3");
    add(&mut library, track.to_str().unwrap(), &TagInfo::default());
    let summary = scan_now(&mut library, std::slice::from_ref(&mount));
    assert_eq!(summary.removed, 0);
    assert!(library.sources().unwrap().contains_key(track.as_path()));
    std::fs::remove_dir_all(&mount).unwrap();
}

#[test]
fn browses_by_artist_and_album() {
    let mut library = Library::open_in_memory().unwrap();
    add(
        &mut library,
        "/m/b/2.mp3",
        &tags("Björk", "Post", "Hyperballad", 2),
    );
    add(
        &mut library,
        "/m/b/1.mp3",
        &tags("Björk", "Post", "Army of Me", 1),
    );
    let mut debut = tags("Björk", "Debut", "Human Behaviour", 1);
    debut.year = Some(1993);
    add(&mut library, "/m/b/d1.mp3", &debut);
    // Filed under the album artist, not the track artist.
    let mut feature = tags("Guest", "Post", "Possibly Maybe", 3);
    feature.album_artist = Some("Björk".into());
    add(&mut library, "/m/b/3.mp3", &feature);
    add(
        &mut library,
        "/m/a/1.mp3",
        &tags("ABBA", "Arrival", "Dancing Queen", 1),
    );
    add(&mut library, "/m/untagged.mp3", &TagInfo::default());

    let artists = library.artists().unwrap();
    let names: Vec<Option<&str>> = artists.iter().map(|a| a.name.as_deref()).collect();
    assert_eq!(names, [Some("ABBA"), Some("Björk"), None]);
    assert_eq!((artists[1].albums, artists[1].tracks), (2, 4));

    let albums = library.albums(Some("Björk")).unwrap();
    let names: Vec<Option<&str>> = albums.iter().map(|a| a.name.as_deref()).collect();
    assert_eq!(names, [Some("Debut"), Some("Post")]);

    let post = library.album_tracks(Some("Björk"), Some("Post")).unwrap();
    assert_eq!(
        titles(&post),
        ["Army of Me", "Hyperballad", "Possibly Maybe"]
    );
    assert_eq!(library.artist_tracks(Some("Björk")).unwrap().len(), 4);

    let untagged = library.album_tracks(None, None).unwrap();
    assert_eq!(titles(&untagged), ["untagged"]);
    assert_eq!(
        library.stats().unwrap(),
        Stats {
            tracks: 6,
            albums: 3,
            artists: 2
        }
    );
}

#[test]
fn browses_all_albums_genres_and_songs() {
    let mut library = Library::open_in_memory().unwrap();
    let with_genre = |artist: &str, album: &str, title: &str, genre: Option<&str>| TagInfo {
        genre: genre.map(str::to_owned),
        ..tags(artist, album, title, 1)
    };
    add(
        &mut library,
        "/m/1.mp3",
        &with_genre("Björk", "Post", "Hyperballad", Some("Electronic")),
    );
    add(
        &mut library,
        "/m/2.mp3",
        &with_genre("Björk", "Post", "Army of Me", Some("Electronic")),
    );
    add(
        &mut library,
        "/m/3.mp3",
        &with_genre("ABBA", "Arrival", "Dancing Queen", Some("pop")),
    );
    add(
        &mut library,
        "/m/4.mp3",
        &with_genre(
            "Air",
            "Moon Safari",
            "La femme d'argent",
            Some("Electronic"),
        ),
    );
    add(&mut library, "/m/untagged.mp3", &TagInfo::default());

    let albums = library.all_albums().unwrap();
    let names: Vec<Option<&str>> = albums.iter().map(|a| a.name.as_deref()).collect();
    assert_eq!(
        names,
        [Some("Arrival"), Some("Moon Safari"), Some("Post"), None]
    );
    assert_eq!(albums[2].artist.as_deref(), Some("Björk"));
    assert_eq!(albums[2].tracks, 2);

    let genres = library.genres().unwrap();
    let names: Vec<Option<&str>> = genres.iter().map(|g| g.name.as_deref()).collect();
    assert_eq!(names, [Some("Electronic"), Some("pop"), None]);
    assert_eq!((genres[0].albums, genres[0].tracks), (2, 3));
    assert_eq!((genres[2].albums, genres[2].tracks), (1, 1));

    let electronic = library.genre_albums(Some("Electronic")).unwrap();
    let names: Vec<Option<&str>> = electronic.iter().map(|a| a.name.as_deref()).collect();
    assert_eq!(names, [Some("Moon Safari"), Some("Post")]);
    assert_eq!(
        titles(&library.genre_tracks(Some("Electronic")).unwrap()),
        ["La femme d'argent", "Hyperballad", "Army of Me"]
    );

    assert_eq!(
        titles(&library.all_tracks().unwrap()),
        [
            "Army of Me",
            "Dancing Queen",
            "Hyperballad",
            "La femme d'argent",
            "untagged"
        ]
    );
}

#[test]
fn search_matches_word_starts_and_ignores_accents_and_syntax() {
    let mut library = Library::open_in_memory().unwrap();
    add(
        &mut library,
        "/m/1.mp3",
        &tags("Beyoncé", "Lemonade", "Formation", 1),
    );
    add(
        &mut library,
        "/m/2.mp3",
        &tags("The Beatles", "Abbey Road", "Something", 1),
    );
    add(
        &mut library,
        "/m/3.mp3",
        &tags("Glove", "Blue", "Like a Ribbon", 1),
    );
    add(&mut library, "/m/live-at-home.flac", &TagInfo::default());

    let found = |q: &str| titles(&library.search(q, 100).unwrap());
    assert_eq!(found("beyonce"), ["Formation"]);
    assert_eq!(found("BEA"), ["Something"]);
    assert_eq!(found("beatles some"), ["Something"]);
    assert_eq!(found("beatles formation"), Vec::<String>::new());
    // Word starts only: "love" does not find "Glove".
    assert_eq!(found("love"), Vec::<String>::new());
    // File names are searched too.
    assert_eq!(found("home"), ["live-at-home"]);
    // Query syntax is taken literally instead of failing.
    for q in ["\"", "a*", "NOT", "(", "abbey -road", "col:umn", "^"] {
        assert!(library.search(q, 100).is_ok(), "{q}");
    }
    assert_eq!(found("   "), Vec::<String>::new());
}

#[test]
fn lists_albums_by_when_they_came_and_tracks_by_plays() {
    let mut library = Library::open_in_memory().unwrap();
    add_at(
        &mut library,
        "/m/a1.mp3",
        &tags("A", "Old", "One", 1),
        1_000,
    );
    add_at(
        &mut library,
        "/m/a2.mp3",
        &tags("A", "Old", "Two", 2),
        1_000,
    );
    add_at(
        &mut library,
        "/m/b1.mp3",
        &tags("B", "New", "Three", 1),
        3_000,
    );
    add_at(
        &mut library,
        "/m/c1.mp3",
        &tags("C", "Middle", "Four", 1),
        2_000,
    );
    let names = |albums: Vec<Album>| -> Vec<String> {
        albums.into_iter().map(|a| a.name.unwrap()).collect()
    };
    assert_eq!(
        names(library.recently_added(10).unwrap()),
        ["New", "Middle", "Old"]
    );
    assert_eq!(names(library.recently_added(1).unwrap()), ["New"]);

    assert!(library.recently_played(10).unwrap().is_empty());
    let play = |path: &str, seconds: u64| {
        library
            .record_play(Path::new(path), None, at(seconds))
            .unwrap();
    };
    play("/m/a1.mp3", 10);
    play("/m/b1.mp3", 20);
    play("/m/a1.mp3", 30);
    play("/m/c1.mp3", 40);
    // Not in the library: kept, but not listed.
    play("/elsewhere/x.mp3", 50);

    let titles_of = |played: &[Played]| -> Vec<String> {
        played.iter().map(|p| p.track.display_title()).collect()
    };
    let recent = library.recently_played(10).unwrap();
    assert_eq!(titles_of(&recent), ["Four", "One", "Three"]);
    assert_eq!(recent[1].plays, 2);
    assert_eq!(recent[1].last, at(30));
    let most = library.most_played(2).unwrap();
    assert_eq!(titles_of(&most), ["One", "Four"]);

    // Artists by how often their music was played, and by what came last.
    let names = |artists: Vec<Artist>| -> Vec<String> {
        artists.into_iter().filter_map(|a| a.name).collect()
    };
    assert_eq!(
        names(library.artists_by(ArtistOrder::Name).unwrap()),
        ["A", "B", "C"]
    );
    assert_eq!(
        names(library.artists_by(ArtistOrder::MostPlayed).unwrap()),
        ["A", "B", "C"]
    );
    library
        .record_play(Path::new("/m/c1.mp3"), None, at(60))
        .unwrap();
    library
        .record_play(Path::new("/m/c1.mp3"), None, at(70))
        .unwrap();
    assert_eq!(
        names(library.artists_by(ArtistOrder::MostPlayed).unwrap()),
        ["C", "A", "B"]
    );
    // An artist's page: their albums, tracks and genres, and their most played tracks.
    let summary = library.artist_summary(Some("A"), 3).unwrap();
    assert_eq!((summary.albums, summary.tracks), (1, 2));
    let popular = library.artist_popular(Some("A"), 5).unwrap();
    assert_eq!(titles_of(&popular), ["One"], "only what was played");
    assert_eq!(popular[0].plays, 2);
    assert!(
        library
            .artist_popular(Some("Nobody"), 5)
            .unwrap()
            .is_empty()
    );
    // B's "New" came last, then C's "Middle".
    assert_eq!(
        names(library.artists_by(ArtistOrder::RecentlyAdded).unwrap()),
        ["B", "C", "A"]
    );
}

#[test]
fn plays_of_a_cue_sheet_track_count_for_that_track_only() {
    let mut library = Library::open_in_memory().unwrap();
    let tx = library.transaction().unwrap();
    for (n, start) in [(1, 0), (2, 300)] {
        let track = NewTrack {
            path: "/m/disc.flac".into(),
            part: Some(Part {
                start: Duration::from_secs(start),
                end: None,
            }),
            tags: tags("A", "Live", &format!("Part {n}"), n),
            fingerprint: None,
        };
        put(&tx, Path::new("/m/disc.cue"), STAMP, 0, &track).unwrap();
    }
    tx.commit().unwrap();
    let second = Part {
        start: Duration::from_secs(300),
        end: None,
    };
    library
        .record_play(Path::new("/m/disc.flac"), Some(second), at(1))
        .unwrap();
    let played = library.recently_played(10).unwrap();
    assert_eq!(played.len(), 1);
    assert_eq!(played[0].track.display_title(), "Part 2");

    // Counted for the album, by where in the file the track starts.
    for _ in 0..2 {
        library
            .record_play(Path::new("/m/disc.flac"), Some(second), at(2))
            .unwrap();
    }
    let plays = library.album_plays(Some("A"), Some("Live")).unwrap();
    assert_eq!(plays.len(), 1);
    assert_eq!(
        plays[&(
            PathBuf::from("/m/disc.flac"),
            Some(Duration::from_secs(300))
        )],
        3
    );
    assert!(
        library
            .album_plays(Some("A"), Some("Other"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn rebuilds_a_library_from_an_older_layout() {
    let dir = temp_dir("schema");
    let path = dir.join("library.sqlite3");
    {
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE tracks (path TEXT); PRAGMA user_version = 0;")
            .unwrap();
    }
    let mut library = Library::open(&path).unwrap();
    add(&mut library, "/m/1.mp3", &tags("A", "B", "C", 1));
    assert_eq!(library.stats().unwrap().tracks, 1);
    library
        .record_play(Path::new("/m/1.mp3"), None, at(1))
        .unwrap();

    // A rebuild drops the tracks, which a scan reads again, but never the plays.
    library.db.pragma_update(None, "user_version", 2).unwrap();
    drop(library);
    let mut library = Library::open(&path).unwrap();
    assert_eq!(library.stats().unwrap().tracks, 0);
    add(&mut library, "/m/1.mp3", &tags("A", "B", "C", 1));
    assert_eq!(library.most_played(10).unwrap()[0].plays, 1);
    // Closed first: Windows does not delete a database that is open.
    drop(library);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn adds_artist_ids_to_a_library_and_reads_every_file_again() {
    let dir = temp_dir("mbid");
    let path = dir.join("library.sqlite3");
    let mut library = Library::open(&path).unwrap();
    add_at(&mut library, "/m/1.mp3", &tags("A", "B", "C", 1), 5);
    library
        .db
        .execute_batch(
            "DROP INDEX tracks_by_fingerprint;
             ALTER TABLE tracks DROP COLUMN fingerprint;
             ALTER TABLE tracks DROP COLUMN artist_mbid;
             PRAGMA user_version = 3;",
        )
        .unwrap();
    drop(library);

    // Both the ids and, after them, the files' fingerprints.
    let mut library = Library::open(&path).unwrap();
    let fingerprint: Option<String> = library
        .db
        .query_row("SELECT fingerprint FROM tracks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(fingerprint, None, "taken at the next scan");
    // Changed as far as a scan can tell, so it reads the tags again.
    let stamp = library.sources().unwrap()[Path::new("/m/1.mp3")];
    assert_eq!(stamp.modified, -1);
    assert_eq!(library.artist_mbid(Some("A")).unwrap(), None);
    // And came in when it did.
    let added: i64 = library
        .db
        .query_row("SELECT added FROM tracks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(added, 5);

    let with_id = |id: &str| TagInfo {
        artist_mbid: Some(id.into()),
        ..tags("A", "B", "C", 1)
    };
    add(&mut library, "/m/2.mp3", &with_id("one"));
    add(&mut library, "/m/3.mp3", &with_id("two"));
    add(&mut library, "/m/4.mp3", &with_id("two"));
    assert_eq!(
        library.artist_mbid(Some("A")).unwrap().as_deref(),
        Some("two")
    );
    assert_eq!(library.artist_mbid(Some("Z")).unwrap(), None);
    drop(library);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The goal for M12: a library of 10 000+ tracks searches in under 50 ms.
#[test]
fn ten_thousand_tracks_browse_and_search_quickly() {
    const WORDS: [&str; 16] = [
        "love", "night", "blue", "river", "fire", "dream", "heart", "rain", "gold", "sky", "stone",
        "light", "road", "home", "ocean", "summer",
    ];
    let mut library = Library::open_in_memory().unwrap();
    let tx = library.transaction().unwrap();
    for i in 0..12_000u32 {
        let word = |n: u32| WORDS[(n as usize) % WORDS.len()];
        let artist = format!("Artist {} {}", i / 120, word(i / 120));
        let album = format!("{} {} {}", word(i / 12), word(i / 7), i / 12);
        let title = format!("{} {} {}", word(i), word(i / 3), word(i * 7));
        let path = format!("/music/{artist}/{album}/{i:05} {title}.flac");
        let track = whole_file(&path, &tags(&artist, &album, &title, i % 12));
        put(&tx, Path::new(&path), STAMP, i64::from(i), &track).unwrap();
    }
    tx.commit().unwrap();

    let time = |what: &str, f: &mut dyn FnMut() -> usize| {
        f(); // Warm up the statement cache.
        let started = Instant::now();
        let found = f();
        let took = started.elapsed();
        eprintln!("{what}: {found} rows in {took:?}");
        // Every track is every row made into a track, in a debug build on a busy machine.
        let budget = if what == "all tracks" { 150 } else { 50 };
        assert!(took < Duration::from_millis(budget), "{what} took {took:?}");
        found
    };
    assert_eq!(
        time("artists", &mut || library.artists().unwrap().len()),
        100
    );
    let albums = time("albums", &mut || {
        library.albums(Some("Artist 7 rain")).unwrap().len()
    });
    assert!(albums > 0);
    // One letter matches nearly everything: the worst case while typing.
    assert_eq!(
        time("search 'l'", &mut || library
            .search("l", 1000)
            .unwrap()
            .len()),
        1000
    );
    time("search 'love'", &mut || {
        library.search("love", 1000).unwrap().len()
    });
    time("search 'river night'", &mut || {
        library.search("river night", 1000).unwrap().len()
    });
    time("stats", &mut || library.stats().unwrap().tracks as usize);
    assert_eq!(
        time("all albums", &mut || library.all_albums().unwrap().len()),
        library.stats().unwrap().albums as usize
    );
    time("genres", &mut || library.genres().unwrap().len());
    // Every track, for the Songs view: the most rows any view asks for.
    assert_eq!(
        time("all tracks", &mut || library.all_tracks().unwrap().len()),
        12_000
    );

    assert_eq!(
        time("recently added", &mut || library
            .recently_added(RECENT)
            .unwrap()
            .len()),
        RECENT
    );
    // A few years of listening: 20 000 plays of 3 000 tracks.
    for i in 0..20_000u64 {
        let track = (i * 7919) % 3_000;
        let path = library
            .db
            .query_row(
                "SELECT path FROM tracks WHERE id = ?1",
                [track as i64 + 1],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        library
            .record_play(Path::new(&path), None, at(i * 600))
            .unwrap();
    }
    assert_eq!(
        time("recently played", &mut || library
            .recently_played(RECENT)
            .unwrap()
            .len()),
        RECENT
    );
    assert_eq!(
        time("most played", &mut || library
            .most_played(RECENT)
            .unwrap()
            .len()),
        RECENT
    );
}

/// How many albums or tracks the Recents views show.
const RECENT: usize = 200;

#[test]
fn artists_sort_and_show_by_what_the_user_changed() {
    let mut library = Library::open_in_memory().unwrap();
    for (i, artist) in ["The Beatles", "Abba", "Cardigans"].into_iter().enumerate() {
        add(
            &mut library,
            &format!("/m/{i}.mp3"),
            &tags(artist, "B", "C", 1),
        );
    }
    let names = |library: &Library| -> Vec<String> {
        library
            .artists()
            .unwrap()
            .into_iter()
            .map(|a| a.shown_name.or(a.name).unwrap_or_default())
            .collect()
    };
    assert_eq!(names(&library), ["Abba", "Cardigans", "The Beatles"]);
    let edit = |name: Option<&str>, sort_name: Option<&str>| ArtistEdit {
        name: name.map(Into::into),
        sort_name: sort_name.map(Into::into),
        ..ArtistEdit::default()
    };
    library
        .set_artist_edit("The Beatles", &edit(None, Some("Beatles, The")))
        .unwrap();
    library
        .set_artist_edit("Cardigans", &edit(Some("The Cardigans"), None))
        .unwrap();
    // Sorted by the sort name, else the name shown.
    assert_eq!(names(&library), ["Abba", "The Beatles", "The Cardigans"]);
}

#[test]
fn an_album_matched_to_another_artist_of_the_name_is_filed_under_them() {
    let mut library = Library::open_in_memory().unwrap();
    add(
        &mut library,
        "/m/1.mp3",
        &tags("Shining", "V - Halmstad", "A", 1),
    );
    add(
        &mut library,
        "/m/2.mp3",
        &tags("Shining", "Blackjazz", "B", 1),
    );
    add(
        &mut library,
        "/m/3.mp3",
        &tags("Shining", "Blackjazz", "C", 2),
    );
    // Another band's album of the same name, without an artist: left alone.
    add(
        &mut library,
        "/m/4.mp3",
        &TagInfo {
            artist: None,
            ..tags("x", "Blackjazz", "D", 1)
        },
    );
    assert_eq!(library.artists().unwrap().len(), 2);

    let norwegian = split_artist("Shining", "no-id");
    let matched = AlbumMatch {
        album_artist: "Shining".into(),
        album: "Blackjazz".into(),
        release_group: Some("rg".into()),
        artist_mbid: "no-id".into(),
        artist_key: Some(norwegian.clone()),
    };
    library.match_album(&matched).unwrap();
    let names: Vec<Option<String>> = library
        .artists()
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(
        names,
        [Some("Shining".to_owned()), Some(norwegian.clone()), None]
    );
    assert_eq!(library.albums(Some(&norwegian)).unwrap()[0].tracks, 2);
    assert_eq!(
        library.album_match(&norwegian, "Blackjazz").unwrap(),
        Some(matched.clone())
    );
    assert_eq!(
        library.albums(None).unwrap().len(),
        1,
        "the untagged one stays"
    );

    // A scan reads the file again: still filed under the artist matched.
    let tx = library.transaction().unwrap();
    remove(&tx, Path::new("/m/2.mp3")).unwrap();
    tx.commit().unwrap();
    add(
        &mut library,
        "/m/2.mp3",
        &tags("Shining", "Blackjazz", "B", 1),
    );
    assert_eq!(library.albums(Some(&norwegian)).unwrap()[0].tracks, 2);

    library.unmatch_album("Shining", "Blackjazz").unwrap();
    assert_eq!(library.artists().unwrap().len(), 2);
    assert_eq!(library.albums(Some("Shining")).unwrap().len(), 2);
    assert_eq!(library.album_match(&norwegian, "Blackjazz").unwrap(), None);
}

#[test]
fn a_scan_makes_each_albums_cover_once_and_again_when_its_pictures_change() {
    let dir = temp_dir("covers");
    let album = dir.join("music").join("Artist").join("Album");
    std::fs::create_dir_all(&album).unwrap();
    std::fs::copy(fixtures().join("sine.mp3"), album.join("sine.mp3")).unwrap();
    let pictures = crate::pictures::Pictures::new(dir.join("pictures"));
    let options = ScanOptions {
        pictures: Some(pictures.clone()),
        ..ScanOptions::default()
    };
    let folders = [dir.join("music")];
    let mut library = Library::open_in_memory().unwrap();
    let scan = |library: &mut Library| {
        let mut covers = 0;
        scan_with(library, &folders, &options, &AtomicBool::new(false), |e| {
            if let ScanEvent::Covers { total, .. } = e {
                covers = total;
            }
        })
        .unwrap();
        covers
    };

    // No picture yet: the album is noted as without one.
    assert_eq!(scan(&mut library), 1);
    let covers = library.album_covers().unwrap();
    assert_eq!(covers.len(), 1);
    let (key, cover) = covers.iter().next().unwrap();
    assert_eq!(cover.cover, None);
    assert_eq!(scan(&mut library), 0, "nothing changed");

    // A cover put in the folder is made at the next scan, and kept among the pictures.
    image::RgbImage::from_pixel(1200, 1200, image::Rgb([200, 30, 30]))
        .save(album.join("cover.jpg"))
        .unwrap();
    assert_eq!(scan(&mut library), 1);
    let cover = &library.album_covers().unwrap()[key];
    let checksum = cover.cover.clone().expect("a cover");
    let made = image::load_from_memory(&pictures.read(&checksum).unwrap()).unwrap();
    assert_eq!((made.width(), made.height()), (600, 600));
    assert!(cover.color.is_some());
    assert!(
        library
            .artist_picture_checksums()
            .unwrap()
            .contains(&checksum)
    );
    assert_eq!(scan(&mut library), 0);

    // Found once, it stays: another picture in the folder does not replace it.
    image::RgbImage::from_pixel(1200, 1200, image::Rgb([30, 30, 200]))
        .save(album.join("cover.jpg"))
        .unwrap();
    assert_eq!(scan(&mut library), 0);
    assert_eq!(library.album_covers().unwrap()[key].cover, Some(checksum));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_scan_takes_in_the_pictures_in_artists_folders() {
    let dir = temp_dir("folder-pictures");
    let artist = dir.join("music").join("Freya");
    let album = artist.join("Album");
    std::fs::create_dir_all(&album).unwrap();
    std::fs::copy(fixtures().join("sine.mp3"), album.join("sine.mp3")).unwrap();
    image::RgbImage::from_pixel(2000, 2000, image::Rgb([0, 120, 0]))
        .save(artist.join("artist.jpg"))
        .unwrap();
    let pictures = crate::pictures::Pictures::new(dir.join("pictures"));
    let options = ScanOptions {
        pictures: Some(pictures.clone()),
        ..ScanOptions::default()
    };
    let folders = [dir.join("music")];
    let mut library = Library::open_in_memory().unwrap();
    let scan = |library: &mut Library| {
        scan_with(library, &folders, &options, &AtomicBool::new(false), |_| {}).unwrap();
    };
    scan(&mut library);
    let name = library.artists().unwrap()[0].name.clone().unwrap();
    let portrait = library
        .artist_picture(&name, PictureKind::Portrait)
        .unwrap()
        .unwrap();
    assert_eq!(portrait.source, "folder");
    let checksum = portrait.picture.unwrap();
    let made = image::load_from_memory(&pictures.read(&checksum).unwrap()).unwrap();
    assert_eq!((made.width(), made.height()), (1400, 1400));

    // Theirs it stays: another picture in the folder does not replace it, nor its going.
    image::RgbImage::from_pixel(500, 500, image::Rgb([0, 0, 200]))
        .save(artist.join("artist.jpg"))
        .unwrap();
    scan(&mut library);
    let kept = library
        .artist_picture(&name, PictureKind::Portrait)
        .unwrap()
        .unwrap();
    assert_eq!(kept.picture.as_deref(), Some(checksum.as_str()));
    std::fs::remove_file(artist.join("artist.jpg")).unwrap();
    scan(&mut library);
    assert!(
        library
            .artist_picture(&name, PictureKind::Portrait)
            .unwrap()
            .is_some()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Last.fm as a test has it: what it says of each artist, and who it was asked about.
struct FakeLastFm {
    asked: std::sync::Mutex<Vec<String>>,
}

impl LastFm for FakeLastFm {
    fn artist_text(
        &self,
        name: &str,
        _mbid: Option<&str>,
    ) -> std::result::Result<ArtistText, String> {
        self.asked.lock().unwrap().push(name.to_owned());
        if name == "Unknown" {
            return Err("no such artist".into());
        }
        Ok(ArtistText {
            summary: format!("{name}, a band."),
            ..ArtistText::default()
        })
    }
}

#[test]
fn a_scan_asks_the_internet_only_for_what_artists_lack() {
    let dir = temp_dir("fetch");
    let album = dir.join("music").join("Artist").join("Album");
    std::fs::create_dir_all(&album).unwrap();
    std::fs::copy(fixtures().join("sine.mp3"), album.join("sine.mp3")).unwrap();
    let lastfm = std::sync::Arc::new(FakeLastFm {
        asked: std::sync::Mutex::new(Vec::new()),
    });
    let options = ScanOptions {
        pictures: Some(crate::pictures::Pictures::new(dir.join("pictures"))),
        online: Online {
            lastfm: Some(lastfm.clone()),
            ..Online::default()
        },
        ..ScanOptions::default()
    };
    let folders = [dir.join("music")];
    let mut library = Library::open_in_memory().unwrap();
    let scan = |library: &mut Library| {
        let mut artists = 0;
        scan_with(library, &folders, &options, &AtomicBool::new(false), |e| {
            if let ScanEvent::Artists { total, .. } = e {
                artists = total;
            }
        })
        .unwrap();
        artists
    };
    assert_eq!(scan(&mut library), 1);
    let name = library.artists().unwrap()[0].name.clone().unwrap();
    assert_eq!(
        library.artist_text(&name).unwrap().unwrap().answer.summary,
        format!("{name}, a band.")
    );
    // Kept: not asked again, by this scan or any later.
    assert_eq!(scan(&mut library), 0);
    assert_eq!(lastfm.asked.lock().unwrap().len(), 1);

    // An artist the internet could not tell about is asked at the next scan.
    library
        .db
        .execute("UPDATE tracks SET album_artist = 'Unknown'", [])
        .unwrap();
    assert_eq!(scan(&mut library), 1);
    assert_eq!(scan(&mut library), 1);
    assert!(library.artist_text("Unknown").unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_scan_of_one_folder_leaves_the_others_alone() {
    let dir = temp_dir("one-folder");
    let (a, b) = (dir.join("a"), dir.join("b"));
    for folder in [&a, &b] {
        std::fs::create_dir_all(folder).unwrap();
        std::fs::copy(fixtures().join("sine.mp3"), folder.join("sine.mp3")).unwrap();
    }
    let folders = [a.clone(), b.clone()];
    let mut library = Library::open_in_memory().unwrap();
    scan_now(&mut library, &folders);
    assert_eq!(library.all_tracks().unwrap().len(), 2);

    // Gone from both; only the folder scanned notices.
    std::fs::remove_file(a.join("sine.mp3")).unwrap();
    std::fs::remove_file(b.join("sine.mp3")).unwrap();
    std::fs::copy(fixtures().join("sine.mp3"), b.join("other.mp3")).unwrap();
    let options = ScanOptions {
        only: vec![b.clone()],
        ..ScanOptions::default()
    };
    let full = library.last_full_scan().unwrap().expect("noted");
    let summary = scan_with(
        &mut library,
        &folders,
        &options,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!((summary.added, summary.removed), (1, 1));
    assert_eq!(
        library.last_full_scan().unwrap(),
        Some(full),
        "not a full scan"
    );
    let paths: Vec<PathBuf> = library
        .all_tracks()
        .unwrap()
        .into_iter()
        .map(|t| t.path)
        .collect();
    assert!(
        paths.contains(&a.join("sine.mp3")),
        "a was not looked through"
    );
    assert!(paths.contains(&b.join("other.mp3")));

    // A folder within one, taken away: its tracks go, though it is not there to look through.
    let album = a.join("Album");
    std::fs::create_dir_all(&album).unwrap();
    // Not empty without it: an empty folder of the library's is taken for a drive not there.
    std::fs::write(a.join("notes.txt"), "").unwrap();
    std::fs::copy(fixtures().join("sine.mp3"), album.join("01.mp3")).unwrap();
    let options = ScanOptions {
        only: vec![album.clone()],
        ..ScanOptions::default()
    };
    let scan = |library: &mut Library| {
        scan_with(library, &folders, &options, &AtomicBool::new(false), |_| {}).unwrap()
    };
    assert_eq!(scan(&mut library).added, 1);
    std::fs::remove_dir_all(&album).unwrap();
    assert_eq!(scan(&mut library).removed, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plays_follow_a_file_moved_or_renamed() {
    let dir = temp_dir("moved");
    let music = dir.join("music");
    let before = music.join("Artist").join("Album");
    std::fs::create_dir_all(&before).unwrap();
    std::fs::copy(fixtures().join("sine.mp3"), before.join("01.mp3")).unwrap();
    let mut library = Library::open_in_memory().unwrap();
    scan_now(&mut library, std::slice::from_ref(&music));
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    library
        .record_play(&before.join("01.mp3"), None, at)
        .unwrap();
    library
        .record_play(&before.join("01.mp3"), None, at)
        .unwrap();
    let added = |library: &Library| -> i64 {
        library
            .db
            .query_row("SELECT added FROM tracks", [], |row| row.get(0))
            .unwrap()
    };
    let first = added(&library);

    let after = music.join("Artist").join("Album (Remaster)");
    std::fs::create_dir_all(&after).unwrap();
    std::fs::rename(before.join("01.mp3"), after.join("01 Sine.mp3")).unwrap();
    let summary = scan_now(&mut library, std::slice::from_ref(&music));
    assert_eq!((summary.added, summary.removed), (1, 1));
    let played = library.most_played(10).unwrap();
    assert_eq!(played.len(), 1);
    assert_eq!(played[0].track.path, after.join("01 Sine.mp3"));
    assert_eq!(played[0].plays, 2);
    assert_eq!(added(&library), first, "it came in when it first did");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_scan_of_one_folder_makes_no_other_albums_covers() {
    let dir = temp_dir("one-folder-covers");
    let music = dir.join("music");
    // Albums apart by their tags, as the fixtures have them.
    for (album, file) in [("A", "sine.mp3"), ("B", "sine.flac")] {
        let folder = music.join(album);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::copy(fixtures().join(file), folder.join(file)).unwrap();
        image::RgbImage::from_pixel(300, 300, image::Rgb([0, 90, 0]))
            .save(folder.join("cover.jpg"))
            .unwrap();
    }
    let folders = [music.clone()];
    let pictures = crate::pictures::Pictures::new(dir.join("pictures"));
    let mut library = Library::open_in_memory().unwrap();
    let scan = |library: &mut Library, only: Vec<PathBuf>| {
        let options = ScanOptions {
            pictures: Some(pictures.clone()),
            only,
            ..ScanOptions::default()
        };
        let mut covers = 0;
        scan_with(library, &folders, &options, &AtomicBool::new(false), |e| {
            if let ScanEvent::Covers { total, .. } = e {
                covers = total;
            }
        })
        .unwrap();
        covers
    };
    assert_eq!(scan(&mut library, Vec::new()), 2);
    assert_eq!(scan(&mut library, vec![music.join("A")]), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_favourite_artist_is_as_recent_as_when_they_were_made_one() {
    let mut library = Library::open_in_memory().unwrap();
    add_at(&mut library, "/m/1.mp3", &tags("Old", "A", "One", 1), 1000);
    add_at(&mut library, "/m/2.mp3", &tags("New", "B", "Two", 1), 2000);
    let order = |library: &Library| -> Vec<String> {
        library
            .artists_by(ArtistOrder::RecentlyAdded)
            .unwrap()
            .into_iter()
            .filter_map(|a| a.name)
            .collect()
    };
    assert_eq!(order(&library), ["New", "Old"]);
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
    library.set_artist_favorite("Old", true, at).unwrap();
    assert_eq!(order(&library), ["Old", "New"]);
    assert!(library.artist_favorites().unwrap().contains_key("Old"));
    library.set_artist_favorite("Old", false, at).unwrap();
    assert_eq!(order(&library), ["New", "Old"]);

    library.set_playlist_favorite("Mix", true, at).unwrap();
    library.rename_playlist("Mix", "Night").unwrap();
    assert!(library.playlist_favorites().unwrap().contains_key("Night"));
    library.forget_playlist("Night").unwrap();
    assert!(library.playlist_favorites().unwrap().is_empty());
}

#[test]
fn a_track_of_many_genres_is_in_each() {
    assert_eq!(
        split_genres("Alternative Metal, industrial metal;Power Pop / alternative metal"),
        ["Alternative Metal", "industrial metal", "Power Pop"]
    );
    let mut library = Library::open_in_memory().unwrap();
    let mut both = tags("A", "Album", "One", 1);
    both.genre = Some("Black Metal, Ambient".into());
    let mut black = tags("B", "Other", "Two", 1);
    black.genre = Some("black metal".into());
    add(&mut library, "/m/1.mp3", &both);
    add(&mut library, "/m/2.mp3", &black);
    add(&mut library, "/m/3.mp3", &tags("C", "Third", "Three", 1));
    let genres = library.genres().unwrap();
    let names: Vec<Option<&str>> = genres.iter().map(|g| g.name.as_deref()).collect();
    assert_eq!(names.len(), 3, "{names:?}");
    assert_eq!(names[0], Some("Ambient"));
    assert!(names[1].is_some_and(|n| n.eq_ignore_ascii_case("black metal")));
    assert_eq!(names[2], None, "untagged last");
    assert_eq!((genres[1].albums, genres[1].tracks), (2, 2));
    assert_eq!(library.genre_albums(Some("BLACK METAL")).unwrap().len(), 2);
    assert_eq!(library.genre_tracks(Some("Ambient")).unwrap().len(), 1);
    assert_eq!(library.genre_tracks(None).unwrap().len(), 1);
    assert_eq!(
        library.artist_summary(Some("A"), 3).unwrap().genres.len(),
        2
    );
}
