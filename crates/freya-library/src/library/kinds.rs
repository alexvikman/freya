//! What kind of release each album is: album, single, EP or compilation. Its tracks' tags say
//! first, the kind most of them have; else MusicBrainz, asked by a scan for the albums whose
//! tags do not say, and kept apart, so a scan of the music never takes it away.

use rusqlite::{OptionalExtension, params};

use super::{Asked, Library, Result};
pub use freya_protocol::AlbumKind;

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS album_kinds (
        -- The album, as its tracks are filed: their album artist and album.
        album_artist TEXT NOT NULL,
        album TEXT NOT NULL,
        -- The kind MusicBrainz gives the release group found; NULL when none was found.
        kind TEXT,
        release_group TEXT,
        -- When MusicBrainz was asked, in milliseconds since 1970.
        asked INTEGER NOT NULL,
        PRIMARY KEY (album_artist, album)
    );
";

/// What MusicBrainz said an album was: of which release group, if it found one, and when.
pub type OnlineKind = (Option<String>, Asked<Option<AlbumKind>>);

/// An album whose tags do not say what kind it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Untyped {
    pub album_artist: String,
    pub album: String,
    /// The release group the user matched it to, if they did.
    pub matched: Option<String>,
}

impl Library {
    /// What kind of release each album of `artist` is, by the album's title: as most of its
    /// tracks' tags say, else as MusicBrainz said. Albums neither says anything of are left
    /// out: they are albums.
    pub fn album_kinds(&self, artist: Option<&str>) -> Result<Vec<(Option<String>, AlbumKind)>> {
        let mut query = self.db.prepare_cached(
            "SELECT a.album,
                    (SELECT t.album_kind FROM tracks t
                     WHERE t.album_artist IS ?1 AND t.album IS a.album
                           AND t.album_kind IS NOT NULL
                     GROUP BY t.album_kind ORDER BY count(*) DESC, t.album_kind LIMIT 1),
                    (SELECT k.kind FROM album_kinds k
                     WHERE k.album_artist IS ?1 AND k.album IS a.album)
             FROM (SELECT DISTINCT album FROM tracks WHERE album_artist IS ?1) a",
        )?;
        let rows = query.query_map([artist], |row| {
            let tagged: Option<String> = row.get(1)?;
            let found: Option<String> = row.get(2)?;
            Ok((row.get::<_, Option<String>>(0)?, tagged.or(found)))
        })?;
        let mut kinds = Vec::new();
        for row in rows {
            let (album, kind) = row?;
            if let Some(kind) = kind.as_deref().and_then(AlbumKind::from_name) {
                kinds.push((album, kind));
            }
        }
        Ok(kinds)
    }

    /// The albums, with an artist and a title, whose tags do not say what kind they are.
    pub fn untyped_albums(&self) -> Result<Vec<Untyped>> {
        let mut query = self.db.prepare_cached(
            "SELECT t.album_artist, t.album,
                    (SELECT m.release_group FROM album_matches m
                     WHERE ifnull(m.artist_key, m.album_artist) = t.album_artist
                           AND m.album = t.album)
             FROM tracks t
             WHERE t.album_artist IS NOT NULL AND t.album IS NOT NULL
             GROUP BY t.album_artist, t.album
             HAVING count(t.album_kind) = 0
             ORDER BY t.album_artist, t.album",
        )?;
        query
            .query_map([], |row| {
                Ok(Untyped {
                    album_artist: row.get(0)?,
                    album: row.get(1)?,
                    matched: row.get(2)?,
                })
            })?
            .collect()
    }

    /// What MusicBrainz said the album was, of which release group, and when it was asked.
    pub fn album_online_kind(
        &self,
        album_artist: &str,
        album: &str,
    ) -> Result<Option<OnlineKind>> {
        self.db
            .prepare_cached(
                "SELECT release_group, kind, asked FROM album_kinds
                 WHERE album_artist = ?1 AND album = ?2",
            )?
            .query_row([album_artist, album], |row| {
                let kind: Option<String> = row.get(1)?;
                Ok((
                    row.get(0)?,
                    Asked {
                        answer: kind.as_deref().and_then(AlbumKind::from_name),
                        asked: super::from_epoch_millis(row.get(2)?),
                    },
                ))
            })
            .optional()
    }

    /// Keeps what MusicBrainz said the album was: of the release group `group`, or that it
    /// found none; asked at `asked`.
    pub fn set_album_online_kind(
        &self,
        album_artist: &str,
        album: &str,
        group: Option<&str>,
        kind: Option<AlbumKind>,
        asked: std::time::SystemTime,
    ) -> Result<()> {
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO album_kinds (album_artist, album, kind, release_group, asked)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![
                album_artist,
                album,
                kind.map(AlbumKind::name),
                group,
                super::epoch_millis(asked)
            ])?;
        Ok(())
    }
}
