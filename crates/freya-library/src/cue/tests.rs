use super::encoding::decode;
use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("freya-cue-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes empty files; finding and cutting files does not read their audio.
fn touch(dir: &Path, names: &[&str]) {
    for name in names {
        std::fs::write(dir.join(name), b"").unwrap();
    }
}

fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

const ALBUM: &str = r#"REM GENRE "Vocal Jazz"
REM DATE 2004
REM DISCID 93129E1B
PERFORMER "Various"
TITLE "The Story of Vocal Jazz - CD 2"
FILE "Album.wav" WAVE
  TRACK 01 AUDIO
    TITLE "Mahalia Jackson - Amazing grace"
    PERFORMER "Mahalia Jackson - Amazing grace"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Good rockin' tonight"
    PERFORMER "Roy Brown"
    INDEX 00 02:37:64
    INDEX 01 02:41:64
  TRACK 03 AUDIO
    TITLE "Hi de ho man"
    INDEX 01 06:12:39
"#;

#[test]
fn parses_a_sheet() {
    let sheet = Sheet::parse(ALBUM);
    assert_eq!(
        sheet.title.as_deref(),
        Some("The Story of Vocal Jazz - CD 2")
    );
    assert_eq!(sheet.performer.as_deref(), Some("Various"));
    assert_eq!(sheet.genre.as_deref(), Some("Vocal Jazz"));
    assert_eq!(sheet.year, Some(2004));
    assert_eq!(sheet.files, ["Album.wav"]);
    let starts: Vec<_> = sheet.tracks.iter().map(|t| t.start).collect();
    // 2:41 and 64/75 of a second; 6:12 and 39/75.
    assert_eq!(starts, [Some(ms(0)), Some(ms(161_853)), Some(ms(372_520))]);
    assert_eq!(sheet.tracks[2].number, 3);
}

#[test]
fn tracks_take_the_sheets_tags_where_they_have_none() {
    let sheet = Sheet::parse(ALBUM);
    let part = Part {
        start: ms(161_853),
        end: Some(ms(372_520)),
    };
    let second = sheet.tags(&sheet.tracks[1], part);
    assert_eq!(second.title.as_deref(), Some("Good rockin' tonight"));
    assert_eq!(second.artist.as_deref(), Some("Roy Brown"));
    assert_eq!(
        second.album.as_deref(),
        Some("The Story of Vocal Jazz - CD 2")
    );
    assert_eq!(second.album_artist.as_deref(), Some("Various"));
    assert_eq!(second.track, Some(2));
    assert_eq!(second.duration, Some(ms(210_667)));

    let third = sheet.tags(
        &sheet.tracks[2],
        Part {
            start: ms(0),
            end: None,
        },
    );
    assert_eq!(third.artist.as_deref(), Some("Various"));
    assert_eq!(third.duration, None);
}

#[test]
fn splits_artist_and_title_given_as_both() {
    let sheet = Sheet::parse(ALBUM);
    let first = sheet.tags(
        &sheet.tracks[0],
        Part {
            start: ms(0),
            end: None,
        },
    );
    assert_eq!(first.artist.as_deref(), Some("Mahalia Jackson"));
    assert_eq!(first.title.as_deref(), Some("Amazing grace"));
}

#[test]
fn reads_unquoted_and_odd_values() {
    let sheet = Sheet::parse(
        "title Plain words\r\nREM DATE \"1999/05/01\"\r\nREM DISCNUMBER 2\r\n\
         FILE C:\\Rips\\My Album.flac WAVE\r\n  TRACK 1 AUDIO\r\n    TITLE \"Say \"hi\"\"\r\n\
         INDEX 01 1:02:03\r\n  TRACK 2 AUDIO\r\n    INDEX 01 nonsense\r\n",
    );
    assert_eq!(sheet.title.as_deref(), Some("Plain words"));
    assert_eq!((sheet.year, sheet.disc), (Some(1999), Some(2)));
    assert_eq!(sheet.files, ["C:\\Rips\\My Album.flac"]);
    assert_eq!(sheet.tracks[0].title.as_deref(), Some("Say \"hi\""));
    assert_eq!(sheet.tracks[0].start, Some(ms(62_040)));
    assert_eq!(sheet.tracks[1].start, None);
}

#[test]
fn a_track_belongs_to_the_file_its_index_01_is_in() {
    let sheet = Sheet::parse(
        "FILE \"a.flac\" WAVE\n TRACK 01 AUDIO\n  INDEX 01 00:00:00\n TRACK 02 AUDIO\n  \
         INDEX 00 03:00:00\nFILE \"b.flac\" WAVE\n  INDEX 01 00:00:00\n",
    );
    assert_eq!(sheet.tracks[1].file, 1);
}

#[test]
fn decodes_the_usual_encodings() {
    assert_eq!(
        decode(b"\xEF\xBB\xBFTITLE \"Bj\xC3\xB6rk\""),
        "TITLE \"Björk\""
    );
    assert_eq!(decode("Beyoncé".as_bytes()), "Beyoncé");
    // UTF-16 with a byte order mark, as some Windows rippers write.
    let utf16: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain("Sigur Rós".encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    assert_eq!(decode(&utf16), "Sigur Rós");
    // Windows-1252: an accented letter here and there, and typographic quotes.
    assert_eq!(
        decode(b"Mot\xF6rhead \x93Ace\x94 \x80"),
        "Motörhead “Ace” €"
    );
    // Windows-1251: words of Cyrillic letters.
    assert_eq!(decode(b"\xCA\xE8\xED\xEE \xB9 1"), "Кино № 1");
    assert_eq!(decode(b"\xC4\xE0\xF8\xE0 \xE8 \xF1\xEE\xED"), "Даша и сон");
}

#[test]
fn reads_a_disc_and_finds_its_file_under_another_extension() {
    let dir = temp_dir("disc");
    touch(&dir, &["Album.ape", "cover.jpg"]);
    std::fs::write(dir.join("Album.cue"), ALBUM).unwrap();

    let disc = read_disc(&dir.join("Album.cue")).unwrap();
    assert_eq!(disc.files, [dir.join("Album.ape")]);
    let parts: Vec<Part> = disc.tracks.iter().map(|t| t.part).collect();
    assert_eq!(
        parts,
        [
            Part {
                start: ms(0),
                end: Some(ms(161_853))
            },
            Part {
                start: ms(161_853),
                end: Some(ms(372_520))
            },
            Part {
                start: ms(372_520),
                end: None
            },
        ]
    );
    assert!(disc.tracks.iter().all(|t| t.path == dir.join("Album.ape")));
}

#[test]
fn a_single_file_sheet_finds_the_file_named_like_itself() {
    let dir = temp_dir("named");
    touch(&dir, &["CDImage.flac", "Bonus.mp3"]);
    std::fs::write(
        dir.join("CDImage.flac.cue"),
        "FILE \"Range.wav\" WAVE\n TRACK 01 AUDIO\n  INDEX 01 00:00:00\n \
         TRACK 02 AUDIO\n  INDEX 01 01:00:00\n",
    )
    .unwrap();
    let disc = read_disc(&dir.join("CDImage.flac.cue")).unwrap();
    assert_eq!(disc.files, [dir.join("CDImage.flac")]);
}

#[test]
fn files_that_are_tracks_already_are_left_alone() {
    let dir = temp_dir("per-track");
    touch(&dir, &["01.flac", "02.flac"]);
    let sheet = dir.join("Album.cue");
    std::fs::write(
        &sheet,
        "FILE \"01.flac\" WAVE\n TRACK 01 AUDIO\n  INDEX 01 00:00:00\n\
         FILE \"02.flac\" WAVE\n TRACK 02 AUDIO\n  INDEX 01 00:00:00\n",
    )
    .unwrap();
    assert_eq!(read_disc(&sheet), None);
    let found = apply_sheets(files::collect_with_sheets(std::slice::from_ref(&dir)));
    assert_eq!(
        found,
        [
            Found::File(dir.join("01.flac")),
            Found::File(dir.join("02.flac"))
        ]
    );
}

#[test]
fn a_disc_replaces_its_file_where_it_was_found() {
    let dir = temp_dir("apply");
    touch(&dir, &["A.mp3", "Album.flac", "Z.mp3", ".hidden.cue"]);
    std::fs::write(dir.join("Album.cue"), ALBUM).unwrap();
    // A second sheet for the same file, not named after it: ignored.
    std::fs::write(dir.join("Album (copy).cue"), ALBUM).unwrap();

    let found = apply_sheets(files::collect_with_sheets(std::slice::from_ref(&dir)));
    assert_eq!(found.len(), 3);
    assert_eq!(found[0], Found::File(dir.join("A.mp3")));
    let Found::Disc(disc) = &found[1] else {
        panic!("{:?}", found[1]);
    };
    assert_eq!(disc.sheet, dir.join("Album.cue"));
    assert_eq!(disc.tracks.len(), 3);
    assert_eq!(found[2], Found::File(dir.join("Z.mp3")));
}

#[test]
fn a_sheet_given_on_its_own_brings_its_tracks() {
    let dir = temp_dir("alone");
    touch(&dir, &["Album.flac"]);
    std::fs::write(dir.join("Album.cue"), ALBUM).unwrap();
    let found = apply_sheets(vec![dir.join("Album.cue")]);
    assert!(matches!(&found[..], [Found::Disc(d)] if d.tracks.len() == 3));
}

#[test]
fn a_sheet_without_its_audio_is_left_out() {
    let dir = temp_dir("missing");
    // WavPack, which Freya does not play.
    touch(&dir, &["Album.wv", "Other.flac", "More.flac"]);
    std::fs::write(dir.join("Album.cue"), ALBUM).unwrap();
    assert_eq!(read_disc(&dir.join("Album.cue")), None);
}

#[test]
fn finds_the_disc_number_in_the_sheets_name() {
    let disc = |name: &str, title: &str| disc_in_name(Path::new(name), Some(title));
    assert_eq!(disc("Strings - Classics [CD2].cue", "Classics"), Some(2));
    assert_eq!(disc("Album - Disc 3.cue", "Album"), Some(3));
    assert_eq!(disc("album_disk_1.cue", "Album"), Some(1));
    // Part of the title: a series of single discs.
    assert_eq!(disc("Jazz Vocal - CD 14.cue", "Jazz Vocal - CD 14"), None);
    // Not a word of its own, or no number.
    assert_eq!(disc("ABCD2.cue", ""), None);
    assert_eq!(disc("CDImage.cue", ""), None);
}
