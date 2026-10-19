//! The media library: every track in the user's music folders, kept in SQLite so browsing and
//! searching stay fast without reading tags again.
//!
//! The tracks only hold what can be read back from the files, so they are a cache: when their
//! layout changes, they are rebuilt by the next scan. What the files cannot tell, when each
//! track was played, is kept in a table of its own that a rebuild leaves alone.

mod covers;
mod edits;
mod fetch;
mod matches;
mod online;
mod scan;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::cue::Part;
use crate::tags::TagInfo;

pub use covers::AlbumCover;
pub use edits::{ArtistEdit, Header};
pub use fetch::{LastFm, Online};
pub use matches::{AlbumMatch, plain_name, split_artist};
pub use online::{ASK_AGAIN_AFTER, ArtistText, Asked, CHOSEN, FOLDER, PictureKind, PictureRow};
pub use rusqlite::Error;
pub use scan::{ScanEvent, ScanOptions, ScanSummary, Scanner, scan, scan_with};

pub type Result<T> = std::result::Result<T, Error>;

/// Bumped whenever the tracks' tables change; they are then rebuilt.
const SCHEMA_VERSION: i32 = 5;

const SCHEMA: &str = "
    CREATE TABLE tracks (
        id INTEGER PRIMARY KEY,
        path TEXT NOT NULL,
        -- For a track of a CUE sheet, the part of the file it is, in milliseconds; the end is
        -- NULL for the file's last track. Both are NULL for a whole file.
        start_ms INTEGER,
        end_ms INTEGER,
        -- What the track was read from: the audio file, or the CUE sheet that cuts it into
        -- tracks. With its size and modification time then, to notice when it changes; for a
        -- sheet, these cover its audio files too.
        source TEXT NOT NULL,
        size INTEGER NOT NULL,
        modified INTEGER NOT NULL,
        -- The audio file's fingerprint (see `files::fingerprint`), by which its plays follow
        -- it when it is moved or renamed.
        fingerprint TEXT,
        title TEXT,
        artist TEXT,
        album TEXT,
        -- The artist the album is filed under: the album artist tag, else the artist.
        album_artist TEXT,
        -- MusicBrainz's id of the album artist, from the tags.
        artist_mbid TEXT,
        genre TEXT,
        year INTEGER,
        track INTEGER,
        disc INTEGER,
        duration_ms INTEGER,
        -- When the track came into the library, in milliseconds since 1970.
        added INTEGER NOT NULL
    );
    CREATE INDEX tracks_by_album ON tracks (album_artist, album);
    CREATE INDEX tracks_by_source ON tracks (source);
    CREATE INDEX tracks_by_path ON tracks (path, start_ms);
    CREATE INDEX tracks_by_fingerprint ON tracks (fingerprint);

    -- Words to search in, one row per track with the track's id as rowid. Diacritics are
    -- ignored, so \"beyonce\" finds \"Beyoncé\".
    CREATE VIRTUAL TABLE search USING fts5 (
        words,
        tokenize = 'unicode61 remove_diacritics 2'
    );
";

/// The user's own history, which no file holds: never dropped. A track is known by its path
/// and, for a track of a CUE sheet, where it starts, so plays outlive the track's row.
const HISTORY: &str = "
    CREATE TABLE IF NOT EXISTS plays (
        path TEXT NOT NULL,
        start_ms INTEGER,
        -- When, in milliseconds since 1970.
        played INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS plays_by_track ON plays (path, start_ms);
    -- When things last happened to the library, by name, in milliseconds since 1970:
    -- 'full-scan', when a scan of all its folders last ended.
    CREATE TABLE IF NOT EXISTS library_state (
        key TEXT NOT NULL PRIMARY KEY,
        at INTEGER NOT NULL
    );
";

/// Each played track once, with how often and when it was last played; joined with `tracks`
/// as `tracks.id`, `plays.n` and `plays.last`.
const PLAYED: &str = "(SELECT path, start_ms, count(*) AS n, max(played) AS last
                        FROM plays GROUP BY path, start_ms) AS plays
                       JOIN tracks ON tracks.path = plays.path
                                  AND tracks.start_ms IS plays.start_ms";

