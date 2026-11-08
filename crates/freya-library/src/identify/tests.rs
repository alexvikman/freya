use std::collections::HashMap;

use serde_json::json;

use super::*;

fn file(number: u32, title: &str, seconds: u64) -> FileToMatch {
    FileToMatch {
        path: PathBuf::from(format!("/m/{number:02}.flac")),
        title: Some(title.to_owned()),
        track: Some(number),
        disc: None,
        duration: Some(Duration::from_secs(seconds)),
    }
}

fn track(number: u32, title: &str, seconds: u64) -> ReleaseTrack {
    ReleaseTrack {
        disc: 1,
        number,
        title: title.to_owned(),
        artist: "Metallica".to_owned(),
        artist_id: "a".to_owned(),
        length: Some(Duration::from_secs(seconds)),
        recording_id: format!("rec-{number}"),
        track_id: format!("track-{number}"),
    }
}

fn release(id: &str, date: &str, note: &str, tracks: Vec<ReleaseTrack>) -> Release {
    Release {
        id: id.to_owned(),
        title: "Master of Puppets".to_owned(),
        artist: "Metallica".to_owned(),
        artist_id: "a".to_owned(),
        group_id: "g".to_owned(),
        date: date.to_owned(),
        first_date: "1986-03-03".to_owned(),
        country: "US".to_owned(),
        disambiguation: note.to_owned(),
        discs: 1,
        tracks,
        ..Release::default()
    }
}

fn puppets() -> FolderToMatch {
    FolderToMatch {
        artist: Some("Metallica".to_owned()),
        album: Some("Master Of Puppets".to_owned()),
        edition: Some("2016 Remastered".to_owned()),
        release_id: None,
        files: vec![file(1, "Battery", 312), file(2, "Master Of Puppets", 515)],
    }
}

fn tracks() -> Vec<ReleaseTrack> {
    vec![track(1, "Battery", 313), track(2, "Master of Puppets", 516)]
}

#[derive(Default)]
struct Fake {
    found: Vec<Found>,
    releases: HashMap<String, Release>,
}

impl Fake {
    fn with(releases: Vec<Release>) -> Self {
        let mut fake = Self::default();
        for release in releases {
            fake.found.push(Found {
                id: release.id.clone(),
                tracks: release.tracks.len() as u32,
                score: 100,
            });
            fake.releases.insert(release.id.clone(), release);
        }
        fake
    }
}

impl Source for Fake {
    fn search(&self, _: &str) -> Result<Vec<Found>, String> {
        Ok(self.found.clone())
    }

    fn release(&self, id: &str) -> Result<Release, String> {
        self.releases
            .get(id)
            .cloned()
            .ok_or_else(|| "no".to_owned())
    }
}

#[test]
fn a_release_that_fits_is_ready_to_save() {
    let fake = Fake::with(vec![release("r1", "2016-04-15", "2016 remaster", tracks())]);
    let found = identify(&puppets(), &fake).unwrap();
    assert_eq!(found.state, ProposalState::Ready);
    assert_eq!(found.chosen.as_deref(), Some("r1"));
    assert_eq!(found.drafts.len(), 2);
    let draft = &found
        .drafts
        .iter()
        .find(|d| d.path.ends_with("02.flac"))
        .unwrap()
        .changes;
    assert_eq!(draft[&TagField::Title], "Master of Puppets");
    assert_eq!(draft[&TagField::Album], "Master of Puppets");
    assert_eq!(draft[&TagField::Edition], "2016 Remastered");
    assert_eq!(draft[&TagField::OriginalDate], "1986-03-03");
    assert_eq!(draft[&TagField::RecordingId], "rec-2");
    assert_eq!(draft[&TagField::ReleaseGroupId], "g");
    assert_eq!(draft[&TagField::TrackTotal], "2");
    assert!(
        !draft.contains_key(&TagField::Genre),
        "what it does not know stays"
    );
}

#[test]
fn the_edition_the_folder_names_goes_first() {
    let fake = Fake::with(vec![
        release("r1", "1986-03-03", "", tracks()),
        release("r2", "2016-04-15", "2016 remaster", tracks()),
    ]);
    let found = identify(&puppets(), &fake).unwrap();
    assert_eq!(found.chosen.as_deref(), Some("r2"));
    assert_eq!(found.candidates[0].release_id, "r2");
}

#[test]
fn releases_it_cannot_tell_apart_are_for_the_user_to_look_at() {
    let mut other = tracks();
    other[0].recording_id = "another".to_owned();
    let fake = Fake::with(vec![
        release("r1", "2000", "", tracks()),
        release("r2", "2001", "", other),
    ]);
    let mut folder = puppets();
    folder.edition = None;
    assert_eq!(
        identify(&folder, &fake).unwrap().state,
        ProposalState::Review
    );
}

