use std::path::PathBuf;
use std::time::SystemTime;

use freya_protocol::{TagDraft, TagField, TagMap};
use rusqlite::params;

use super::{SavedFile, Thumbnails};
use crate::Library;

fn track(library: &Library, path: &str, artist: &str, album: &str) {
    library
        .db
        .execute(
            "INSERT INTO tracks (path, source, size, modified, album_artist, album, added)
             VALUES (?1, ?1, 1, 1, ?2, ?3, 1)",
            params![path, artist, album],
        )
        .unwrap();
}

fn saved(path: &str, fields: &[(TagField, &str)]) -> SavedFile {
    let after: TagMap = fields.iter().map(|(f, v)| (*f, (*v).to_owned())).collect();
    SavedFile {
        path: PathBuf::from(path),
        path_after: PathBuf::from(path),
        before: TagMap::new(),
        after,
    }
}

fn count(library: &Library, sql: &str) -> i64 {
    library.db.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn drafts_are_kept_until_they_change_nothing() {
    let mut library = Library::open_in_memory().unwrap();
    let path = PathBuf::from("/m/a.flac");
    let draft = TagDraft {
        path: path.clone(),
        changes: TagMap::from([(TagField::Title, "One".to_owned())]),
    };
    library
        .set_tag_drafts(std::slice::from_ref(&draft))
        .unwrap();
    assert_eq!(
        library.tag_drafts(std::slice::from_ref(&path)).unwrap(),
        [draft]
    );
    library
        .set_tag_drafts(&[TagDraft {
            path: path.clone(),
            changes: TagMap::new(),
        }])
        .unwrap();
    assert!(library.tag_drafts(&[path]).unwrap().is_empty());
}

#[test]
fn what_was_made_with_an_album_follows_its_new_name() {
    let mut library = Library::open_in_memory().unwrap();
    track(
        &library,
        "/m/1.flac",
        "Metallica",
        "Master Of Puppets (2016)",
    );
    track(
        &library,
        "/m/2.flac",
        "Metallica",
        "Master Of Puppets (2016)",
    );
    library
        .db
        .execute_batch(
            "INSERT INTO album_favorites (album_artist, album, at)
             VALUES ('Metallica', 'Master Of Puppets (2016)', 1);",
        )
        .unwrap();
    let old = Thumbnails::key(Some("Metallica"), Some("Master Of Puppets (2016)"));
    library
        .db
        .execute(
            "INSERT INTO album_chosen_covers (key, cover, chosen) VALUES (?1, 'c', 1)",
            [&old],
        )
        .unwrap();
    let rename = |path| saved(path, &[(TagField::Album, "Master of Puppets")]);
    library
        .note_tag_save(
            "/m".as_ref(),
            &[rename("/m/1.flac"), rename("/m/2.flac")],
            SystemTime::now(),
        )
        .unwrap();
    assert_eq!(
        count(
            &library,
            "SELECT count(*) FROM album_favorites WHERE album = 'Master of Puppets'"
        ),
        1
    );
    let new = Thumbnails::key(Some("Metallica"), Some("Master of Puppets"));
    let found: i64 = library
        .db
        .query_row(
            "SELECT count(*) FROM album_chosen_covers WHERE key = ?1",
            [&new],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(found, 1);
}

#[test]
fn an_album_only_partly_renamed_keeps_what_was_made_with_it() {
    let mut library = Library::open_in_memory().unwrap();
    track(&library, "/m/1.flac", "A", "Album");
    track(&library, "/m/2.flac", "A", "Album");
    library
        .db
        .execute_batch(
            "INSERT INTO album_favorites (album_artist, album, at) VALUES ('A', 'Album', 1);",
        )
        .unwrap();
    library
        .note_tag_save(
            "/m".as_ref(),
            &[saved("/m/1.flac", &[(TagField::Album, "Other")])],
            SystemTime::now(),
        )
        .unwrap();
    assert_eq!(
        count(
            &library,
            "SELECT count(*) FROM album_favorites WHERE album = 'Album'"
        ),
        1
    );
}

#[test]
fn what_was_made_with_an_artist_follows_their_new_name() {
    let mut library = Library::open_in_memory().unwrap();
    track(&library, "/m/1.flac", "metallica", "Album");
    library
        .db
        .execute_batch("INSERT INTO artist_favorites (artist, at) VALUES ('metallica', 1);")
        .unwrap();
    library
        .note_tag_save(
            "/m".as_ref(),
            &[saved("/m/1.flac", &[(TagField::AlbumArtist, "Metallica")])],
            SystemTime::now(),
        )
        .unwrap();
    assert_eq!(
        count(
            &library,
            "SELECT count(*) FROM artist_favorites WHERE artist = 'Metallica'"
        ),
        1
    );
}

#[test]
fn plays_follow_a_moved_file() {
    let mut library = Library::open_in_memory().unwrap();
    track(&library, "/m/old.flac", "A", "Album");
    library
        .record_play("/m/old.flac".as_ref(), None, SystemTime::now(), None)
        .unwrap();
    let mut file = saved("/m/old.flac", &[]);
    file.path_after = PathBuf::from("/m/new.flac");
    library
        .note_tag_save("/m".as_ref(), &[file], SystemTime::now())
        .unwrap();
    assert_eq!(
        count(
            &library,
            "SELECT count(*) FROM plays WHERE path = '/m/new.flac'"
        ),
        1
    );
    assert_eq!(
        count(
            &library,
            "SELECT count(*) FROM tracks WHERE path = '/m/new.flac'"
        ),
        1
    );
}