/// Columns of `tracks` read into a [`Track`], in [`Track::from_row`]'s order.
const TRACK_COLUMNS: &str = "tracks.id, tracks.path, tracks.start_ms, end_ms, title, artist, \
     album, album_artist, year, track, disc, duration_ms";

/// The order tracks are listed in: by album, then as on the album.
const ALBUM_ORDER: &str =
    "album_artist IS NULL, album_artist COLLATE NOCASE, album COLLATE NOCASE, disc, track, path";

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: i64,
    pub path: PathBuf,
    /// The part of the file this track is, for a track of a CUE sheet.
    pub part: Option<Part>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<u32>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub duration: Option<Duration>,
}

impl Track {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let start: Option<i64> = row.get(2)?;
        let end: Option<i64> = row.get(3)?;
        Ok(Self {
            id: row.get(0)?,
            path: PathBuf::from(row.get::<_, String>(1)?),
            part: start.map(|start| Part {
                start: from_millis(start),
                end: end.map(from_millis),
            }),
            title: row.get(4)?,
            artist: row.get(5)?,
            album: row.get(6)?,
            album_artist: row.get(7)?,
            year: row.get(8)?,
            track: row.get(9)?,
            disc: row.get(10)?,
            duration: row.get::<_, Option<i64>>(11)?.map(from_millis),
        })
    }

    /// The title, or the file name when the track has none.
    pub fn display_title(&self) -> String {
        self.title.clone().unwrap_or_else(|| file_stem(&self.path))
    }

    /// The tags as the playlist wants them.
    pub fn tags(&self) -> TagInfo {
        TagInfo {
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            album_artist: self.album_artist.clone(),
            artist_mbid: None,
            genre: None,
            year: self.year,
            track: self.track,
            disc: self.disc,
            duration: self.duration,
        }
    }
}

/// What an artist's page says of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtistSummary {
    pub albums: u32,
    pub tracks: u32,
    pub duration: Duration,
    /// Their most frequent genres, most first.
    pub genres: Vec<String>,
}

/// How artists are listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtistOrder {
    Name,
    /// Most played first, counting every play of every one of their tracks.
    MostPlayed,
    /// The one whose music came last first.
    RecentlyAdded,
}

/// An artist the library has albums by. `name` is `None` for untagged tracks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artist {
    /// As the tags name them; how they are known.
    pub name: Option<String>,
    /// The name the user gave them in Freya, when they did.
    pub shown_name: Option<String>,
    pub albums: u32,
    pub tracks: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Album {
    pub artist: Option<String>,
    pub name: Option<String>,
    pub year: Option<u32>,
    pub tracks: u32,
    /// One of the album's tracks, to find its cover from.
    pub track: PathBuf,
}

/// A genre the library has tracks in. `name` is `None` for tracks without one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Genre {
    pub name: Option<String>,
    pub albums: u32,
    pub tracks: u32,
}

/// A track that has been played.
#[derive(Clone, Debug, PartialEq)]
pub struct Played {
    pub track: Track,
    /// How many times.
    pub plays: u32,
    /// When it was last played.
    pub last: SystemTime,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub tracks: u32,
    pub albums: u32,
    pub artists: u32,
}

/// Size and modification time of a file, to tell whether it changed since it was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub size: i64,
    /// Milliseconds since 1970.
    pub modified: i64,
}

impl FileStamp {
    pub fn of(metadata: &std::fs::Metadata) -> Self {
        let modified = metadata.modified().map_or(0, epoch_millis);
        Self {
            size: metadata.len() as i64,
            modified,
        }
    }
}

pub struct Library {
    db: Connection,
}

