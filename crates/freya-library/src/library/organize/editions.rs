//! Editions of an album: folders that hold the same album, a remaster and a deluxe edition of
//! it, shown as one album of the name they share, one of them the one listed and played. What
//! says so is the release group MusicBrainz puts them in, or a title and tracks in common;
//! folders neither joins stay albums of their own.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use freya_protocol::{AlbumEdition, plain_name};
use rusqlite::{Transaction, params};

use super::carry_album;
use super::names::{album_folder, comparable, parse_folder_name, split_edition};
use crate::Library;
use crate::library::Result;

pub(in crate::library) const SCHEMA: &str = "
    -- The tracks shown as the library's own: of an album's editions, only the chosen one's.
    CREATE VIEW IF NOT EXISTS listed_tracks AS SELECT * FROM tracks WHERE alternate = 0;
    -- Which of an album's editions the user chose, by the folder it is in.
    CREATE TABLE IF NOT EXISTS preferred_editions (
        album_artist TEXT NOT NULL,
        -- The album's title, as `comparable` makes it.
        album TEXT NOT NULL,
        folder TEXT NOT NULL,
        PRIMARY KEY (album_artist, album)
    );
";

/// What share of a smaller edition's tracks must be on the other, to say they are one album's.
const SHARED: f64 = 0.5;

/// One album in one folder.
struct Copy {
    folder: PathBuf,
    artist: String,
    album: String,
    /// The album's title without its edition, as compared.
    base: String,
    release_group: Option<String>,
    edition: Option<String>,
    year: Option<u32>,
    titles: HashSet<String>,
    slots: HashSet<(u32, u32)>,
    tracks: Vec<i64>,
}

impl Copy {
    fn is_edition_of(&self, other: &Copy) -> bool {
        if self.folder == other.folder {
            return false;
        }
        match (&self.release_group, &other.release_group) {
            (Some(a), Some(b)) => return a == b,
            _ if self.base != other.base => return false,
            _ => {}
        }
        // Nothing calling either an edition: not for Freya to say they are.
        if self.edition.is_none() && other.edition.is_none() {
            return false;
        }
        shared(&self.titles, &other.titles) >= SHARED
            || (self.titles.is_empty()
                && other.titles.is_empty()
                && shared(&self.slots, &other.slots) >= SHARED)
    }
}

/// How much of the smaller of two sets is in the other.
fn shared<T: Eq + std::hash::Hash>(a: &HashSet<T>, b: &HashSet<T>) -> f64 {
    let smaller = a.len().min(b.len());
    if smaller == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / smaller as f64
}

/// Finds which of `count` things belong together, given pairs that do.
struct Joined(Vec<usize>);

impl Joined {
    fn new(count: usize) -> Self {
        Self((0..count).collect())
    }

    fn top(&mut self, mut at: usize) -> usize {
        while self.0[at] != at {
            self.0[at] = self.0[self.0[at]];
            at = self.0[at];
        }
        at
    }

    fn join(&mut self, a: usize, b: usize) {
        let (a, b) = (self.top(a), self.top(b));
        self.0[a] = b;
    }
}

/// The album each folder holds, once for each album it has.
fn read_copies(tx: &Transaction<'_>) -> Result<Vec<Copy>> {
    let mut statement = tx.prepare_cached(
        "SELECT id, path, album_artist, album, title, disc, track, edition, release_group_id,
                release_year
         FROM tracks WHERE album_artist IS NOT NULL AND album IS NOT NULL",
    )?;
    let mut rows = statement.query([])?;
    let mut copies: BTreeMap<(PathBuf, String, String), Copy> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let path: String = row.get(1)?;
        let artist: String = row.get(2)?;
        let album: String = row.get(3)?;
        let folder = album_folder(Path::new(&path));
        let (title, in_title) = split_edition(&album);
        let in_name = parse_folder_name(&folder_name(&folder)).edition;
        let copy = copies
            .entry((folder.clone(), artist.clone(), album.clone()))
            .or_insert_with(|| Copy {
                folder,
                artist,
                base: comparable(&title),
                album,
                release_group: None,
                // Its folder's words say more than the title's: "2016 Remastered".
                edition: in_name.or(in_title),
                year: None,
                titles: HashSet::new(),
                slots: HashSet::new(),
                tracks: Vec::new(),
            });
        copy.tracks.push(row.get(0)?);
        if let Some(title) = row.get::<_, Option<String>>(4)? {
            copy.titles.insert(comparable(&title));
        }
        let number: Option<u32> = row.get(6)?;
        if let Some(number) = number {
            let disc: Option<u32> = row.get(5)?;
            copy.slots.insert((disc.unwrap_or(1), number));
        }
        // What the tags call it goes before what the album's title does.
        if let Some(edition) = row.get::<_, Option<String>>(7)? {
            copy.edition = Some(edition);
        }
        copy.release_group = copy.release_group.take().or(row.get(8)?);
        copy.year = copy.year.or(row.get(9)?);
    }
    Ok(copies.into_values().collect())
}

