//! What Freya keeps of the user's playlists beside their files, which hold the tracks: a
//! description, a picture and a wide picture of Freya's own (see [`crate::pictures`]), and
//! when each track was added. Kept by the playlist's name, as its file is named.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

use rusqlite::{OptionalExtension, params};

use super::{Library, Result, epoch_millis, from_epoch_millis};

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS playlist_info (
        playlist TEXT NOT NULL PRIMARY KEY,
        description TEXT,
        -- Pictures among Freya's own, by their SHA-256.
        picture TEXT,
        backdrop TEXT
    );
    CREATE TABLE IF NOT EXISTS playlist_added (
        playlist TEXT NOT NULL,
        path TEXT NOT NULL,
        -- Where a track of a CUE sheet starts, in milliseconds; 0 for a whole file.
        start_ms INTEGER NOT NULL,
        -- When it was added, in milliseconds since 1970.
        added INTEGER NOT NULL,
        PRIMARY KEY (playlist, path, start_ms)
    );
";

/// What a playlist has beside its tracks; each `None` when the user gave none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaylistInfo {
    pub description: Option<String>,
    pub picture: Option<String>,
    pub backdrop: Option<String>,
}

/// A track in a playlist: its file, and where in it it starts, in milliseconds.
pub type PlaylistTrack = (PathBuf, i64);

impl Library {
    pub fn playlist_info(&self, playlist: &str) -> Result<PlaylistInfo> {
        Ok(self
            .db
            .prepare_cached(
                "SELECT description, picture, backdrop FROM playlist_info WHERE playlist = ?1",
            )?
            .query_row([playlist], |row| {
                Ok(PlaylistInfo {
                    description: row.get(0)?,
                    picture: row.get(1)?,
                    backdrop: row.get(2)?,
                })
            })
            .optional()?
            .unwrap_or_default())
    }

    /// Keeps what `playlist` has beside its tracks; with nothing, its row goes.
    pub fn set_playlist_info(&self, playlist: &str, info: &PlaylistInfo) -> Result<()> {
        let description = info
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty());
        if description.is_none() && info.picture.is_none() && info.backdrop.is_none() {
            self.db
                .prepare_cached("DELETE FROM playlist_info WHERE playlist = ?1")?
                .execute([playlist])?;
            return Ok(());
        }
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO playlist_info (playlist, description, picture, backdrop)
                 VALUES (?1, ?2, ?3, ?4)",
            )?
            .execute(params![playlist, description, info.picture, info.backdrop])?;
        Ok(())
    }

    /// Keeps what the playlist `from` has under its new name `to`.
    pub fn rename_playlist(&mut self, from: &str, to: &str) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute(
            "UPDATE playlist_info SET playlist = ?2 WHERE playlist = ?1",
            params![from, to],
        )?;
        tx.execute(
            "UPDATE playlist_added SET playlist = ?2 WHERE playlist = ?1",
            params![from, to],
        )?;
        tx.execute(
            "UPDATE playlist_favorites SET playlist = ?2 WHERE playlist = ?1",
            params![from, to],
        )?;
        tx.commit()
    }

    /// Forgets what the playlist had beside its tracks: it was deleted.
    pub fn forget_playlist(&mut self, playlist: &str) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM playlist_info WHERE playlist = ?1", [playlist])?;
        tx.execute("DELETE FROM playlist_added WHERE playlist = ?1", [playlist])?;
        tx.execute(
            "DELETE FROM playlist_favorites WHERE playlist = ?1",
            [playlist],
        )?;
        tx.commit()
    }

    /// When each track of `playlist` was added, those noted.
    pub fn playlist_added(&self, playlist: &str) -> Result<HashMap<PlaylistTrack, SystemTime>> {
        let mut statement = self.db.prepare_cached(
            "SELECT path, start_ms, added FROM playlist_added WHERE playlist = ?1",
        )?;
        let rows = statement.query_map([playlist], |row| {
            let path: String = row.get(0)?;
            Ok((
                (PathBuf::from(path), row.get(1)?),
                from_epoch_millis(row.get(2)?),
            ))
        })?;
        rows.collect()
    }

    /// Notes that `tracks` were added to `playlist` at `at`, those not noted before.
    pub fn note_playlist_added(
        &mut self,
        playlist: &str,
        tracks: &[PlaylistTrack],
        at: SystemTime,
    ) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT OR IGNORE INTO playlist_added (playlist, path, start_ms, added)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (path, start) in tracks {
                insert.execute(params![
                    playlist,
                    path.to_string_lossy(),
                    start,
                    epoch_millis(at)
                ])?;
            }
        }
        tx.commit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_what_a_playlist_has_beside_its_tracks() {
        let mut library = Library::open_in_memory().unwrap();
        assert_eq!(
            library.playlist_info("Mix").unwrap(),
            PlaylistInfo::default()
        );
        let info = PlaylistInfo {
            description: Some(" Songs for the night. ".into()),
            picture: Some("aa".into()),
            backdrop: None,
        };
        library.set_playlist_info("Mix", &info).unwrap();
        assert_eq!(
            library.playlist_info("Mix").unwrap().description.as_deref(),
            Some("Songs for the night.")
        );
        assert!(library.artist_picture_checksums().unwrap().contains("aa"));

        let at = |s: u64| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(s);
        let track = (PathBuf::from("/m/a.flac"), 0);
        library
            .note_playlist_added("Mix", std::slice::from_ref(&track), at(100))
            .unwrap();
        library
            .note_playlist_added("Mix", std::slice::from_ref(&track), at(200))
            .unwrap();
        assert_eq!(
            library.playlist_added("Mix").unwrap()[&track],
            at(100),
            "the first time"
        );

        library.rename_playlist("Mix", "Night").unwrap();
        assert_eq!(
            library.playlist_info("Night").unwrap().picture.as_deref(),
            Some("aa")
        );
        assert_eq!(library.playlist_added("Night").unwrap().len(), 1);
        library.forget_playlist("Night").unwrap();
        assert_eq!(
            library.playlist_info("Night").unwrap(),
            PlaylistInfo::default()
        );
        assert!(library.playlist_added("Night").unwrap().is_empty());
    }
}
