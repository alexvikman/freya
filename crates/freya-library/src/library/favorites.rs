//! The albums the user made favourites, and when: a favourite is as recently added as when it
//! was made one, so it comes first among the albums by when they came, and stays in that
//! order. Not a pin: it does not keep it first.

use std::collections::HashMap;
use std::time::SystemTime;

use rusqlite::params;

use super::{Album, Library, Result, album_from_row, epoch_millis, from_epoch_millis};

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS album_favorites (
        -- The album as it is filed: its album artist and title, '' for none.
        album_artist TEXT NOT NULL,
        album TEXT NOT NULL,
        -- When it was made a favourite, in milliseconds since 1970.
        at INTEGER NOT NULL,
        PRIMARY KEY (album_artist, album)
    );
";

/// How the albums are ordered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlbumOrder {
    /// By when they came into the library, or were made favourites, the latest first.
    #[default]
    Added,
    /// By when they were last played, the latest first; those never played last.
    Played,
    /// By title.
    Name,
    /// By artist, then year.
    Artist,
}

/// An album as its favourite is kept: its album artist and title.
pub type AlbumKey = (Option<String>, Option<String>);

impl Library {
    /// Makes the album `artist` / `album` a favourite at `at`, or not one.
    pub fn set_album_favorite(
        &self,
        artist: Option<&str>,
        album: Option<&str>,
        favorite: bool,
        at: SystemTime,
    ) -> Result<()> {
        let (artist, album) = (artist.unwrap_or(""), album.unwrap_or(""));
        if favorite {
            self.db
                .prepare_cached(
                    "INSERT OR REPLACE INTO album_favorites (album_artist, album, at)
                     VALUES (?1, ?2, ?3)",
                )?
                .execute(params![artist, album, epoch_millis(at)])?;
        } else {
            self.db
                .prepare_cached(
                    "DELETE FROM album_favorites WHERE album_artist = ?1 AND album = ?2",
                )?
                .execute(params![artist, album])?;
        }
        Ok(())
    }

    /// Every favourite album, and when it was made one.
    pub fn album_favorites(&self) -> Result<HashMap<AlbumKey, SystemTime>> {
        let mut statement = self
            .db
            .prepare_cached("SELECT album_artist, album, at FROM album_favorites")?;
        let rows = statement.query_map([], |row| {
            let none = |text: String| (!text.is_empty()).then_some(text);
            Ok((
                (none(row.get(0)?), none(row.get(1)?)),
                from_epoch_millis(row.get(2)?),
            ))
        })?;
        rows.collect()
    }

    /// Every album, as `order` says.
    pub fn albums_by(&self, order: AlbumOrder) -> Result<Vec<Album>> {
        let order = match order {
            AlbumOrder::Added => "coalesce(f.at, max(t.added)) DESC, t.album COLLATE NOCASE",
            AlbumOrder::Played => {
                "max(p.last) IS NULL, max(p.last) DESC, max(t.added) DESC, t.album COLLATE NOCASE"
            }
            AlbumOrder::Name => {
                "t.album IS NULL, t.album COLLATE NOCASE, t.album_artist COLLATE NOCASE"
            }
            AlbumOrder::Artist => {
                "t.album_artist IS NULL, t.album_artist COLLATE NOCASE, min(t.year),
                 t.album COLLATE NOCASE"
            }
        };
        let mut query = self.db.prepare_cached(&format!(
            "SELECT t.album_artist, t.album, min(t.year), count(*), min(t.path)
             FROM tracks t
             LEFT JOIN album_favorites f
                 ON f.album_artist = ifnull(t.album_artist, '') AND f.album = ifnull(t.album, '')
             LEFT JOIN (SELECT path, max(played) AS last FROM plays GROUP BY path) p
                 ON p.path = t.path
             GROUP BY t.album_artist, t.album
             ORDER BY {order}"
        ))?;
        query.query_map([], album_from_row)?.collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::*;

    fn names(albums: &[Album]) -> Vec<&str> {
        albums.iter().filter_map(|a| a.name.as_deref()).collect()
    }

    #[test]
    fn a_favourite_is_as_recent_as_when_it_was_made_one() {
        let mut library = Library::open_in_memory().unwrap();
        let at = |s: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(s);
        for (i, album) in ["Old", "Middle", "New"].iter().enumerate() {
            let mut tags = crate::tags::TagInfo {
                artist: Some("A".into()),
                album: Some((*album).into()),
                title: Some("Track".into()),
                ..Default::default()
            };
            tags.track = Some(1);
            let tx = library.transaction().unwrap();
            let path = format!("/m/{album}.mp3");
            super::super::put(
                &tx,
                Path::new(&path),
                super::super::FileStamp {
                    size: 0,
                    modified: 0,
                },
                (i as i64 + 1) * 1000,
                &super::super::NewTrack {
                    path: path.clone().into(),
                    part: None,
                    tags,
                    fingerprint: None,
                },
            )
            .unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            names(&library.albums_by(AlbumOrder::Added).unwrap()),
            ["New", "Middle", "Old"]
        );

        library
            .set_album_favorite(Some("A"), Some("Old"), true, at(10))
            .unwrap();
        assert_eq!(
            names(&library.albums_by(AlbumOrder::Added).unwrap()),
            ["Old", "New", "Middle"]
        );
        assert!(
            library
                .album_favorites()
                .unwrap()
                .contains_key(&(Some("A".into()), Some("Old".into())))
        );

        // Played: the one played last first, those never played after.
        library
            .record_play(Path::new("/m/Middle.mp3"), None, at(20))
            .unwrap();
        assert_eq!(
            names(&library.albums_by(AlbumOrder::Played).unwrap())[0],
            "Middle"
        );
        assert_eq!(
            names(&library.albums_by(AlbumOrder::Name).unwrap()),
            ["Middle", "New", "Old"]
        );

        library
            .set_album_favorite(Some("A"), Some("Old"), false, at(30))
            .unwrap();
        assert!(library.album_favorites().unwrap().is_empty());
    }
}
