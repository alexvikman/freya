//! Organizing the music's files: what is to be changed in their tags until it is written, what
//! was written so it can be undone, and what the users made with the library following the
//! names and places that changed.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use freya_protocol::{
    FolderProposal, FolderState, OrganizeFolder, ProposalState, ReleaseCandidate, TagDraft,
    TagField, TagMap, TagSave, plain_name,
};
use rusqlite::{OptionalExtension, Transaction, params};

use super::{Library, Result, epoch_millis, from_epoch_millis};
use crate::thumbnails::Thumbnails;

pub(super) const SCHEMA: &str = "
    -- What is to be changed in a file's tags, as JSON, until it is written or dropped.
    CREATE TABLE IF NOT EXISTS tag_drafts (
        path TEXT NOT NULL PRIMARY KEY,
        changes TEXT NOT NULL
    );
    -- Which releases a folder may be, and the one its drafts are of.
    CREATE TABLE IF NOT EXISTS folder_proposals (
        folder TEXT NOT NULL PRIMARY KEY,
        state TEXT NOT NULL,
        chosen TEXT,
        candidates TEXT NOT NULL
    );
    -- Each time drafts were written to files, to undo it.
    CREATE TABLE IF NOT EXISTS tag_saves (
        id INTEGER PRIMARY KEY,
        at INTEGER NOT NULL,
        folder TEXT NOT NULL,
        undone INTEGER NOT NULL DEFAULT 0,
        -- A cover the save added beside the music, which undoing it takes away.
        cover TEXT
    );
    CREATE TABLE IF NOT EXISTS tag_saved_files (
        save INTEGER NOT NULL,
        -- Where it was before the save, and is after it.
        path TEXT NOT NULL,
        path_after TEXT NOT NULL,
        -- What the fields changed were, as JSON: blank for one it did not have.
        before TEXT NOT NULL,
        PRIMARY KEY (save, path)
    );
";

pub mod editions;
pub mod names;

/// A file a save changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedFile {
    pub path: PathBuf,
    pub path_after: PathBuf,
    /// What the changed fields were: blank for one the file did not have.
    pub before: TagMap,
    /// What they are now.
    pub after: TagMap,
}

fn state_name(state: ProposalState) -> &'static str {
    match state {
        ProposalState::None => "none",
        ProposalState::Review => "review",
        ProposalState::Ready => "ready",
    }
}

