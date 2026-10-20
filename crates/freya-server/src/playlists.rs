//! The user's playlists, as the server keeps them: an M3U file each in its folder, named after
//! the playlist, so the files are also easy to find and share.

use std::path::{Path, PathBuf};

use freya_library::Entry;
use freya_library::playlist_file;
use freya_protocol::{PlaylistEntry, SavedPlaylist};

/// Characters a file name cannot hold on some system, so a playlist's name cannot either.
pub const FORBIDDEN: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];
const EXTENSION: &str = "m3u8";

/// The playlists in a folder.
pub struct Playlists {
    dir: PathBuf,
}

impl Playlists {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Every playlist, by name.
    pub fn all(&self) -> Vec<SavedPlaylist> {
        let Ok(files) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut all: Vec<SavedPlaylist> = files
            .flatten()
            .map(|file| file.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == EXTENSION))
            .filter_map(|path| {
                let name = path.file_stem()?.to_str()?.to_owned();
                match playlist_file::load(&path) {
                    Ok(entries) => Some(SavedPlaylist {
                        name,
                        created: created(&path),
                        entries: entries.iter().map(to_wire).collect(),
                    }),
                    Err(e) => {
                        log::warn!("could not read the playlist {}: {e}", path.display());
                        None
                    }
                }
            })
            .collect();
        all.sort_by_key(|p| p.name.to_lowercase());
        all
    }

    /// Keeps the playlist `name` with `entries`.
    pub fn save(&self, name: &str, entries: &[PlaylistEntry]) -> Result<(), String> {
        let file = self.file(name)?;
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let entries: Vec<Entry> = entries.iter().map(from_wire).collect();
        playlist_file::save(&file, &entries).map_err(|e| e.to_string())
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        let (from, to) = (self.file(from)?, self.file(to)?);
        std::fs::rename(&from, &to).map_err(|e| e.to_string())
    }

    pub fn delete(&self, name: &str) -> Result<(), String> {
        std::fs::remove_file(self.file(name)?).map_err(|e| e.to_string())
    }

    /// The file of the playlist `name`; an error for a name no file can have, which would
    /// lead out of the folder.
    fn file(&self, name: &str) -> Result<PathBuf, String> {
        let usable = !name.trim().is_empty() && !name.starts_with('.') && !name.contains(FORBIDDEN);
        if !usable {
            return Err(format!("no playlist can be called {name:?}"));
        }
        Ok(self.dir.join(format!("{name}.{EXTENSION}")))
    }
}

/// When the file at `path` was made, else last changed.
fn created(path: &Path) -> Option<std::time::SystemTime> {
    let metadata = std::fs::metadata(path).ok()?;
    metadata.created().or_else(|_| metadata.modified()).ok()
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

fn from_wire(entry: &PlaylistEntry) -> Entry {
    Entry {
        part: entry.part,
        title: entry.title.clone(),
        artist: entry.artist.clone(),
        album: entry.album.clone(),
        duration: entry.duration,
        ..Entry::new(entry.path.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, title: &str) -> PlaylistEntry {
        PlaylistEntry {
            path: path.into(),
            part: None,
            title: Some(title.into()),
            artist: Some("Freya".into()),
            album: None,
            duration: Some(std::time::Duration::from_secs(61)),
        }
    }

    #[test]
    fn keeps_playlists_as_files_in_name_order() {
        let dir =
            std::env::temp_dir().join(format!("freya-server-playlists-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let playlists = Playlists::new(dir.clone());
        assert!(playlists.all().is_empty());
        playlists.save("road", &[entry("/m/a.flac", "A")]).unwrap();
        playlists
            .save("Calm", &[entry("/m/b.flac", "B"), entry("/m/c.flac", "C")])
            .unwrap();
        let all = playlists.all();
        let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Calm", "road"]);
        assert_eq!(all[1].entries, [entry("/m/a.flac", "A")]);
        assert!(all[0].created.is_some());
        assert!(dir.join("Calm.m3u8").is_file());

        playlists.rename("road", "Road trip").unwrap();
        playlists.delete("Calm").unwrap();
        let names: Vec<String> = playlists.all().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Road trip"]);
        // No name leads out of the folder.
        assert!(playlists.save("../out", &[]).is_err());
        assert!(playlists.delete("..").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
