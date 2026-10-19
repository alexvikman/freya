//! What the internet said of the library's artists: which of the downloaded pictures is
//! theirs (see [`crate::pictures`]), their MusicBrainz id, and what Last.fm says of them. Kept
//! with the user's plays, as getting them again takes the internet: never dropped.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use rusqlite::{OptionalExtension, params};

use super::{Library, Result, epoch_millis, from_epoch_millis};

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS artist_pictures (
        artist TEXT NOT NULL,
        -- 'portrait' or 'backdrop'.
        kind TEXT NOT NULL,
        -- Where it was asked for: 'deezer' or 'fanart.tv'; or 'user' when the user chose it,
        -- which the internet's answers never replace.
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
    -- Their Wikipedia article's opening, by the MusicBrainz id it was found by; title NULL
    -- when there is none.
    CREATE TABLE IF NOT EXISTS artist_articles (
        artist TEXT NOT NULL PRIMARY KEY,
        mbid TEXT NOT NULL,
        title TEXT,
        summary TEXT NOT NULL,
        biography TEXT NOT NULL,
        url TEXT NOT NULL,
        asked INTEGER NOT NULL
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

/// The source of a picture the user chose themselves.
pub const CHOSEN: &str = "user";
/// The source of a picture a scan found in the artist's folder.
pub const FOLDER: &str = "folder";

/// Which downloaded picture is an artist's, where from, and when it was asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct PictureRow {
    /// Its SHA-256 among the downloaded pictures; none when the source had none, or the user
    /// chose none.
    pub picture: Option<String>,
    pub source: String,
    pub asked: SystemTime,
}

impl PictureRow {
    /// Whether the user chose it.
    pub fn chosen(&self) -> bool {
        self.source == CHOSEN
    }

    /// Whether it is recent enough not to ask again.
    pub fn recent(&self) -> bool {
        self.asked.elapsed().is_ok_and(|age| age < ASK_AGAIN_AFTER)
    }

    /// Whether it came from the internet: neither the user's choice nor their folder's.
    pub fn from_internet(&self) -> bool {
        self.source != CHOSEN && self.source != FOLDER
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
    /// Which downloaded picture is the artist's, if one; where from, and when.
    pub fn artist_picture(&self, artist: &str, kind: PictureKind) -> Result<Option<PictureRow>> {
        self.db
            .prepare_cached(
                "SELECT picture, source, asked FROM artist_pictures
                 WHERE artist = ?1 AND kind = ?2",
            )?
            .query_row(params![artist, kind.name()], |row| {
                Ok(PictureRow {
                    picture: row.get(0)?,
                    source: row.get(1)?,
                    asked: from_epoch_millis(row.get(2)?),
                })
            })
            .optional()
    }

    /// Every artist's picture of `kind`, by the artist: those who have one.
    pub fn artist_pictures(&self, kind: PictureKind) -> Result<HashMap<String, String>> {
        let mut statement = self.db.prepare_cached(
            "SELECT artist, picture FROM artist_pictures WHERE kind = ?1 AND picture IS NOT NULL",
        )?;
        let rows = statement.query_map([kind.name()], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// Notes that `source` was asked for the artist's picture at `asked`, and what it gave:
    /// the picture's SHA-256 among Freya's pictures, or none. Once an artist has a picture it
    /// stays theirs: only the user's choice, [`CHOSEN`], replaces it; another source only fills
    /// in a picture they lack.
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
                "INSERT INTO artist_pictures (artist, kind, source, picture, asked)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (artist, kind) DO UPDATE
                 SET source = ?3, picture = ?4, asked = ?5
                 WHERE ?3 = 'user'
                    OR (artist_pictures.source != 'user' AND artist_pictures.picture IS NULL)",
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

    /// Forgets what was asked of the artist's picture, so it is asked again.
    pub fn forget_artist_picture(&self, artist: &str, kind: PictureKind) -> Result<()> {
        self.db
            .prepare_cached("DELETE FROM artist_pictures WHERE artist = ?1 AND kind = ?2")?
            .execute(params![artist, kind.name()])?;
        Ok(())
    }

    /// The pictures any artist, album or playlist has, to keep.
    pub fn artist_picture_checksums(&self) -> Result<HashSet<String>> {
        let mut statement = self.db.prepare(
            "SELECT picture FROM artist_pictures WHERE picture IS NOT NULL
             UNION SELECT cover FROM album_covers WHERE cover IS NOT NULL
             UNION SELECT picture FROM playlist_info WHERE picture IS NOT NULL
             UNION SELECT backdrop FROM playlist_info WHERE backdrop IS NOT NULL",
        )?;
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

    /// The artist's Wikipedia article, or that they have none, found by the MusicBrainz id
    /// given with it; asked when.
    pub fn artist_article(
        &self,
        artist: &str,
    ) -> Result<Option<(String, Asked<Option<crate::wikipedia::Article>>)>> {
        self.db
            .prepare_cached(
                "SELECT mbid, title, summary, biography, url, asked FROM artist_articles
                 WHERE artist = ?1",
            )?
            .query_row([artist], |row| {
                let title: Option<String> = row.get(1)?;
                let article = match title {
                    Some(title) => Some(crate::wikipedia::Article {
                        title,
                        summary: row.get(2)?,
                        biography: row.get(3)?,
                        url: row.get(4)?,
                    }),
                    None => None,
                };
                Ok((
                    row.get(0)?,
                    Asked {
                        answer: article,
                        asked: from_epoch_millis(row.get(5)?),
                    },
                ))
            })
            .optional()
    }

    pub fn set_artist_article(
        &self,
        artist: &str,
        mbid: &str,
        article: Option<&crate::wikipedia::Article>,
        asked: SystemTime,
    ) -> Result<()> {
        let text = |f: fn(&crate::wikipedia::Article) -> &str| article.map_or("", f);
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO artist_articles
                     (artist, mbid, title, summary, biography, url, asked)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?
            .execute(params![
                artist,
                mbid,
                article.map(|a| a.title.as_str()),
                text(|a| &a.summary),
                text(|a| &a.biography),
                text(|a| &a.url),
                epoch_millis(asked)
            ])?;
        Ok(())
    }

    /// Forgets what Last.fm said of the artist, so it is asked again.
    pub fn forget_artist_text(&self, artist: &str) -> Result<()> {
        self.db
            .prepare_cached("DELETE FROM artist_info WHERE artist = ?1")?
            .execute([artist])?;
        Ok(())
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
            (opeth.picture.as_deref(), opeth.asked),
            (Some("aaa"), at(10))
        );
        assert!(!opeth.recent(), "asked in 1970");
        let backdrop = library
            .artist_picture("Opeth", PictureKind::Backdrop)
            .unwrap();
        assert_eq!(backdrop.unwrap().picture, None);
        assert_eq!(
            library.artist_picture_checksums().unwrap(),
            HashSet::from(["aaa".to_owned()])
        );

        // Once theirs, a picture stays: another answer does not replace it.
        let now = SystemTime::now();
        library
            .set_artist_picture("Opeth", portrait, "deezer", Some("bbb"), now)
            .unwrap();
        let opeth = library.artist_picture("Opeth", portrait).unwrap().unwrap();
        assert_eq!(opeth.picture.as_deref(), Some("aaa"));
        // But fills in one they lack.
        library
            .set_artist_picture(
                "Opeth",
                PictureKind::Backdrop,
                "fanart.tv",
                Some("wide"),
                now,
            )
            .unwrap();
        let wide = library
            .artist_picture("Opeth", PictureKind::Backdrop)
            .unwrap()
            .unwrap();
        assert_eq!(wide.picture.as_deref(), Some("wide"));
        assert!(wide.recent());

        // The user's choice replaces any, and stays.
        library
            .set_artist_picture("Opeth", portrait, CHOSEN, None, now)
            .unwrap();
        library
            .set_artist_picture("Opeth", portrait, "deezer", Some("ccc"), now)
            .unwrap();
        let opeth = library.artist_picture("Opeth", portrait).unwrap().unwrap();
        assert!(opeth.chosen());
        assert_eq!(opeth.picture, None);
        library
            .set_artist_picture("Opeth", portrait, CHOSEN, Some("ddd"), now)
            .unwrap();
        let opeth = library.artist_picture("Opeth", portrait).unwrap().unwrap();
        assert_eq!(opeth.picture.as_deref(), Some("ddd"));
    }

    #[test]
    fn keeps_wikipedia_articles_by_the_id_they_were_found_by() {
        let library = Library::open_in_memory().unwrap();
        assert_eq!(library.artist_article("Shining").unwrap(), None);
        let article = crate::wikipedia::Article {
            title: "Shining (Norwegian band)".into(),
            summary: "A Norwegian band.".into(),
            biography: "A Norwegian band. From Oslo.".into(),
            url: "https://en.wikipedia.org/wiki/Shining_(Norwegian_band)".into(),
        };
        library
            .set_artist_article("Shining", "no", Some(&article), at(3))
            .unwrap();
        let (mbid, kept) = library.artist_article("Shining").unwrap().unwrap();
        assert_eq!(
            (mbid.as_str(), kept.answer, kept.asked),
            ("no", Some(article), at(3))
        );
        library
            .set_artist_article("Opeth", "op", None, at(4))
            .unwrap();
        assert_eq!(
            library.artist_article("Opeth").unwrap().unwrap().1.answer,
            None
        );
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