fn state_from_name(name: &str) -> ProposalState {
    match name {
        "review" => ProposalState::Review,
        "ready" => ProposalState::Ready,
        _ => ProposalState::None,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn from_json(json: &str, column: usize) -> rusqlite::Result<TagMap> {
    serde_json::from_str(json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn to_json(map: &TagMap) -> String {
    serde_json::to_string(map).unwrap_or_else(|_| "{}".to_owned())
}

impl Library {
    /// What is to be changed in the files at `paths`, those that have a draft.
    pub fn tag_drafts(&self, paths: &[PathBuf]) -> Result<Vec<TagDraft>> {
        let mut query = self
            .db
            .prepare_cached("SELECT changes FROM tag_drafts WHERE path = ?1")?;
        let mut drafts = Vec::new();
        for path in paths {
            let changes = query
                .query_row([text(path)], |row| from_json(&row.get::<_, String>(0)?, 0))
                .optional()?;
            if let Some(changes) = changes {
                drafts.push(TagDraft {
                    path: path.clone(),
                    changes,
                });
            }
        }
        Ok(drafts)
    }

    /// Keeps `drafts`, in place of what those files had; one that changes nothing goes.
    pub fn set_tag_drafts(&mut self, drafts: &[TagDraft]) -> Result<()> {
        let tx = self.db.transaction()?;
        for draft in drafts {
            if draft.changes.is_empty() {
                tx.execute(
                    "DELETE FROM tag_drafts WHERE path = ?1",
                    [text(&draft.path)],
                )?;
            } else {
                tx.execute(
                    "INSERT OR REPLACE INTO tag_drafts (path, changes) VALUES (?1, ?2)",
                    params![text(&draft.path), to_json(&draft.changes)],
                )?;
            }
        }
        tx.commit()
    }

    /// The library's folders of music, by what they are in and their names.
    pub fn organize_folders(&self) -> Result<Vec<OrganizeFolder>> {
        #[derive(Default)]
        struct Folder {
            files: u32,
            identified: u32,
            drafts: u32,
            albums: HashMap<String, u32>,
        }
        let reviewed: HashSet<String> = self
            .db
            .prepare_cached("SELECT folder FROM folder_proposals WHERE state = 'review'")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_>>()?;
        let drafted: HashSet<String> = self
            .db
            .prepare_cached("SELECT path FROM tag_drafts")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_>>()?;
        let mut statement = self
            .db
            .prepare_cached("SELECT path, album, max(release_id) FROM tracks GROUP BY path")?;
        let mut rows = statement.query([])?;
        let mut folders: BTreeMap<PathBuf, Folder> = BTreeMap::new();
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            let album: Option<String> = row.get(1)?;
            let release: Option<String> = row.get(2)?;
            let folder = folders
                .entry(names::album_folder(Path::new(&path)))
                .or_default();
            folder.files += 1;
            folder.identified += u32::from(release.is_some());
            folder.drafts += u32::from(drafted.contains(&path));
            *folder.albums.entry(album.unwrap_or_default()).or_default() += 1;
        }
        let mut list: Vec<OrganizeFolder> = folders
            .into_iter()
            .map(|(path, folder)| {
                let name = file_name(&path);
                let group = path.parent().map(file_name).unwrap_or_default();
                let most = folder.albums.values().copied().max().unwrap_or(0);
                let state = if folder.drafts > 0 && reviewed.contains(&text(&path)) {
                    FolderState::Review
                } else if folder.drafts > 0 {
                    FolderState::Ready
                } else if folder.identified == folder.files {
                    FolderState::Identified
                } else {
                    FolderState::Unidentified
                };
                OrganizeFolder {
                    parsed: names::parse_folder_name(&name),
                    path,
                    group,
                    name,
                    files: folder.files,
                    state,
                    // Most of them on one album is an album with a stray track or two.
                    mixed: folder.albums.len() > 1 && most * 5 < folder.files * 4,
                }
            })
            .collect();
        list.sort_by_cached_key(|folder| (folder.group.to_lowercase(), folder.name.to_lowercase()));
        Ok(list)
    }

    /// The paths of the library's files under `folder`, each once, in order.
    pub fn files_under(&self, folder: &Path) -> Result<Vec<(PathBuf, Option<Duration>, bool)>> {
        let tracks = self.tracks_under(&[folder.to_path_buf()])?;
        let mut files: Vec<(PathBuf, Option<Duration>, bool)> = Vec::new();
        for track in tracks {
            match files.last_mut() {
                // A CUE sheet's tracks are one file, known as parts of it.
                Some(last) if last.0 == track.path => last.2 = true,
                _ => files.push((track.path, track.duration, track.part.is_some())),
            }
        }
        Ok(files)
    }

    /// The releases the album's folder `folder` may be, when it was asked.
    pub fn folder_proposal(&self, folder: &Path) -> Result<Option<FolderProposal>> {
        self.db
            .prepare_cached(
                "SELECT state, chosen, candidates FROM folder_proposals WHERE folder = ?1",
            )?
            .query_row([text(folder)], |row| {
                let candidates: String = row.get(2)?;
                Ok(FolderProposal {
                    folder: folder.to_path_buf(),
                    state: state_from_name(&row.get::<_, String>(0)?),
                    chosen: row.get(1)?,
                    candidates: serde_json::from_str::<Vec<ReleaseCandidate>>(&candidates)
                        .unwrap_or_default(),
                })
            })
            .optional()
    }

    /// Keeps what came of asking which release a folder is.
    pub fn set_folder_proposal(&mut self, proposal: &FolderProposal) -> Result<()> {
        let candidates =
            serde_json::to_string(&proposal.candidates).unwrap_or_else(|_| "[]".to_owned());
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO folder_proposals (folder, state, chosen, candidates)
                 VALUES (?1, ?2, ?3, ?4)",
            )?
            .execute(params![
                text(&proposal.folder),
                state_name(proposal.state),
                proposal.chosen,
                candidates
            ])?;
        Ok(())
    }

    /// The last `limit` saves, the latest first.
    pub fn tag_saves(&self, limit: usize) -> Result<Vec<TagSave>> {
        let mut query = self.db.prepare_cached(
            "SELECT s.id, s.at, s.folder, s.undone,
                    (SELECT count(*) FROM tag_saved_files WHERE save = s.id)
             FROM tag_saves AS s ORDER BY s.id DESC LIMIT ?1",
        )?;
        let saves = query.query_map([limit as i64], |row| {
            Ok(TagSave {
                id: row.get(0)?,
                at: from_epoch_millis(row.get(1)?),
                folder: PathBuf::from(row.get::<_, String>(2)?),
                undone: row.get(3)?,
                files: row.get(4)?,
            })
        })?;
        saves.collect()
    }

    /// Notes that `files` were written, takes away their drafts, and has what the users made
    /// with the library follow the names and places that changed. The save's id.
    pub fn note_tag_save(
        &mut self,
        folder: &Path,
        files: &[SavedFile],
        at: SystemTime,
    ) -> Result<i64> {
        let tx = self.db.transaction()?;
        carry_names(&tx, files)?;
        carry_places(&tx, files)?;
        tx.execute(
            "INSERT INTO tag_saves (at, folder) VALUES (?1, ?2)",
            params![epoch_millis(at), text(folder)],
        )?;
        let save = tx.last_insert_rowid();
        for file in files {
            tx.execute(
                "INSERT INTO tag_saved_files (save, path, path_after, before)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    save,
                    text(&file.path),
                    text(&file.path_after),
                    to_json(&file.before)
                ],
            )?;
            tx.execute("DELETE FROM tag_drafts WHERE path = ?1", [text(&file.path)])?;
            tx.execute(
                "DELETE FROM folder_proposals WHERE folder = ?1",
                [text(&names::album_folder(&file.path))],
            )?;
        }
        tx.commit()?;
        Ok(save)
    }

    /// Notes the cover the save `save` added beside the music.
    pub fn note_save_cover(&self, save: i64, cover: &Path) -> Result<()> {
        self.db.execute(
            "UPDATE tag_saves SET cover = ?2 WHERE id = ?1",
            params![save, text(cover)],
        )?;
        Ok(())
    }

    /// The cover the save `save` added beside the music, if it did.
    pub fn save_cover(&self, save: i64) -> Result<Option<PathBuf>> {
        Ok(self
            .db
            .prepare_cached("SELECT cover FROM tag_saves WHERE id = ?1")?
            .query_row([save], |row| row.get::<_, Option<String>>(0))
            .optional()?
            .flatten()
            .map(PathBuf::from))
    }

    /// The files of the save `save`, as they are to be put back; none when it was undone or
    /// there is no such save. `before` holds what to write, and `path` where each is to go.
    pub fn tag_save_files(&self, save: i64) -> Result<Vec<SavedFile>> {
        let undone: Option<bool> = self
            .db
            .prepare_cached("SELECT undone FROM tag_saves WHERE id = ?1")?
            .query_row([save], |row| row.get(0))
            .optional()?;
        if undone != Some(false) {
            return Ok(Vec::new());
        }
        let mut query = self.db.prepare_cached(
            "SELECT path, path_after, before FROM tag_saved_files WHERE save = ?1",
        )?;
        let files = query.query_map([save], |row| {
            Ok(SavedFile {
                path: PathBuf::from(row.get::<_, String>(0)?),
                path_after: PathBuf::from(row.get::<_, String>(1)?),
                before: from_json(&row.get::<_, String>(2)?, 2)?,
                after: TagMap::new(),
            })
        })?;
        files.collect()
    }

    /// Notes that the save `save` was undone: `files` are as they were, with what the users
    /// made following them back. Each of `files` is as the undo wrote it: `path` where it was
    /// when undone, `path_after` where it is now, `after` what was written.
    pub fn note_tag_undo(&mut self, save: i64, files: &[SavedFile]) -> Result<()> {
        let tx = self.db.transaction()?;
        carry_names(&tx, files)?;
        carry_places(&tx, files)?;
        tx.execute("UPDATE tag_saves SET undone = 1 WHERE id = ?1", [save])?;
        tx.commit()
    }
}

/// A name's tracks before a save, and how many of them the save gave one new name.
#[derive(Default)]
struct Renames {
    moved: HashMap<(String, String), u32>,
}

/// Has what the users made with albums and artists whose names the save changed follow them:
/// only when every track of the old name got the new one, as otherwise the old name lives on.
fn carry_names(tx: &Transaction<'_>, files: &[SavedFile]) -> Result<()> {
    let mut albums: HashMap<(String, String), Renames> = HashMap::new();
    let mut artists: HashMap<String, Renames> = HashMap::new();
    let mut row = tx.prepare_cached(
        "SELECT album_artist, album FROM tracks WHERE path = ?1 AND album_artist IS NOT NULL
             AND album IS NOT NULL LIMIT 1",
    )?;
    for file in files {
        let found: Option<(String, String)> = row
            .query_row([text(&file.path)], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let Some((artist, album)) = found else {
            continue;
        };
        let new_album = named(&file.after, TagField::Album).unwrap_or(&album);
        let new_artist = named(&file.after, TagField::AlbumArtist).unwrap_or(plain_name(&artist));
        if new_album != album || new_artist != plain_name(&artist) {
            *albums
                .entry((artist.clone(), album.clone()))
                .or_default()
                .moved
                .entry((new_artist.to_owned(), new_album.to_owned()))
                .or_default() += 1;
        }
        if new_artist != plain_name(&artist) {
            *artists
                .entry(artist.clone())
                .or_default()
                .moved
                .entry((new_artist.to_owned(), String::new()))
                .or_default() += 1;
        }
    }
    drop(row);
    for ((artist, album), renames) in albums {
        let total: u32 = tx.query_row(
            "SELECT count(DISTINCT path) FROM tracks WHERE album_artist = ?1 AND album = ?2",
            params![artist, album],
            |row| row.get(0),
        )?;
        if let Some(((new_artist, new_album), _)) =
            renames.moved.into_iter().find(|(_, count)| *count == total)
        {
            carry_album(tx, &artist, &album, &new_artist, &new_album)?;
        }
    }
    for (artist, renames) in artists {
        let total: u32 = tx.query_row(
            "SELECT count(DISTINCT path) FROM tracks WHERE album_artist = ?1",
            [&artist],
            |row| row.get(0),
        )?;
        if let Some(((new_artist, _), _)) =
            renames.moved.into_iter().find(|(_, count)| *count == total)
        {
            carry_artist(tx, &artist, &new_artist)?;
        }
    }
    Ok(())
}

/// The field's new value, when the save gave it one.
fn named(fields: &TagMap, field: TagField) -> Option<&str> {
    fields
        .get(&field)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
}

pub(super) fn carry_album(
    tx: &Transaction<'_>,
    artist: &str,
    album: &str,
    new_artist: &str,
    new_album: &str,
) -> Result<()> {
    // Filed under a split-off artist while the artist's name stays: filed under them still.
    let filed = if new_artist == plain_name(artist) {
        artist
    } else {
        new_artist
    };
    for table in ["album_favorites", "album_kinds", "album_online_genres"] {
        tx.execute(
            &format!(
                "UPDATE OR REPLACE {table} SET album_artist = ?1, album = ?2
                 WHERE album_artist = ?3 AND album = ?4"
            ),
            params![filed, new_album, artist, album],
        )?;
    }
    tx.execute(
        "UPDATE OR REPLACE album_matches SET album_artist = ?1, album = ?2
         WHERE album_artist = ?3 AND album = ?4",
        params![new_artist, new_album, plain_name(artist), album],
    )?;
    let old_key = Thumbnails::key(Some(artist), Some(album));
    let new_key = Thumbnails::key(Some(filed), Some(new_album));
    for table in ["album_covers", "album_online_covers", "album_chosen_covers"] {
        tx.execute(
            &format!("UPDATE OR REPLACE {table} SET key = ?1 WHERE key = ?2"),
            params![new_key, old_key],
        )?;
    }
    Ok(())
}

fn carry_artist(tx: &Transaction<'_>, artist: &str, new_artist: &str) -> Result<()> {
    for table in [
        "artist_edits",
        "artist_favorites",
        "artist_ids",
        "artist_articles",
        "artist_info",
        "artist_pictures",
    ] {
        tx.execute(
            &format!("UPDATE OR REPLACE {table} SET artist = ?1 WHERE artist = ?2"),
            params![new_artist, artist],
        )?;
    }
    Ok(())
}

/// Has plays, when playlist tracks were added, and drafts follow files that moved, with the
/// library's own rows, so a scan finds each file where it knows it to be.
fn carry_places(tx: &Transaction<'_>, files: &[SavedFile]) -> Result<()> {
    for file in files.iter().filter(|file| file.path != file.path_after) {
        let (from, to) = (text(&file.path), text(&file.path_after));
        for (table, column) in [
            ("plays", "path"),
            ("playlist_added", "path"),
            ("tag_drafts", "path"),
            ("tracks", "path"),
            ("tracks", "source"),
        ] {
            tx.execute(
                &format!("UPDATE OR REPLACE {table} SET {column} = ?1 WHERE {column} = ?2"),
                params![to, from],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