/// Which copies are editions of the same album, those with others only, as indexes.
fn find_albums(copies: &[Copy]) -> Vec<Vec<usize>> {
    let mut by_name: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    let mut by_group: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (at, copy) in copies.iter().enumerate() {
        by_name
            .entry((plain_name(&copy.artist), &copy.base))
            .or_default()
            .push(at);
        if let Some(group) = &copy.release_group {
            by_group
                .entry((plain_name(&copy.artist), group))
                .or_default()
                .push(at);
        }
    }
    let mut joined = Joined::new(copies.len());
    for bucket in by_name.values().chain(by_group.values()) {
        for (i, a) in bucket.iter().enumerate() {
            for b in &bucket[i + 1..] {
                if copies[*a].is_edition_of(&copies[*b]) {
                    joined.join(*a, *b);
                }
            }
        }
    }
    let mut albums: HashMap<usize, Vec<usize>> = HashMap::new();
    for at in 0..copies.len() {
        let top = joined.top(at);
        albums.entry(top).or_default().push(at);
    }
    albums.into_values().filter(|a| a.len() > 1).collect()
}

/// What tells a copy from the album's others.
fn label(copy: &Copy) -> String {
    copy.edition
        .clone()
        .or_else(|| copy.year.map(|year| year.to_string()))
        .unwrap_or_else(|| folder_name(&copy.folder))
}

fn folder_name(folder: &Path) -> String {
    folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl Library {
    /// Finds the library's albums with several editions, lists one of each and has them share
    /// a name. Run after the tracks changed.
    pub fn assign_editions(&mut self) -> Result<()> {
        let tx = self.db.transaction()?;
        let copies = read_copies(&tx)?;
        let albums = find_albums(&copies);
        let mut listed: HashMap<i64, (bool, Option<String>, String)> = HashMap::new();
        for album in &albums {
            let artist = &copies[album[0]].artist;
            let key = copies[album[0]].base.clone();
            let chosen: Option<String> = tx
                .query_row(
                    "SELECT folder FROM preferred_editions WHERE album_artist = ?1 AND album = ?2",
                    params![plain_name(artist), key],
                    |row| row.get(0),
                )
                .ok();
            let preferred = *album
                .iter()
                .find(|at| {
                    Some(copies[**at].folder.to_string_lossy().as_ref()) == chosen.as_deref()
                })
                .or_else(|| {
                    // The album as it came, not its largest edition; of those, the newest.
                    album.iter().min_by_key(|at| {
                        let copy = &copies[**at];
                        (
                            copy.tracks.len(),
                            std::cmp::Reverse(copy.year),
                            copy.folder.clone(),
                        )
                    })
                })
                .expect("an album has editions");
            let name = split_edition(&copies[preferred].album).0;
            for at in album {
                let copy = &copies[*at];
                if copy.album != name {
                    carry_album(
                        &tx,
                        &copy.artist,
                        &copy.album,
                        plain_name(&copy.artist),
                        &name,
                    )?;
                }
                for track in &copy.tracks {
                    listed.insert(*track, (*at != preferred, Some(label(copy)), name.clone()));
                }
            }
        }
        // Only what differs, so an unchanged library is not written to on every scan.
        let current: Vec<(i64, bool)> = tx
            .prepare_cached("SELECT id, alternate FROM tracks WHERE alternate != 0")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_>>()?;
        for (id, _) in current {
            if !listed.contains_key(&id) {
                tx.execute("UPDATE tracks SET alternate = 0 WHERE id = ?1", [id])?;
            }
        }
        for (id, (alternate, edition, name)) in listed {
            tx.execute(
                "UPDATE tracks SET alternate = ?2, edition = ?3, album = ?4
                 WHERE id = ?1 AND (alternate != ?2 OR edition IS NOT ?3 OR album IS NOT ?4)",
                params![id, alternate, edition, name],
            )?;
        }
        tx.commit()
    }

    /// The editions of the album, when it has several.
    pub fn album_editions(
        &self,
        artist: Option<&str>,
        album: Option<&str>,
    ) -> Result<Vec<AlbumEdition>> {
        let mut statement = self.db.prepare_cached(
            "SELECT path, edition, alternate, release_year FROM tracks
             WHERE album_artist IS ?1 AND album IS ?2",
        )?;
        let rows = statement.query_map(params![artist, album], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, Option<u32>>(3)?,
            ))
        })?;
        let mut editions: BTreeMap<PathBuf, AlbumEdition> = BTreeMap::new();
        for row in rows {
            let (path, label, alternate, year) = row?;
            let folder = album_folder(Path::new(&path));
            let edition = editions
                .entry(folder.clone())
                .or_insert_with(|| AlbumEdition {
                    label: label.unwrap_or_else(|| folder_name(&folder)),
                    folder,
                    tracks: 0,
                    year,
                    preferred: !alternate,
                });
            edition.tracks += 1;
        }
        let editions: Vec<AlbumEdition> = editions.into_values().collect();
        Ok(if editions.iter().any(|e| !e.preferred) {
            editions
        } else {
            Vec::new()
        })
    }

    /// Makes the edition in `folder` the one the album shows.
    pub fn set_preferred_edition(
        &mut self,
        artist: &str,
        album: &str,
        folder: &Path,
    ) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO preferred_editions (album_artist, album, folder)
             VALUES (?1, ?2, ?3)",
            params![
                plain_name(artist),
                comparable(album),
                folder.to_string_lossy()
            ],
        )?;
        self.assign_editions()
    }
}

#[cfg(test)]
mod tests;