impl Library {
    /// Opens the library at `path`, creating it, or rebuilding it if it is from an older
    /// version of Freya.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::setup(Connection::open(path)?)
    }

    /// A library that lives only in memory, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::setup(Connection::open_in_memory()?)
    }

    fn setup(db: Connection) -> Result<Self> {
        // First, so that every statement below waits for a lock another connection holds
        // (the window's, while a scan opens its own) instead of failing at once.
        db.busy_timeout(Duration::from_secs(5))?;
        // WAL lets the window read while a scan writes on another connection.
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        let mut version: i32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        // Each file is read again on the next scan, which keeps when its tracks came in.
        if version == 3 {
            log::info!("adding artists' MusicBrainz ids to the library");
            db.execute_batch(
                "ALTER TABLE tracks ADD COLUMN artist_mbid TEXT;
                 UPDATE tracks SET modified = -1;",
            )?;
            version = 4;
            db.pragma_update(None, "user_version", version)?;
        }
        if version == 4 {
            log::info!("adding files' fingerprints to the library");
            db.execute_batch(
                "ALTER TABLE tracks ADD COLUMN fingerprint TEXT;
                 CREATE INDEX tracks_by_fingerprint ON tracks (fingerprint);
                 UPDATE tracks SET modified = -1;",
            )?;
            version = 5;
            db.pragma_update(None, "user_version", version)?;
        }
        if version != SCHEMA_VERSION {
            if version != 0 {
                log::info!("rebuilding the library (layout {version} → {SCHEMA_VERSION})");
            }
            db.execute_batch(
                "DROP TABLE IF EXISTS tracks;
                 DROP TABLE IF EXISTS search;",
            )?;
            db.execute_batch(SCHEMA)?;
            db.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        db.execute_batch(HISTORY)?;
        db.execute_batch(online::SCHEMA)?;
        db.execute_batch(edits::SCHEMA)?;
        edits::migrate(&db)?;
        db.execute_batch(matches::SCHEMA)?;
        db.execute_batch(covers::SCHEMA)?;
        Ok(Self { db })
    }

    // ----- Browsing ----------------------------------------------------------------------------

    pub fn stats(&self) -> Result<Stats> {
        self.db.query_row(
            "SELECT count(*),
                    count(DISTINCT album_artist || char(0) || album),
                    count(DISTINCT album_artist)
             FROM tracks",
            [],
            |row| {
                Ok(Stats {
                    tracks: row.get(0)?,
                    albums: row.get(1)?,
                    artists: row.get(2)?,
                })
            },
        )
    }

    /// Every album artist, alphabetically, with untagged tracks last.
    pub fn artists(&self) -> Result<Vec<Artist>> {
        self.artists_by(ArtistOrder::Name)
    }

    /// Every artist, in `order`; without a name last whatever the order.
    pub fn artists_by(&self, order: ArtistOrder) -> Result<Vec<Artist>> {
        let order = match order {
            ArtistOrder::Name => "",
            ArtistOrder::MostPlayed => "sum(coalesce(plays.n, 0)) DESC,",
            ArtistOrder::RecentlyAdded => "max(tracks.added) DESC,",
        };
        // By the name the user sorts them by, else the name shown.
        let mut query = self.db.prepare_cached(&format!(
            "SELECT album_artist, count(DISTINCT album), count(*), edits.name
             FROM tracks
             LEFT JOIN (SELECT path, start_ms, count(*) AS n FROM plays GROUP BY path, start_ms)
                 AS plays ON plays.path = tracks.path AND plays.start_ms IS tracks.start_ms
             LEFT JOIN artist_edits AS edits ON edits.artist = tracks.album_artist
             GROUP BY album_artist
             ORDER BY album_artist IS NULL, {order}
                      coalesce(edits.sort_name, edits.name, album_artist) COLLATE NOCASE"
        ))?;
        query
            .query_map([], |row| {
                Ok(Artist {
                    name: row.get(0)?,
                    shown_name: row.get(3)?,
                    albums: row.get(1)?,
                    tracks: row.get(2)?,
                })
            })?
            .collect()
    }

    /// The albums filed under `artist`, oldest first; albums without a year after those with.
    pub fn albums(&self, artist: Option<&str>) -> Result<Vec<Album>> {
        let mut query = self.db.prepare_cached(
            "SELECT album_artist, album, min(year), count(*), min(path)
             FROM tracks
             WHERE album_artist IS ?1
             GROUP BY album
             ORDER BY album IS NULL, min(year) IS NULL, min(year), album COLLATE NOCASE",
        )?;
        query.query_map([artist], album_from_row)?.collect()
    }

    /// The track in the file `path` that starts at `start` (zero for a whole file).
    pub fn track_at(&self, path: &Path, start: Duration) -> Result<Option<Track>> {
        let Some(path) = path.to_str() else {
            return Ok(None);
        };
        self.db
            .prepare_cached(&format!(
                "SELECT {TRACK_COLUMNS} FROM tracks
                 WHERE path = ?1 AND ifnull(start_ms, 0) = ?2 LIMIT 1"
            ))?
            .query_row(params![path, start.as_millis() as i64], Track::from_row)
            .optional()
    }

    /// The tracks of one album, in album order.
    pub fn album_tracks(&self, artist: Option<&str>, album: Option<&str>) -> Result<Vec<Track>> {
        let mut query = self.db.prepare_cached(&format!(
            "SELECT {TRACK_COLUMNS} FROM tracks
             WHERE album_artist IS ?1 AND album IS ?2
             ORDER BY disc, track, path"
        ))?;
        query
            .query_map(params![artist, album], Track::from_row)?
            .collect()
    }

    /// Every track filed under `artist`, album by album.
    pub fn artist_tracks(&self, artist: Option<&str>) -> Result<Vec<Track>> {
        let mut query = self.db.prepare_cached(&format!(
            "SELECT {TRACK_COLUMNS} FROM tracks
             WHERE album_artist IS ?1
             ORDER BY album IS NULL, year IS NULL, year, album COLLATE NOCASE, disc, track, path"
        ))?;
        query.query_map([artist], Track::from_row)?.collect()
    }

    /// Every album, by name; albums without a name last.
    pub fn all_albums(&self) -> Result<Vec<Album>> {
        let mut query = self.db.prepare_cached(
            "SELECT album_artist, album, min(year), count(*), min(path)
             FROM tracks
             GROUP BY album_artist, album
             ORDER BY album IS NULL, album COLLATE NOCASE, album_artist COLLATE NOCASE",
        )?;
        query.query_map([], album_from_row)?.collect()
    }

    /// Every genre, alphabetically, with tracks without one last.
    pub fn genres(&self) -> Result<Vec<Genre>> {
        let mut query = self.db.prepare_cached(
            "SELECT genre,
                    count(DISTINCT ifnull(album_artist, '') || char(0) || ifnull(album, '')),
                    count(*)
             FROM tracks
             GROUP BY genre
             ORDER BY genre IS NULL, genre COLLATE NOCASE",
        )?;
        query
            .query_map([], |row| {
                Ok(Genre {
                    name: row.get(0)?,
                    albums: row.get(1)?,
                    tracks: row.get(2)?,
                })
            })?
            .collect()
    }

    /// The albums with tracks in `genre`, by name.
    pub fn genre_albums(&self, genre: Option<&str>) -> Result<Vec<Album>> {
        let mut query = self.db.prepare_cached(
            "SELECT album_artist, album, min(year), count(*), min(path)
             FROM tracks
             WHERE genre IS ?1
             GROUP BY album_artist, album
             ORDER BY album IS NULL, album COLLATE NOCASE, album_artist COLLATE NOCASE",
        )?;
        query.query_map([genre], album_from_row)?.collect()
    }

    /// Every track in `genre`, album by album.
    pub fn genre_tracks(&self, genre: Option<&str>) -> Result<Vec<Track>> {
        let mut query = self.db.prepare_cached(&format!(
            "SELECT {TRACK_COLUMNS} FROM tracks
             WHERE genre IS ?1
             ORDER BY {ALBUM_ORDER}"
        ))?;
        query.query_map([genre], Track::from_row)?.collect()
    }

    /// Every track, by title, then artist; tracks without a title last.
    pub fn all_tracks(&self) -> Result<Vec<Track>> {
        let mut query = self.db.prepare_cached(&format!(
            "SELECT {TRACK_COLUMNS} FROM tracks
             ORDER BY title IS NULL, title COLLATE NOCASE, artist COLLATE NOCASE, path"
        ))?;
        query.query_map([], Track::from_row)?.collect()
    }

    /// The albums that came into the library last, newest first. At most `limit`.
    pub fn recently_added(&self, limit: usize) -> Result<Vec<Album>> {
        let mut query = self.db.prepare_cached(
            "SELECT album_artist, album, min(year), count(*), min(path)
             FROM tracks
             GROUP BY album_artist, album
             ORDER BY max(added) DESC, album COLLATE NOCASE
             LIMIT ?1",
        )?;
        query.query_map([limit as i64], album_from_row)?.collect()
    }

    /// The tracks played last, the latest first. At most `limit`.
    pub fn recently_played(&self, limit: usize) -> Result<Vec<Played>> {
        self.played("plays.last DESC", limit)
    }

    /// The tracks played most often; equally often played ones by when they were played last.
    /// At most `limit`.
    pub fn most_played(&self, limit: usize) -> Result<Vec<Played>> {
        self.played("plays.n DESC, plays.last DESC", limit)
    }

    fn played(&self, order: &str, limit: usize) -> Result<Vec<Played>> {
        let mut query = self.db.prepare_cached(&format!(
            "SELECT {TRACK_COLUMNS}, plays.n, plays.last FROM {PLAYED}
             ORDER BY {order}
             LIMIT ?1"
        ))?;
        query
            .query_map([limit as i64], |row| {
                Ok(Played {
                    track: Track::from_row(row)?,
                    plays: row.get(12)?,
                    last: from_epoch_millis(row.get(13)?),
                })
            })?
            .collect()
    }

    /// The artist's MusicBrainz id, as most of their tracks' tags give it.
    pub fn artist_mbid(&self, artist: Option<&str>) -> Result<Option<String>> {
        self.db
            .prepare_cached(
                "SELECT artist_mbid FROM tracks
                 WHERE album_artist IS ?1 AND artist_mbid IS NOT NULL
                 GROUP BY artist_mbid ORDER BY count(*) DESC LIMIT 1",
            )?
            .query_row([artist], |row| row.get(0))
            .optional()
    }

    /// What an artist's page says of them: how many albums and tracks, how long they play,
    /// and their most frequent genres, at most `genres`.
    pub fn artist_summary(&self, artist: Option<&str>, genres: usize) -> Result<ArtistSummary> {
        let (albums, tracks, millis) = self.db.query_row(
            // Tracks without an album are one, "Unknown album", as the albums list them.
            "SELECT count(DISTINCT coalesce(album, char(0))), count(*), coalesce(sum(duration_ms), 0)
             FROM tracks WHERE album_artist IS ?1",
            [artist],
            |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)?)),
        )?;
        let mut query = self.db.prepare_cached(
            "SELECT genre FROM tracks WHERE album_artist IS ?1 AND genre IS NOT NULL
             GROUP BY genre ORDER BY count(*) DESC, genre COLLATE NOCASE LIMIT ?2",
        )?;
        let genres = query
            .query_map(params![artist, genres as i64], |row| row.get(0))?
            .collect::<Result<_>>()?;
        Ok(ArtistSummary {
            albums,
            tracks,
            duration: from_millis(millis),
            genres,
        })
    }

    /// An artist's most played tracks, most first; equally often played ones by when they were
    /// played last. At most `limit`, and only those played.
    pub fn artist_popular(&self, artist: Option<&str>, limit: usize) -> Result<Vec<Played>> {
        let mut query = self.db.prepare_cached(&format!(
            "SELECT {TRACK_COLUMNS}, plays.n, plays.last FROM {PLAYED}
             WHERE tracks.album_artist IS ?1
             ORDER BY plays.n DESC, plays.last DESC
             LIMIT ?2"
        ))?;
        query
            .query_map(params![artist, limit as i64], |row| {
                Ok(Played {
                    track: Track::from_row(row)?,
                    plays: row.get(12)?,
                    last: from_epoch_millis(row.get(13)?),
                })
            })?
            .collect()
    }

    /// Tracks where every word of `text` starts a word of the artist, album, title, genre or
    /// file name, in album order. At most `limit` tracks.
    pub fn search(&self, text: &str, limit: usize) -> Result<Vec<Track>> {
        let Some(pattern) = search_pattern(text) else {
            return Ok(Vec::new());
        };
        let mut query = self.db.prepare_cached(&format!(
            "SELECT {TRACK_COLUMNS} FROM search
             JOIN tracks ON tracks.id = search.rowid
             WHERE search MATCH ?1
             ORDER BY {ALBUM_ORDER}
             LIMIT ?2"
        ))?;
        query
            .query_map(params![pattern, limit as i64], Track::from_row)?
            .collect()
    }

    // ----- History ------------------------------------------------------------------------------

    /// When a scan of all the library's folders last ended, if one ever did.
    pub fn last_full_scan(&self) -> Result<Option<SystemTime>> {
        let at: Option<i64> = self
            .db
            .prepare_cached("SELECT at FROM library_state WHERE key = 'full-scan'")?
            .query_row([], |row| row.get(0))
            .optional()?;
        Ok(at.map(from_epoch_millis))
    }

    /// Notes that a scan of all the library's folders ended at `at`.
    pub fn note_full_scan(&self, at: SystemTime) -> Result<()> {
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO library_state (key, at) VALUES ('full-scan', ?1)",
            )?
            .execute([epoch_millis(at)])?;
        Ok(())
    }

    /// Notes that a track was played, at `at`. It need not be in the library: its plays count
    /// once it is.
    pub fn record_play(&self, path: &Path, part: Option<Part>, at: SystemTime) -> Result<()> {
        let Some(path) = path.to_str() else {
            return Ok(());
        };
        self.db
            .prepare_cached("INSERT INTO plays (path, start_ms, played) VALUES (?1, ?2, ?3)")?
            .execute(params![
                path,
                part.map(|p| p.start.as_millis() as i64),
                epoch_millis(at)
            ])?;
        Ok(())
    }

    /// How often each played track of an album was played, by its file and where in the file
    /// it starts (`None` for a whole file).
    pub fn album_plays(
        &self,
        artist: Option<&str>,
        album: Option<&str>,
    ) -> Result<HashMap<(PathBuf, Option<Duration>), u32>> {
        let mut query = self.db.prepare_cached(
            "SELECT plays.path, plays.start_ms, count(*) FROM plays
             JOIN tracks ON tracks.path = plays.path AND tracks.start_ms IS plays.start_ms
             WHERE tracks.album_artist IS ?1 AND tracks.album IS ?2
             GROUP BY plays.path, plays.start_ms",
        )?;
        query
            .query_map(params![artist, album], |row| {
                let path = PathBuf::from(row.get::<_, String>(0)?);
                let start = row.get::<_, Option<i64>>(1)?.map(from_millis);
                Ok(((path, start), row.get::<_, u32>(2)?))
            })?
            .collect()
    }

    // ----- Changes, made by scans --------------------------------------------------------------

    /// Every file the library was read from, audio files and CUE sheets, with the stamp it
    /// had then.
    pub fn sources(&self) -> Result<HashMap<PathBuf, FileStamp>> {
        let mut query = self
            .db
            .prepare("SELECT source, size, modified FROM tracks GROUP BY source")?;
        query
            .query_map([], |row| {
                Ok((
                    PathBuf::from(row.get::<_, String>(0)?),
                    FileStamp {
                        size: row.get(1)?,
                        modified: row.get(2)?,
                    },
                ))
            })?
            .collect()
    }

    pub fn transaction(&mut self) -> Result<Transaction<'_>> {
        self.db.transaction()
    }
}

