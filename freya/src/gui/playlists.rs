//! Playlists the user keeps in Freya, which the server keeps for it (see
//! `freya_server::playlists`). Up Next, the queue, is not one of them: it is the app's
//! `Playlist`.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use freya_media::Entry;
use freya_protocol::{PlaylistEntry, Server};
use slint_interpreter::{Struct, Value};

use super::skin_state::{format_time, string};

/// Characters a file name cannot hold on some system, so a playlist's name cannot either: the
/// server keeps each as a file named after it.
const FORBIDDEN: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

pub struct SavedPlaylist {
    pub name: String,
    pub entries: Vec<Entry>,
    /// When it was made.
    created: Option<SystemTime>,
}

pub struct Playlists {
    /// The server that keeps them; without one, they are kept only while Freya runs.
    server: Option<Arc<dyn Server>>,
    /// By name, ignoring case.
    list: Vec<SavedPlaylist>,
    /// Goes up with every change, so the skin's rows are only rebuilt after one.
    version: u64,
}

impl Playlists {
    /// None yet: they come from the server once connected.
    pub fn new() -> Self {
        Self {
            server: None,
            list: Vec::new(),
            version: 0,
        }
    }

    /// Takes the playlists `server` keeps, which keeps them from now on.
    /// Keeps the playlists of `server`, which are `saved`, as it said when connected to.
    pub fn connect(
        &mut self,
        server: Arc<dyn Server>,
        saved: freya_protocol::ApiResult<Vec<freya_protocol::SavedPlaylist>>,
    ) {
        self.server = Some(server);
        self.keep(saved);
    }

    /// Reads the playlists from the server again: another client changed them.
    pub fn reload(&mut self) {
        let Some(server) = &self.server else {
            return;
        };
        let saved = server.playlists();
        self.keep(saved);
    }

    fn keep(&mut self, saved: freya_protocol::ApiResult<Vec<freya_protocol::SavedPlaylist>>) {
        let saved = match saved {
            Ok(saved) => saved,
            Err(e) => {
                log::warn!("could not read the playlists: {e}");
                return;
            }
        };
        self.list = saved
            .into_iter()
            .map(|playlist| SavedPlaylist {
                name: playlist.name,
                entries: playlist.entries.into_iter().map(from_wire).collect(),
                created: playlist.created,
            })
            .collect();
        self.list.sort_by_key(|p| p.name.to_lowercase());
        self.version += 1;
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

    /// When the `index`th playlist was made.
    pub fn created(&self, index: usize) -> Option<SystemTime> {
        self.list.get(index)?.created
    }

    /// Where the playlist named `name` is, ignoring case.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        let name = name.to_lowercase();
        self.list.iter().position(|p| p.name.to_lowercase() == name)
    }

    /// Makes a playlist of `entries`. Returns its index.
    pub fn create(&mut self, name: &str, entries: Vec<Entry>) -> Result<usize, String> {
        let name = self.check_name(name, None)?;
        let playlist = SavedPlaylist {
            name,
            entries: entries.into_iter().map(unselected).collect(),
            created: Some(SystemTime::now()),
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
        let (from, to) = (playlist.name.clone(), name.clone());
        self.tell(move |server| {
            if let Err(e) = server.rename_saved_playlist(&from, &to) {
                log::warn!("could not rename the playlist {from}: {e}");
            }
        });
        playlist.name = name;
        Ok(self.insert(playlist))
    }

    pub fn delete(&mut self, index: usize) {
        if index >= self.list.len() {
            return;
        }
        self.version += 1;
        let playlist = self.list.remove(index);
        self.tell(move |server| {
            if let Err(e) = server.delete_playlist(&playlist.name) {
                log::warn!("could not delete the playlist {}: {e}", playlist.name);
            }
        });
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

    fn save(&self, playlist: &SavedPlaylist) {
        let entries: Vec<PlaylistEntry> = playlist.entries.iter().map(to_wire).collect();
        let name = playlist.name.clone();
        self.tell(move |server| {
            if let Err(e) = server.save_playlist(&name, &entries) {
                log::warn!("could not save the playlist {name}: {e}");
            }
        });
    }

    /// Tells the server what changed, the window not waiting for it: the list here is
    /// changed already, and the server is told in order.
    fn tell(&self, tell: impl FnOnce(&dyn Server) + Send + 'static) {
        if let Some(server) = self.server.clone() {
            crate::errands::run(move || tell(server.as_ref()));
        }
    }
}

fn to_wire(entry: &Entry) -> PlaylistEntry {
    PlaylistEntry {
        path: entry.path.clone(),
        part: entry.part,
        title: entry.title.clone(),
        artist: entry.artist.clone(),
        album: entry.album.clone(),
        duration: entry.duration,
    }
}

fn from_wire(entry: PlaylistEntry) -> Entry {
    Entry {
        part: entry.part,
        title: entry.title,
        artist: entry.artist,
        album: entry.album,
        duration: entry.duration,
        ..Entry::new(entry.path)
    }
}

/// An entry as kept in a playlist: selection belongs to the list it is shown in.
fn unselected(mut entry: Entry) -> Entry {
    entry.selected = false;
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(playlists: &Playlists) -> Vec<&str> {
        playlists.list.iter().map(|p| p.name.as_str()).collect()
    }

    fn entry(path: &str) -> Entry {
        let mut entry = Entry::new(path);
        entry.selected = true;
        entry
    }

    #[test]
    fn keeps_playlists_in_name_order() {
        let mut playlists = Playlists::new();
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
        assert_eq!(names(&playlists), ["Calm evenings", "Road trip"]);
        let road = playlists.get(1).unwrap();
        assert_eq!(road.entries.len(), 1);
        assert!(!road.entries[0].selected, "selection is not kept");
        assert!(playlists.created(1).is_some());
    }

    #[test]
    fn names_must_be_new_and_usable_as_file_names() {
        let mut playlists = Playlists::new();
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
