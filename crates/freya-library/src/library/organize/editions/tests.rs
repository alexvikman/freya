use std::path::Path;

use rusqlite::params;

use crate::Library;

struct Tune<'a> {
    folder: &'a str,
    album: &'a str,
    title: &'a str,
    number: u32,
    group: Option<&'a str>,
}

fn add(library: &Library, tune: Tune<'_>) {
    let path = format!("{}/{:02}.flac", tune.folder, tune.number);
    library
        .db
        .execute(
            "INSERT INTO tracks (path, source, size, modified, album_artist, album, title, track,
                                 release_group_id, added)
             VALUES (?1, ?1, 1, 1, 'Metallica', ?2, ?3, ?4, ?5, 1)",
            params![path, tune.album, tune.title, tune.number, tune.group],
        )
        .unwrap();
}

fn album(library: &Library, folder: &str, name: &str, titles: &[&str], group: Option<&str>) {
    for (at, title) in titles.iter().enumerate() {
        add(
            library,
            Tune {
                folder,
                album: name,
                title,
                number: at as u32 + 1,
                group,
            },
        );
    }
}

const PUPPETS_2016: &str = "/m/Metallica/(1986) Master Of Puppets (2016 Remastered)";
const PUPPETS_2017: &str = "/m/Metallica/(1986) Master Of Puppets (2017 Remastered Deluxe Edition)";

fn puppets(library: &Library) {
    album(
        library,
        PUPPETS_2016,
        "Master Of Puppets (Remastered)",
        &["Battery", "Master Of Puppets", "Orion"],
        None,
    );
    album(
        library,
        PUPPETS_2017,
        "Master Of Puppets (Remastered Deluxe Box Set)",
        &["Battery", "Master Of Puppets", "Orion", "Battery (Live)"],
        None,
    );
}

fn names(library: &Library) -> Vec<String> {
    library
        .albums(Some("Metallica"))
        .unwrap()
        .into_iter()
        .map(|a| a.name.unwrap())
        .collect()
}

#[test]
fn editions_are_one_album_and_show_as_editions() {
    let mut library = Library::open_in_memory().unwrap();
    puppets(&library);
    library.assign_editions().unwrap();

    assert_eq!(names(&library), ["Master Of Puppets"]);
    let tracks = library
        .album_tracks(Some("Metallica"), Some("Master Of Puppets"))
        .unwrap();
    assert_eq!(tracks.len(), 3, "the smaller edition is the one listed");
    let editions = library
        .album_editions(Some("Metallica"), Some("Master Of Puppets"))
        .unwrap();
    assert_eq!(editions.len(), 2);
    let preferred: Vec<_> = editions.iter().filter(|e| e.preferred).collect();
    assert_eq!(preferred.len(), 1);
    assert_eq!(preferred[0].folder, Path::new(PUPPETS_2016));
    assert_eq!(preferred[0].label, "2016 Remastered");
    assert_eq!(library.all_tracks().unwrap().len(), 3);
}

#[test]
fn the_user_chooses_the_edition_and_it_stays() {
    let mut library = Library::open_in_memory().unwrap();
    puppets(&library);
    library.assign_editions().unwrap();
    library
        .set_preferred_edition("Metallica", "Master Of Puppets", Path::new(PUPPETS_2017))
        .unwrap();
    let count = |library: &Library| {
        library
            .album_tracks(Some("Metallica"), Some("Master Of Puppets"))
            .unwrap()
            .len()
    };
    assert_eq!(count(&library), 4);
    library.assign_editions().unwrap();
    assert_eq!(count(&library), 4, "a later scan keeps the choice");
}