/// A track as read from its files, to be added.
#[derive(Clone, Debug, PartialEq)]
struct NewTrack {
    path: PathBuf,
    part: Option<Part>,
    tags: TagInfo,
    /// The audio file's fingerprint, when it could be read.
    fingerprint: Option<String>,
}

/// Adds a track read from `source`, which had `stamp` then. `added` is when the track came into
/// the library, in milliseconds since 1970.
fn put(
    db: &Connection,
    source: &Path,
    stamp: FileStamp,
    added: i64,
    track: &NewTrack,
) -> Result<()> {
    let (Some(source_text), Some(path_text)) = (source.to_str(), track.path.to_str()) else {
        log::warn!("skipping {}: the path is not valid UTF-8", source.display());
        return Ok(());
    };
    let tags = &track.tags;
    let tagged = tags.album_artist.as_ref().or(tags.artist.as_ref());
    // Matched to another artist by the user: filed under them.
    let filed = match (tagged, &tags.album) {
        (Some(artist), Some(album)) => matches::filed_under(db, artist, album)?,
        _ => None,
    };
    let album_artist = filed.as_ref().or(tagged);
    let millis = |d: Duration| d.as_millis() as i64;
    let id: i64 = db
        .prepare_cached(
            "INSERT INTO tracks (path, start_ms, end_ms, source, size, modified, title, artist,
                                 album, album_artist, artist_mbid, genre, year, track, disc,
                                 duration_ms, added, fingerprint)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                     ?18)
             RETURNING id",
        )?
        .query_row(
            params![
                path_text,
                track.part.map(|p| millis(p.start)),
                track.part.and_then(|p| p.end).map(millis),
                source_text,
                stamp.size,
                stamp.modified,
                tags.title,
                tags.artist,
                tags.album,
                album_artist,
                tags.artist_mbid,
                tags.genre,
                tags.year,
                tags.track,
                tags.disc,
                tags.duration.map(millis),
                added,
                track.fingerprint,
            ],
            |row| row.get(0),
        )?;
    // A whole file is also found by its name, in case its tags say little.
    let file_name = track.part.is_none().then(|| file_stem(&track.path));
    let words = [
        tags.artist.as_deref(),
        tags.album_artist.as_deref(),
        tags.album.as_deref(),
        tags.title.as_deref(),
        tags.genre.as_deref(),
        file_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");
    db.prepare_cached("INSERT INTO search (rowid, words) VALUES (?1, ?2)")?
        .execute(params![id, words])?;
    Ok(())
}

