//! What the internet said of the library's artists: which of the downloaded pictures is
//! theirs (see [`crate::pictures`]), their MusicBrainz id, and what Last.fm says of them. Kept
//! with the user's plays, as getting them again takes the internet: never dropped.

use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use rusqlite::{OptionalExtension, params};

use super::{Library, Result, epoch_millis, from_epoch_millis};

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS artist_pictures (
        artist TEXT NOT NULL,
        -- 'portrait' or 'backdrop'.
        kind TEXT NOT NULL,
        -- Where it was asked for: 'deezer' or 'fanart.tv'.
        source TEXT NOT NULL,
        -- The picture's SHA-256 among the downloaded pictures; NULL when the source had none.
        picture TEXT,
        -- When, in milliseconds since 1970.
        asked INTEGER NOT NULL,
        PRIMARY KEY (artist, kind)
    );
    -- What MusicBrainz's search found for artists whose tags give no id.
    CREATE TABLE IF NOT EXISTS artist_ids (
        artist TEXT NOT NULL PRIMARY KEY,
        mbid TEXT,
        searched INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS artist_info (
        artist TEXT NOT NULL PRIMARY KEY,
        summary TEXT NOT NULL,
        biography TEXT NOT NULL,
        -- A JSON array of strings.
        tags TEXT NOT NULL,
        url TEXT NOT NULL,
        asked INTEGER NOT NULL
    );
";

/// How long an answer is believed before the internet is asked again: pictures are added,
/// and biographies written, all the time.
pub const ASK_AGAIN_AFTER: Duration = Duration::from_secs(30 * 24 * 3600);

/// A picture of an artist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PictureKind {
    /// Square, for the grid of artists and beside their name.
    Portrait,
    /// Wide, across the top of their page.
    Backdrop,
}

impl PictureKind {
    fn name(self) -> &'static str {
        match self {
            Self::Portrait => "portrait",
            Self::Backdrop => "backdrop",
        }
    }
}

/// An answer from the internet, and when it was given.
#[derive(Clone, Debug, PartialEq)]
pub struct Asked<T> {
    pub answer: T,
    pub asked: SystemTime,
}

impl<T> Asked<T> {
    /// Whether it is recent enough not to ask again.
    pub fn recent(&self) -> bool {
        self.asked.elapsed().is_ok_and(|age| age < ASK_AGAIN_AFTER)
    }
}

/// What Last.fm says of an artist, in plain text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArtistText {
    pub summary: String,
    pub biography: String,
    pub tags: Vec<String>,
    pub url: String,
}

impl Library {
    /// Which downloaded picture is the artist's, if one; asked of their source when.
    pub fn artist_picture(
        &self,
        artist: &str,
        kind: PictureKind,
    ) -> Result<Option<Asked<Option<String>>>> {
        self.db
            .prepare_cached(
                "SELECT picture, asked FROM artist_pictures WHERE artist = ?1 AND kind = ?2",
            )?
            .query_row(params![artist, kind.name()], |row| {
                Ok(Asked {
                    answer: row.get(0)?,
                    asked: from_epoch_millis(row.get(1)?),
                })
            })
            .optional()
    }

