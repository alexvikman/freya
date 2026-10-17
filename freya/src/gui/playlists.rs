//! Playlists the user keeps in Freya. Each is an M3U file in Freya's data folder, named after
//! the playlist, so the files are also easy to find and share. Up Next, the queue, is not one
//! of them: it is the app's `Playlist`.

use std::path::PathBuf;
use std::time::Duration;

use freya_library::{Entry, playlist_file};
use slint_interpreter::{Struct, Value};

use super::skin_state::{format_time, string};

/// Characters a file name cannot hold on some system, so a playlist's name cannot either.
const FORBIDDEN: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];
const EXTENSION: &str = "m3u8";

pub struct SavedPlaylist {
    pub name: String,
    pub entries: Vec<Entry>,
}

pub struct Playlists {
    /// Where the files are; `None` keeps playlists only while Freya runs.
    dir: Option<PathBuf>,
    /// By name, ignoring case.
    list: Vec<SavedPlaylist>,
    /// Goes up with every change, so the skin's rows are only rebuilt after one.
    version: u64,
}

impl Playlists {
    /// The playlists in `dir`.
    pub fn load(dir: Option<PathBuf>) -> Self {
        let mut list: Vec<SavedPlaylist> = dir
            .as_deref()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .into_iter()
            .flatten()
            .flatten()
            .map(|file| file.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == EXTENSION))
            .filter_map(|path| {
                let name = path.file_stem()?.to_str()?.to_owned();
                match playlist_file::load(&path) {
                    Ok(entries) => Some(SavedPlaylist { name, entries }),
                    Err(e) => {
                        log::warn!("could not read the playlist {}: {e}", path.display());
                        None
                    }
                }
            })
            .collect();
        list.sort_by_key(|p| p.name.to_lowercase());
        Self {
            dir,
            list,
            version: 0,
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn get(&self, index: usize) -> Option<&SavedPlaylist> {
        self.list.get(index)
    }

    /// Makes a playlist of `entries`. Returns its index.
    pub fn create(&mut self, name: &str, entries: Vec<Entry>) -> Result<usize, String> {
        let name = self.check_name(name, None)?;
        let playlist = SavedPlaylist {
            name,
            entries: entries.into_iter().map(unselected).collect(),
        };
        self.save(&playlist);
        Ok(self.insert(playlist))
    }

    /// Renames a playlist. Returns its new index: the list stays in name order.
    pub fn rename(&mut self, index: usize, name: &str) -> Result<usize, String> {
        let name = self.check_name(name, Some(index))?;
        if index >= self.list.len() {
            return Err("No such playlist".to_owned());
        }
        let mut playlist = self.list.remove(index);
        if let (Some(old), Some(new)) = (self.file(&playlist.name), self.file(&name))
            && let Err(e) = std::fs::rename(&old, &new)
        {
            log::warn!("could not rename {}: {e}", old.display());
        }
        playlist.name = name;
        Ok(self.insert(playlist))
    }

    pub fn delete(&mut self, index: usize) {
        if index >= self.list.len() {
            return;
        }
        self.version += 1;
        let playlist = self.list.remove(index);
        if let Some(file) = self.file(&playlist.name)
            && let Err(e) = std::fs::remove_file(&file)
        {
            log::warn!("could not delete {}: {e}", file.display());
        }
    }

    /// Adds `entries` at the end of a playlist.
    pub fn add(&mut self, index: usize, entries: Vec<Entry>) {
        let Some(playlist) = self.list.get_mut(index) else {
            return;
        };
        playlist.entries.extend(entries.into_iter().map(unselected));
        self.version += 1;
        let playlist = &self.list[index];
        self.save(playlist);
    }

    /// Removes one track from a playlist.
    pub fn remove_track(&mut self, index: usize, row: usize) {
        let Some(playlist) = self.list.get_mut(index) else {
            return;
        };
        if row < playlist.entries.len() {
            playlist.entries.remove(row);
            self.version += 1;
            let playlist = &self.list[index];
            self.save(playlist);
        }
    }

    /// `PlaylistRow`s for the navigation.
    pub fn rows(&self) -> Vec<Value> {
        self.list
            .iter()
            .map(|playlist| {
                Value::Struct(Struct::from_iter([
                    ("name".to_owned(), string(&playlist.name)),
                    (
                        "count".to_owned(),
                        string(playlist.entries.len().to_string()),
                    ),
                ]))
            })
            .collect()
    }

    /// "12 tracks · 48:10" for a playlist.
    pub fn summary(&self, index: usize) -> String {
        let Some(playlist) = self.list.get(index) else {
            return String::new();
        };
        let n = playlist.entries.len();
        let tracks = format!("{n} track{}", if n == 1 { "" } else { "s" });
        let total: Duration = playlist.entries.iter().filter_map(|e| e.duration).sum();
        if total.is_zero() {
            tracks
        } else {
            format!("{tracks} · {}", format_time(total))
        }
    }

    /// A playlist's name as it will be kept: trimmed, not empty, a possible file name, and
    /// not another playlist's (`except` is the playlist being renamed).
    fn check_name(&self, name: &str, except: Option<usize>) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Give the playlist a name".to_owned());
        }
        if name.starts_with('.') || name.contains(FORBIDDEN) {
            return Err(format!(
                "A name cannot start with a dot or hold any of {}",
                FORBIDDEN.iter().collect::<String>()
            ));
        }
        let taken = self
            .list
            .iter()
            .enumerate()
            .any(|(i, p)| Some(i) != except && p.name.to_lowercase() == name.to_lowercase());
        if taken {
            return Err(format!("There is already a playlist called {name}"));
        }
        Ok(name.to_owned())
    }

    fn insert(&mut self, playlist: SavedPlaylist) -> usize {
        self.version += 1;
        let key = playlist.name.to_lowercase();
        let index = self.list.partition_point(|p| p.name.to_lowercase() < key);
        self.list.insert(index, playlist);
        index
    }

    fn file(&self, name: &str) -> Option<PathBuf> {
        self.dir
            .as_deref()
            .map(|dir| dir.join(format!("{name}.{EXTENSION}")))
    }

    fn save(&self, playlist: &SavedPlaylist) {
        let Some(file) = self.file(&playlist.name) else {
            return;
        };
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = playlist_file::save(&file, &playlist.entries) {
            log::warn!("could not save the playlist {}: {e}", file.display());
        }
    }
}

