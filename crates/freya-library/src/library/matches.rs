//! Albums the user matched to the artist in MusicBrainz who made them. Matched to another
//! artist than the one their tags name, maybe one of the same name, an album is filed under an
//! artist of its own: [`split_artist`], the name with that artist's id. So two bands called
//! Shining are two artists. Each match holds through scans; the files are left alone.

use rusqlite::{OptionalExtension, params};

use super::{Library, Result};

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS album_matches (
        -- The album as its tags name it.
        album_artist TEXT NOT NULL,
        album TEXT NOT NULL,
        -- MusicBrainz's ids of the album (its release group) and of the artist who made it.
        release_group TEXT,
        artist_mbid TEXT NOT NULL,
        -- The artist it is filed under, when not the one its tags name.
        artist_key TEXT,
        PRIMARY KEY (album_artist, album)
    );
";

/// Between an artist's name and the MusicBrainz id in the name of an artist split off.
const SPLIT: char = '\u{1f}';

/// The artist an album is filed under once matched to `mbid`, another artist than the one
/// its tags name `name`.
pub fn split_artist(name: &str, mbid: &str) -> String {
    format!("{name}{SPLIT}{mbid}")
}

/// An artist's name as their name in the library has it: without the id of one split off.
pub fn plain_name(artist: &str) -> &str {
    artist.split(SPLIT).next().unwrap_or(artist)
}

/// An album matched to the artist in MusicBrainz who made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumMatch {
    /// The album as its tags name it.
    pub album_artist: String,
    pub album: String,
    pub release_group: Option<String>,
    pub artist_mbid: String,
    /// The artist it is filed under, when not `album_artist`.
    pub artist_key: Option<String>,
}

impl Library {
    /// Keeps `album`'s match, and files its tracks under the artist it says.
    pub fn match_album(&mut self, album: &AlbumMatch) -> Result<()> {
        let tx = self.db.transaction()?;
        let before = filed_under(&tx, &album.album_artist, &album.album)?;
        let filed = album.artist_key.as_deref().unwrap_or(&album.album_artist);
        tx.execute(
            "UPDATE tracks SET album_artist = ?1
             WHERE album IS ?2 AND (album_artist IS ?3 OR (?4 IS NOT NULL AND album_artist = ?4))",
            params![filed, album.album, album.album_artist, before],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO album_matches
                 (album_artist, album, release_group, artist_mbid, artist_key)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                album.album_artist,
                album.album,
                album.release_group,
                album.artist_mbid,
                album.artist_key
            ],
        )?;
        tx.commit()
    }

    /// Forgets `album`'s match, by its tags' names, and files it under the artist they name
    /// again.
    pub fn unmatch_album(&mut self, album_artist: &str, album: &str) -> Result<()> {
        let tx = self.db.transaction()?;
        if let Some(before) = filed_under(&tx, album_artist, album)? {
            tx.execute(
                "UPDATE tracks SET album_artist = ?1 WHERE album IS ?2 AND album_artist = ?3",
                params![album_artist, album, before],
            )?;
        }
        tx.execute(
            "DELETE FROM album_matches WHERE album_artist = ?1 AND album = ?2",
            params![album_artist, album],
        )?;
        tx.commit()
    }

    /// The match of the album named `album` filed under `artist`, if it has one.
    pub fn album_match(&self, artist: &str, album: &str) -> Result<Option<AlbumMatch>> {
        self.db
            .prepare_cached(
                "SELECT album_artist, album, release_group, artist_mbid, artist_key
                 FROM album_matches
                 WHERE album = ?2 AND (artist_key = ?1 OR (artist_key IS NULL AND album_artist = ?1))",
            )?
            .query_row(params![artist, album], |row| {
                Ok(AlbumMatch {
                    album_artist: row.get(0)?,
                    album: row.get(1)?,
                    release_group: row.get(2)?,
                    artist_mbid: row.get(3)?,
                    artist_key: row.get(4)?,
                })
            })
            .optional()
    }
}

impl Library {
    /// Every album's match.
    pub fn album_matches(&self) -> Result<Vec<AlbumMatch>> {
        let mut statement = self.db.prepare_cached(
            "SELECT album_artist, album, release_group, artist_mbid, artist_key FROM album_matches",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(AlbumMatch {
                album_artist: row.get(0)?,
                album: row.get(1)?,
                release_group: row.get(2)?,
                artist_mbid: row.get(3)?,
                artist_key: row.get(4)?,
            })
        })?;
        rows.collect()
    }
}

/// The artist a matched album is filed under instead of `album_artist`, its tags' name; none
/// for an album not matched to another.
pub(super) fn filed_under(
    db: &rusqlite::Connection,
    album_artist: &str,
    album: &str,
) -> Result<Option<String>> {
    Ok(db
        .prepare_cached(
            "SELECT artist_key FROM album_matches WHERE album_artist = ?1 AND album = ?2",
        )?
        .query_row(params![album_artist, album], |row| row.get(0))
        .optional()?
        .flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_split_artists_apart() {
        let split = split_artist("Shining", "b16e3960");
        assert_ne!(split, "Shining");
        assert_eq!(plain_name(&split), "Shining");
        assert_eq!(plain_name("Shining"), "Shining");
    }
}
