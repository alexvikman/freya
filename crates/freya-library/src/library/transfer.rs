//! What the users made with the library, read out to be exported, and taken in from another's
//! export: plays, favourites, what playlists say of themselves and when tracks were added to
//! them, and what was changed of artists, albums and genres. Tracks are found again by their
//! path, by their path within a music folder, or by their tags.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use freya_protocol::{
    AlbumMatchRow, ArtistEditRow, ArtistPictureRow, FavoriteRow, GenreInfoRow, Made, PlayRow,
    PlaylistAddedRow, PlaylistInfoRow, StationPlayRow, TrackRef,
};
use rusqlite::{OptionalExtension, params};

use super::{Library, Result, epoch_millis, from_epoch_millis};

/// A track of this library: its path and where it starts, as its rows keep them.
pub type Found = (String, Option<i64>);

/// What an import took in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Taken {
    pub plays: usize,
    pub favorites: usize,
    pub edits: usize,
}

impl Library {
    /// What the users made, every user's, their tracks named for another library to find:
    /// within `folders`, the library's music folders.
    pub fn made(&self, folders: &[PathBuf]) -> Result<Made> {
        let mut made = Made::default();
        let mut query = self.db.prepare(
            "SELECT plays.path, plays.start_ms, plays.played, plays.user, plays.playlist,
                    t.album_artist, t.album, t.title, t.track, t.disc
             FROM plays LEFT JOIN tracks AS t
                  ON t.path = plays.path AND t.start_ms IS plays.start_ms
             ORDER BY plays.played",
        )?;
        made.plays = query
            .query_map([], |row| {
                Ok(PlayRow {
                    track: track_ref(row, 0, 1, 5, folders)?,
                    at: from_epoch_millis(row.get(2)?),
                    user: row.get(3)?,
                    playlist: row.get(4)?,
                })
            })?
            .collect::<Result<_>>()?;
        let mut query = self
            .db
            .prepare("SELECT user, name, url, played FROM station_plays ORDER BY played")?;
        made.station_plays = query
            .query_map([], |row| {
                Ok(StationPlayRow {
                    user: row.get(0)?,
                    name: row.get(1)?,
                    url: row.get(2)?,
                    at: from_epoch_millis(row.get(3)?),
                })
            })?
            .collect::<Result<_>>()?;
        for (table, kind, columns) in [
            ("album_favorites", "album", "album_artist, album"),
            ("artist_favorites", "artist", "artist, ''"),
            ("playlist_favorites", "playlist", "playlist, ''"),
        ] {
            let mut query = self
                .db
                .prepare(&format!("SELECT user, {columns}, at FROM {table}"))?;
            made.favorites.extend(
                query
                    .query_map([], |row| {
                        Ok(FavoriteRow {
                            user: row.get(0)?,
                            kind: kind.to_owned(),
                            name: row.get(1)?,
                            album: row.get(2)?,
                            at: from_epoch_millis(row.get(3)?),
                        })
                    })?
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        let mut query = self
            .db
            .prepare("SELECT user, playlist, description, picture, backdrop FROM playlist_info")?;
        made.playlist_info = query
            .query_map([], |row| {
                Ok(PlaylistInfoRow {
                    user: row.get(0)?,
                    playlist: row.get(1)?,
                    description: row.get(2)?,
                    picture: row.get(3)?,
                    backdrop: row.get(4)?,
                })
            })?
            .collect::<Result<_>>()?;
        let mut query = self.db.prepare(
            "SELECT added.path, nullif(added.start_ms, 0), added.added, added.user,
                    added.playlist, t.album_artist, t.album, t.title, t.track, t.disc
             FROM playlist_added AS added LEFT JOIN tracks AS t
                  ON t.path = added.path AND ifnull(t.start_ms, 0) = added.start_ms",
        )?;
        made.playlist_added = query
            .query_map([], |row| {
                Ok(PlaylistAddedRow {
                    track: track_ref(row, 0, 1, 5, folders)?,
                    added: from_epoch_millis(row.get(2)?),
                    user: row.get(3)?,
                    playlist: row.get(4)?,
                })
            })?
            .collect::<Result<_>>()?;
        let mut query = self.db.prepare(
            "SELECT artist, name, sort_name, mbid, biography, note, header FROM artist_edits",
        )?;
        made.artist_edits = query
            .query_map([], |row| {
                Ok(ArtistEditRow {
                    artist: row.get(0)?,
                    name: row.get(1)?,
                    sort_name: row.get(2)?,
                    mbid: row.get(3)?,
                    biography: row.get(4)?,
                    note: row.get(5)?,
                    header: row.get(6)?,
                })
            })?
            .collect::<Result<_>>()?;
        let mut query = self.db.prepare(
            "SELECT album_artist, album, release_group, artist_mbid, artist_key FROM album_matches",
        )?;
        made.album_matches = query
            .query_map([], |row| {
                Ok(AlbumMatchRow {
                    album_artist: row.get(0)?,
                    album: row.get(1)?,
                    release_group: row.get(2)?,
                    artist_mbid: row.get(3)?,
                    artist_key: row.get(4)?,
                })
            })?
            .collect::<Result<_>>()?;
        let mut query = self
            .db
            .prepare("SELECT genre, picture, backdrop, description FROM genre_info")?;
        made.genre_info = query
            .query_map([], |row| {
                Ok(GenreInfoRow {
                    genre: row.get(0)?,
                    picture: row.get(1)?,
                    backdrop: row.get(2)?,
                    description: row.get(3)?,
                })
            })?
            .collect::<Result<_>>()?;
        // The pictures the user chose, not those the internet gave.
        let mut query = self.db.prepare(
            "SELECT artist, kind, picture FROM artist_pictures
             WHERE source = 'user' AND picture IS NOT NULL",
        )?;
        made.artist_pictures = query
            .query_map([], |row| {
                Ok(ArtistPictureRow {
                    artist: row.get(0)?,
                    kind: row.get(1)?,
                    picture: row.get(2)?,
                })
            })?
            .collect::<Result<_>>()?;
        Ok(made)
    }

    /// The track at `path` from `start_ms`, named for another library to find.
    pub fn track_ref(
        &self,
        path: &Path,
        start_ms: Option<i64>,
        folders: &[PathBuf],
    ) -> Result<TrackRef> {
        let path_text = path.to_string_lossy();
        let tags = self
            .db
            .prepare_cached(
                "SELECT album_artist, album, title, track, disc FROM tracks
                 WHERE path = ?1 AND start_ms IS ?2 LIMIT 1",
            )?
            .query_row(params![path_text, start_ms], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .optional()?;
        let (album_artist, album, title, track, disc) = tags.unwrap_or_default();
        Ok(TrackRef {
            path: path.to_owned(),
            within: within(path, folders),
            start_ms,
            album_artist,
            album,
            title,
            track,
            disc,
        })
    }

    /// This library's track of `track`, another's: at the same path; at the same path within
    /// one of `folders`; or with the same album artist, album and title (and number, when both
    /// have one), whatever their case.
    pub fn find_track(&self, track: &TrackRef, folders: &[PathBuf]) -> Result<Option<Found>> {
        let at = |path: &str| -> Result<Option<Found>> {
            self.db
                .prepare_cached(
                    "SELECT path, start_ms FROM tracks
                     WHERE path = ?1 AND ifnull(start_ms, 0) = ifnull(?2, 0) LIMIT 1",
                )?
                .query_row(params![path, track.start_ms], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()
        };
        if let Some(found) = at(&track.path.to_string_lossy())? {
            return Ok(Some(found));
        }
        if let Some(within) = &track.within {
            for folder in folders {
                let path = within
                    .split('/')
                    .fold(folder.clone(), |path, part| path.join(part));
                if let Some(found) = at(&path.to_string_lossy())? {
                    return Ok(Some(found));
                }
            }
        }
        if track.title.is_none() {
            return Ok(None);
        }
        self.db
            .prepare_cached(
                "SELECT path, start_ms FROM tracks
                 WHERE album_artist IS ?1 COLLATE NOCASE AND album IS ?2 COLLATE NOCASE
                   AND title IS ?3 COLLATE NOCASE
                   AND (?4 IS NULL OR track IS NULL OR track = ?4)
                 ORDER BY (track IS ?4) DESC, (disc IS ?5) DESC, path
                 LIMIT 1",
            )?
            .query_row(
                params![
                    track.album_artist,
                    track.album,
                    track.title,
                    track.track,
                    track.disc
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// This library's tracks of `tracks`, each found as [`Self::find_track`] finds it, once.
    pub fn find_tracks<'a>(
        &self,
        tracks: impl IntoIterator<Item = &'a TrackRef>,
        folders: &[PathBuf],
    ) -> Result<HashMap<TrackRef, Option<Found>>> {
        let mut found = HashMap::new();
        for track in tracks {
            if !found.contains_key(track) {
                found.insert(track.clone(), self.find_track(track, folders)?);
            }
        }
        Ok(found)
    }

    /// Takes in what another library's users made: its users' rows as `users` maps their ids
    /// to this server's (the others' are left out), its playlists by the names `playlists`
    /// gives them here, and its tracks as `found` finds them here (the others are left out).
    /// Adds and never removes; what is here already is kept as it is.
    pub fn take_made(
        &mut self,
        made: &Made,
        users: &HashMap<i64, i64>,
        playlists: &HashMap<String, String>,
        found: &mut dyn FnMut(&TrackRef) -> Option<Found>,
    ) -> Result<Taken> {
        let mut taken = Taken::default();
        let playlist = |name: &str| {
            playlists
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.to_owned())
        };
        let tx = self.db.transaction()?;
        for play in &made.plays {
            let (Some(user), Some((path, start))) = (users.get(&play.user), found(&play.track))
            else {
                continue;
            };
            let at = epoch_millis(play.at);
            let there: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM plays WHERE path = ?1 AND start_ms IS ?2
                                                  AND played = ?3 AND user = ?4)",
                params![path, start, at, user],
                |row| row.get(0),
            )?;
            if !there {
                tx.execute(
                    "INSERT INTO plays (path, start_ms, played, user, playlist)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        path,
                        start,
                        at,
                        user,
                        play.playlist.as_deref().map(playlist)
                    ],
                )?;
                taken.plays += 1;
            }
        }
        for station in &made.station_plays {
            let Some(user) = users.get(&station.user) else {
                continue;
            };
            let at = epoch_millis(station.at);
            tx.execute(
                "INSERT INTO station_plays (name, url, played, user)
                 SELECT ?1, ?2, ?3, ?4 WHERE NOT EXISTS (
                     SELECT 1 FROM station_plays
                     WHERE url = ?2 AND played = ?3 AND user = ?4)",
                params![station.name, station.url, at, user],
            )?;
        }
        for favorite in &made.favorites {
            let Some(user) = users.get(&favorite.user) else {
                continue;
            };
            let at = epoch_millis(favorite.at);
            let added = match favorite.kind.as_str() {
                "album" => tx.execute(
                    "INSERT OR IGNORE INTO album_favorites (user, album_artist, album, at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![user, favorite.name, favorite.album, at],
                )?,
                "artist" => tx.execute(
                    "INSERT OR IGNORE INTO artist_favorites (user, artist, at) VALUES (?1, ?2, ?3)",
                    params![user, favorite.name, at],
                )?,
                "playlist" => tx.execute(
                    "INSERT OR IGNORE INTO playlist_favorites (user, playlist, at)
                     VALUES (?1, ?2, ?3)",
                    params![user, playlist(&favorite.name), at],
                )?,
                _ => 0,
            };
            taken.favorites += added;
        }
        for info in &made.playlist_info {
            let Some(user) = users.get(&info.user) else {
                continue;
            };
            tx.execute(
                "INSERT OR IGNORE INTO playlist_info (user, playlist, description, picture, backdrop)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    user,
                    playlist(&info.playlist),
                    info.description,
                    info.picture,
                    info.backdrop
                ],
            )?;
        }
        for added in &made.playlist_added {
            let (Some(user), Some((path, start))) = (users.get(&added.user), found(&added.track))
            else {
                continue;
            };
            tx.execute(
                "INSERT OR IGNORE INTO playlist_added (user, playlist, path, start_ms, added)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    user,
                    playlist(&added.playlist),
                    path,
                    start.unwrap_or(0),
                    epoch_millis(added.added)
                ],
            )?;
        }
        for edit in &made.artist_edits {
            taken.edits += tx.execute(
                "INSERT OR IGNORE INTO artist_edits
                     (artist, name, sort_name, mbid, biography, note, header)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    edit.artist,
                    edit.name,
                    edit.sort_name,
                    edit.mbid,
                    edit.biography,
                    edit.note,
                    edit.header
                ],
            )?;
        }
        for matched in &made.album_matches {
            taken.edits += tx.execute(
                "INSERT OR IGNORE INTO album_matches
                     (album_artist, album, release_group, artist_mbid, artist_key)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    matched.album_artist,
                    matched.album,
                    matched.release_group,
                    matched.artist_mbid,
                    matched.artist_key
                ],
            )?;
        }
        for genre in &made.genre_info {
            taken.edits += tx.execute(
                "INSERT OR IGNORE INTO genre_info (genre, picture, backdrop, description)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    genre.genre,
                    genre.picture,
                    genre.backdrop,
                    genre.description
                ],
            )?;
        }
        // The user's choice over what the internet gave; not over one the user made here.
        for picture in &made.artist_pictures {
            taken.edits += tx.execute(
                "INSERT INTO artist_pictures (artist, kind, source, picture, asked)
                 VALUES (?1, ?2, 'user', ?3, ?4)
                 ON CONFLICT (artist, kind) DO UPDATE
                     SET source = 'user', picture = excluded.picture, asked = excluded.asked
                     WHERE artist_pictures.source != 'user'",
                params![
                    picture.artist,
                    picture.kind,
                    picture.picture,
                    epoch_millis(std::time::SystemTime::now())
                ],
            )?;
        }
        tx.commit()?;
        Ok(taken)
    }
}

/// A track named from `row`: its path and start at `path` and `start`, its album artist, album,
/// title, number and disc from `tags` on.
fn track_ref(
    row: &rusqlite::Row<'_>,
    path: usize,
    start: usize,
    tags: usize,
    folders: &[PathBuf],
) -> rusqlite::Result<TrackRef> {
    let path = PathBuf::from(row.get::<_, String>(path)?);
    Ok(TrackRef {
        within: within(&path, folders),
        path,
        start_ms: row.get(start)?,
        album_artist: row.get(tags)?,
        album: row.get(tags + 1)?,
        title: row.get(tags + 2)?,
        track: row.get(tags + 3)?,
        disc: row.get(tags + 4)?,
    })
}

/// `path` within the first of `folders` it is in, its parts joined by '/'.
fn within(path: &Path, folders: &[PathBuf]) -> Option<String> {
    folders.iter().find_map(|folder| {
        let rest = path.strip_prefix(folder).ok()?;
        let parts: Vec<String> = rest
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        (!parts.is_empty()).then(|| parts.join("/"))
    })
}