    /// Notes that `source` was asked for the artist's picture at `asked`, and what it gave:
    /// the downloaded picture's SHA-256, or none.
    pub fn set_artist_picture(
        &self,
        artist: &str,
        kind: PictureKind,
        source: &str,
        picture: Option<&str>,
        asked: SystemTime,
    ) -> Result<()> {
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO artist_pictures (artist, kind, source, picture, asked)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![
                artist,
                kind.name(),
                source,
                picture,
                epoch_millis(asked)
            ])?;
        Ok(())
    }

    /// The downloaded pictures any artist has, to keep.
    pub fn artist_picture_checksums(&self) -> Result<HashSet<String>> {
        let mut statement = self
            .db
            .prepare("SELECT DISTINCT picture FROM artist_pictures WHERE picture IS NOT NULL")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect()
    }

    /// The MusicBrainz id its search found for the artist, if any; searched when.
    pub fn searched_artist_id(&self, artist: &str) -> Result<Option<Asked<Option<String>>>> {
        self.db
            .prepare_cached("SELECT mbid, searched FROM artist_ids WHERE artist = ?1")?
            .query_row([artist], |row| {
                Ok(Asked {
                    answer: row.get(0)?,
                    asked: from_epoch_millis(row.get(1)?),
                })
            })
            .optional()
    }

    pub fn set_searched_artist_id(
        &self,
        artist: &str,
        mbid: Option<&str>,
        searched: SystemTime,
    ) -> Result<()> {
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO artist_ids (artist, mbid, searched) VALUES (?1, ?2, ?3)",
            )?
            .execute(params![artist, mbid, epoch_millis(searched)])?;
        Ok(())
    }

    /// What Last.fm said of the artist; asked when.
    pub fn artist_text(&self, artist: &str) -> Result<Option<Asked<ArtistText>>> {
        self.db
            .prepare_cached(
                "SELECT summary, biography, tags, url, asked FROM artist_info WHERE artist = ?1",
            )?
            .query_row([artist], |row| {
                let tags: String = row.get(2)?;
                Ok(Asked {
                    answer: ArtistText {
                        summary: row.get(0)?,
                        biography: row.get(1)?,
                        tags: serde_json::from_str(&tags).unwrap_or_default(),
                        url: row.get(3)?,
                    },
                    asked: from_epoch_millis(row.get(4)?),
                })
            })
            .optional()
    }

    pub fn set_artist_text(
        &self,
        artist: &str,
        text: &ArtistText,
        asked: SystemTime,
    ) -> Result<()> {
        let tags = serde_json::to_string(&text.tags).unwrap_or_else(|_| "[]".to_owned());
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO artist_info (artist, summary, biography, tags, url, asked)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![
                artist,
                text.summary,
                text.biography,
                tags,
                text.url,
                epoch_millis(asked)
            ])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn notes_which_picture_is_whose_and_when_it_was_asked() {
        let library = Library::open_in_memory().unwrap();
        let portrait = PictureKind::Portrait;
        assert_eq!(library.artist_picture("Opeth", portrait).unwrap(), None);

        library
            .set_artist_picture("Opeth", portrait, "deezer", Some("aaa"), at(10))
            .unwrap();
        library
            .set_artist_picture("Opeth", PictureKind::Backdrop, "fanart.tv", None, at(20))
            .unwrap();
        // The same picture for two artists is kept once.
        library
            .set_artist_picture("Ghost", portrait, "deezer", Some("aaa"), at(30))
            .unwrap();
        let opeth = library.artist_picture("Opeth", portrait).unwrap().unwrap();
        assert_eq!(
            (opeth.answer.as_deref(), opeth.asked),
            (Some("aaa"), at(10))
        );
        assert!(!opeth.recent(), "asked in 1970");
        let backdrop = library
            .artist_picture("Opeth", PictureKind::Backdrop)
            .unwrap();
        assert_eq!(backdrop.unwrap().answer, None);
        assert_eq!(
            library.artist_picture_checksums().unwrap(),
            HashSet::from(["aaa".to_owned()])
        );

        // Asked again: the new answer stands.
        let now = SystemTime::now();
        library
            .set_artist_picture("Opeth", portrait, "deezer", Some("bbb"), now)
            .unwrap();
        let opeth = library.artist_picture("Opeth", portrait).unwrap().unwrap();
        assert_eq!(opeth.answer.as_deref(), Some("bbb"));
        assert!(opeth.recent());
    }

    #[test]
    fn keeps_musicbrainz_ids_and_what_last_fm_said() {
        let library = Library::open_in_memory().unwrap();
        assert_eq!(library.searched_artist_id("Opeth").unwrap(), None);
        library
            .set_searched_artist_id("Opeth", Some("c14b4180"), at(5))
            .unwrap();
        library
            .set_searched_artist_id("Nobody", None, at(6))
            .unwrap();
        let opeth = library.searched_artist_id("Opeth").unwrap().unwrap();
        assert_eq!(opeth.answer.as_deref(), Some("c14b4180"));
        assert_eq!(
            library
                .searched_artist_id("Nobody")
                .unwrap()
                .unwrap()
                .answer,
            None
        );

        let text = ArtistText {
            summary: "A Swedish band.".into(),
            biography: "A Swedish band from Stockholm.".into(),
            tags: vec!["progressive metal".into(), "swedish".into()],
            url: "https://www.last.fm/music/Opeth".into(),
        };
        library.set_artist_text("Opeth", &text, at(7)).unwrap();
        let kept = library.artist_text("Opeth").unwrap().unwrap();
        assert_eq!((kept.answer, kept.asked), (text, at(7)));
        assert_eq!(library.artist_text("Ghost").unwrap(), None);
    }
}