#[test]
fn a_favourite_follows_the_album_to_the_shared_name() {
    let mut library = Library::open_in_memory().unwrap();
    puppets(&library);
    library
        .db
        .execute(
            "INSERT INTO album_favorites (album_artist, album, at)
             VALUES ('Metallica', 'Master Of Puppets (Remastered)', 1)",
            [],
        )
        .unwrap();
    library.assign_editions().unwrap();
    let found: i64 = library
        .db
        .query_row(
            "SELECT count(*) FROM album_favorites WHERE album = 'Master Of Puppets'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(found, 1);
}

#[test]
fn albums_with_a_title_in_common_and_no_tracks_stay_apart() {
    let mut library = Library::open_in_memory().unwrap();
    album(
        &library,
        "/m/Metallica/Blue",
        "Weezer (Deluxe)",
        &["One", "Two"],
        None,
    );
    album(
        &library,
        "/m/Metallica/Green",
        "Weezer",
        &["Three", "Four"],
        None,
    );
    library.assign_editions().unwrap();
    assert_eq!(names(&library).len(), 2);
    assert!(
        library
            .album_editions(Some("Metallica"), Some("Weezer"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn different_release_groups_stay_apart_whatever_they_share() {
    let mut library = Library::open_in_memory().unwrap();
    let titles = ["One", "Two"];
    album(
        &library,
        "/m/Metallica/A",
        "Album (Remastered)",
        &titles,
        Some("g1"),
    );
    album(&library, "/m/Metallica/B", "Album", &titles, Some("g2"));
    library.assign_editions().unwrap();
    assert_eq!(names(&library).len(), 2);
}

#[test]
fn one_release_group_is_one_album_whatever_they_are_called() {
    let mut library = Library::open_in_memory().unwrap();
    album(&library, "/m/Metallica/A", "Ride", &["One"], Some("g"));
    album(
        &library,
        "/m/Metallica/B",
        "Ride The Lightning",
        &["Two"],
        Some("g"),
    );
    library.assign_editions().unwrap();
    assert_eq!(names(&library).len(), 1);
}

#[test]
fn a_folder_of_its_own_is_left_as_it_is() {
    let mut library = Library::open_in_memory().unwrap();
    album(
        &library,
        "/m/Metallica/A",
        "Load (Remastered)",
        &["One"],
        None,
    );
    library.assign_editions().unwrap();
    assert_eq!(names(&library), ["Load (Remastered)"]);
}

#[test]
fn discs_of_an_album_are_not_editions_of_it() {
    let mut library = Library::open_in_memory().unwrap();
    album(&library, "/m/Metallica/S&M/CD1", "S&M", &["One"], None);
    album(&library, "/m/Metallica/S&M/CD2", "S&M", &["Two"], None);
    library.assign_editions().unwrap();
    assert_eq!(
        library
            .album_tracks(Some("Metallica"), Some("S&M"))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn copies_nothing_calls_editions_stay_as_they_were() {
    let mut library = Library::open_in_memory().unwrap();
    album(&library, "/m/Metallica/A", "Album", &["One", "Two"], None);
    album(&library, "/m/Metallica/B", "Album", &["One", "Two"], None);
    library.assign_editions().unwrap();
    assert_eq!(
        library
            .album_tracks(Some("Metallica"), Some("Album"))
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn music_in_the_inbox_is_not_the_librarys_yet() {
    let mut library = Library::open_in_memory().unwrap();
    album(&library, "/m/Inbox/New", "New Album", &["One"], None);
    album(&library, "/m/Metallica/Load", "Load", &["Two"], None);
    library.set_inbox(Some(Path::new("/m/Inbox"))).unwrap();
    assert_eq!(names(&library), ["Load"]);
    assert!(
        library
            .all_tracks()
            .unwrap()
            .iter()
            .all(|t| t.title.as_deref() == Some("Two"))
    );

    let folders = library.organize_folders().unwrap();
    assert!(folders.iter().any(|f| f.name == "New" && f.inbox));
    assert!(folders.iter().any(|f| f.name == "Load" && !f.inbox));

    library.set_inbox(None).unwrap();
    assert_eq!(names(&library).len(), 2);
}

#[test]
fn music_let_in_from_the_inbox_is_listed() {
    use std::time::SystemTime;

    use freya_protocol::TagMap;

    use crate::library::organize::SavedFile;

    let mut library = Library::open_in_memory().unwrap();
    album(&library, "/m/Inbox/New", "New Album", &["One"], None);
    library.set_inbox(Some(Path::new("/m/Inbox"))).unwrap();
    assert!(names(&library).is_empty());
    let moved = SavedFile {
        path: "/m/Inbox/New/01.flac".into(),
        path_after: "/m/Metallica/New/01.flac".into(),
        before: TagMap::new(),
        after: TagMap::new(),
    };
    library
        .note_tag_save(Path::new("/m/Metallica/New"), &[moved], SystemTime::now())
        .unwrap();
    library.assign_editions().unwrap();
    assert_eq!(names(&library), ["New Album"]);
}

#[test]
fn an_inbox_copy_is_no_edition_of_what_is_in_the_library() {
    let mut library = Library::open_in_memory().unwrap();
    album(&library, "/m/Inbox/Load (Deluxe)", "Load", &["One"], None);
    album(&library, "/m/Metallica/Load", "Load", &["One"], None);
    library.set_inbox(Some(Path::new("/m/Inbox"))).unwrap();
    assert!(
        library
            .album_editions(Some("Metallica"), Some("Load"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        library
            .album_tracks(Some("Metallica"), Some("Load"))
            .unwrap()
            .len(),
        1
    );
}