/// An entry as kept in a playlist: selection belongs to the list it is shown in.
fn unselected(mut entry: Entry) -> Entry {
    entry.selected = false;
    entry
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("freya-playlists-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn names(playlists: &Playlists) -> Vec<&str> {
        playlists.list.iter().map(|p| p.name.as_str()).collect()
    }

    fn entry(path: &str) -> Entry {
        let mut entry = Entry::new(path);
        entry.selected = true;
        entry
    }

    #[test]
    fn keeps_playlists_as_files_in_name_order() {
        let dir = temp_dir("files");
        let mut playlists = Playlists::load(Some(dir.clone()));
        assert_eq!(playlists.len(), 0);

        assert_eq!(
            playlists.create("Road trip", vec![entry("/m/a.flac")]),
            Ok(0)
        );
        assert_eq!(playlists.create("calm", Vec::new()), Ok(0));
        playlists.add(1, vec![entry("/m/b.flac")]);
        playlists.remove_track(1, 0);
        assert_eq!(playlists.rename(0, "Calm evenings"), Ok(0));
        assert_eq!(playlists.create("Workout", Vec::new()), Ok(2));
        playlists.delete(2);

        let again = Playlists::load(Some(dir.clone()));
        assert_eq!(names(&again), ["Calm evenings", "Road trip"]);
        let road = again.get(1).unwrap();
        assert_eq!(road.entries.len(), 1);
        assert_eq!(road.entries[0].path, Path::new("/m/b.flac"));
        assert!(!road.entries[0].selected, "selection is not kept");
        let files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|f| f.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == EXTENSION))
            .collect();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn names_must_be_new_and_usable_as_file_names() {
        let mut playlists = Playlists::load(None);
        playlists.create("Calm", Vec::new()).unwrap();
        assert_eq!(
            playlists.create("  ", Vec::new()),
            Err("Give the playlist a name".to_owned())
        );
        assert_eq!(
            playlists.create("calm", Vec::new()),
            Err("There is already a playlist called calm".to_owned())
        );
        assert!(playlists.create("AC/DC", Vec::new()).is_err());
        assert!(playlists.create(".hidden", Vec::new()).is_err());
        assert_eq!(
            playlists.rename(0, "CALM"),
            Ok(0),
            "a playlist may keep its own name"
        );
        assert_eq!(playlists.summary(0), "0 tracks");
    }
}