#[test]
fn the_same_recordings_are_no_reason_to_ask_when_no_edition_is_named() {
    let fake = Fake::with(vec![
        release("r1", "2000", "", tracks()),
        release("r2", "2001", "", tracks()),
    ]);
    let mut folder = puppets();
    folder.edition = None;
    assert_eq!(
        identify(&folder, &fake).unwrap().state,
        ProposalState::Ready
    );
}

#[test]
fn a_release_of_other_lengths_is_not_suggested() {
    let wrong = vec![track(1, "Other", 100), track(2, "Another", 900)];
    let fake = Fake::with(vec![release("r1", "2000", "", wrong)]);
    let found = identify(&puppets(), &fake).unwrap();
    assert_eq!(found.state, ProposalState::None);
    assert!(found.drafts.is_empty());
}

#[test]
fn a_deluxe_edition_does_not_fit_the_album_alone() {
    let mut deluxe = tracks();
    deluxe.extend((3..=20).map(|n| track(n, "Bonus", 200)));
    let (_, whole) = pair(&puppets(), &release("r", "2016", "", tracks()));
    let (_, longer) = pair(&puppets(), &release("r", "2016", "", deluxe));
    assert!(whole > longer + 0.5, "{whole} {longer}");
}

#[test]
fn files_without_tags_are_matched_by_their_lengths() {
    let mut folder = puppets();
    for one in &mut folder.files {
        one.title = None;
        one.track = None;
    }
    folder.files.swap(0, 1);
    let release = release("r", "2016", "", tracks());
    let (pairs, score) = pair(&folder, &release);
    assert!(score > 0.9, "{score}");
    assert!(
        pairs.contains(&(0, 1)) && pairs.contains(&(1, 0)),
        "{pairs:?}"
    );
}

#[test]
fn the_release_its_files_name_is_looked_at_first() {
    let mut fake = Fake::with(vec![]);
    fake.releases
        .insert("named".to_owned(), release("named", "2016", "", tracks()));
    let mut folder = puppets();
    folder.release_id = Some("named".to_owned());
    assert_eq!(
        identify(&folder, &fake).unwrap().chosen.as_deref(),
        Some("named")
    );
}

#[test]
fn quotes_what_it_searches_for() {
    let folder = FolderToMatch {
        artist: Some("AC/DC".to_owned()),
        album: Some("Say \"Hi\"".to_owned()),
        ..FolderToMatch::default()
    };
    assert_eq!(
        search_query(&folder).unwrap(),
        "release:\"Say \\\"Hi\\\"\" AND artist:\"AC/DC\""
    );
    assert_eq!(search_query(&FolderToMatch::default()), None);
}

#[test]
fn reads_a_release_from_musicbrainz() {
    let answer = json!({
        "id": "r1",
        "title": "Master of Puppets",
        "date": "2017-11-10",
        "country": "XW",
        "disambiguation": "remastered deluxe box set",
        "artist-credit": [{ "name": "Metallica", "joinphrase": "", "artist": { "id": "a1" } }],
        "release-group": { "id": "g1", "first-release-date": "1986-03-03" },
        "label-info": [{ "label": { "name": "Blackened" } }],
        "media": [
            { "position": 1, "format": "Digital Media", "tracks": [
                { "id": "t1", "position": 1, "title": "Battery", "length": 312000,
                  "recording": { "id": "c1", "title": "Battery" } },
            ] },
            { "position": 2, "format": "Digital Media", "tracks": [
                { "id": "t2", "position": 1, "title": "Orion", "length": 500000,
                  "artist-credit": [{ "name": "Other", "artist": { "id": "a2" } }],
                  "recording": { "id": "c2", "title": "Orion" } },
            ] },
        ],
    });
    let release = parse_release(&answer).unwrap();
    assert_eq!(release.first_date, "1986-03-03");
    assert_eq!(release.label, "Blackened");
    assert_eq!(release.format, "Digital Media");
    assert_eq!(release.discs, 2);
    assert_eq!(release.tracks[1].disc, 2);
    assert_eq!(release.tracks[1].artist, "Other");
    assert_eq!(release.tracks[1].artist_id, "a2");
    assert_eq!(release.tracks[0].artist, "");
    assert_eq!(release.tracks[0].recording_id, "c1");
}

#[test]
fn reads_what_a_search_found() {
    let answer = json!({ "releases": [
        { "id": "a", "track-count": 8, "score": 100 },
        { "track-count": 1 },
    ] });
    let found = parse_search(&answer);
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].tracks, found[0].score), (8, 100));
}

#[test]
fn takes_the_album_from_its_tags_or_its_folder() {
    let folder = freya_protocol::FolderName {
        year: Some(1986),
        title: "From Folder".to_owned(),
        edition: Some("2016 Remastered".to_owned()),
    };
    assert_eq!(
        album_and_edition(Some("Puppets (Deluxe)"), &folder),
        (
            Some("Puppets".to_owned()),
            Some("2016 Remastered".to_owned())
        )
    );
    assert_eq!(
        album_and_edition(None, &folder).0.as_deref(),
        Some("From Folder")
    );
}