/// When the tracks read from `source` came into the library, if they are in it.
fn added(db: &Connection, source: &Path) -> Result<Option<i64>> {
    db.prepare_cached("SELECT min(added) FROM tracks WHERE source = ?1")?
        .query_row([source.to_string_lossy()], |row| row.get(0))
}

/// Removes every track read from `source`.
fn remove(db: &Connection, source: &Path) -> Result<()> {
    let source = source.to_string_lossy();
    db.prepare_cached(
        "DELETE FROM search WHERE rowid IN (SELECT id FROM tracks WHERE source = ?1)",
    )?
    .execute([&source])?;
    db.prepare_cached("DELETE FROM tracks WHERE source = ?1")?
        .execute([&source])?;
    Ok(())
}

/// Turns typed text into an FTS5 query: every word must start a word in the track. Each word
/// is quoted, so characters like `"`, `*` or `-` are searched for rather than taken as query
/// syntax. `None` when there is nothing to search for.
fn search_pattern(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split_whitespace()
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// An album from a row of artist, name, year, track count and one track's path.
fn album_from_row(row: &Row<'_>) -> rusqlite::Result<Album> {
    Ok(Album {
        artist: row.get(0)?,
        name: row.get(1)?,
        year: row.get(2)?,
        tracks: row.get(3)?,
        track: PathBuf::from(row.get::<_, String>(4)?),
    })
}

fn from_millis(ms: i64) -> Duration {
    Duration::from_millis(ms.max(0) as u64)
}

/// Milliseconds since 1970.
fn epoch_millis(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn from_epoch_millis(ms: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + from_millis(ms)
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
