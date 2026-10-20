//! The music folders, watched for changes as the system tells of them: what changed in them
//! is scanned once they have been still for a moment, rather than every folder at each start.
//! Network drives may not tell of changes made from other computers; a periodic scan catches
//! those (see [`crate::Service`]).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use freya_library::files::{is_audio, is_image};
use notify::{EventKind, RecursiveMode, Watcher};

/// How long the folders must be still before what changed is scanned: copying an album in
/// tells of each file.
const STILL: Duration = Duration::from_secs(3);
/// And how long changes are gathered at most, while they keep coming.
const LONGEST: Duration = Duration::from_secs(30);

pub struct FolderWatch {
    /// Kept to keep watching; dropped, it stops.
    _watcher: notify::RecommendedWatcher,
    changes: Receiver<PathBuf>,
    /// How many of the folders asked for are watched: those there when it began.
    watching: usize,
    /// The folders where something changed, not scanned yet; when the first and the last
    /// change came.
    changed: Vec<PathBuf>,
    first: Option<Instant>,
    last: Option<Instant>,
}

impl FolderWatch {
    /// Watches `folders` and everything in them; none when the system cannot watch any.
    pub fn new(folders: &[PathBuf]) -> Option<Self> {
        let (tell, changes) = channel();
        let handler = move |event: notify::Result<notify::Event>| {
            let event = match event {
                Ok(event) => event,
                Err(e) => {
                    log::debug!("watching the music folders: {e}");
                    return;
                }
            };
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            for path in event.paths {
                if let Some(folder) = folder_of(&path) {
                    let _ = tell.send(folder);
                }
            }
        };
        let mut watcher = match notify::recommended_watcher(handler) {
            Ok(watcher) => watcher,
            Err(e) => {
                log::warn!("could not watch the music folders: {e}");
                return None;
            }
        };
        let mut watching = 0;
        for folder in folders {
            match watcher.watch(folder, RecursiveMode::Recursive) {
                Ok(()) => watching += 1,
                Err(e) => log::warn!("could not watch {}: {e}", folder.display()),
            }
        }
        log::info!("watching {watching} of {} music folders", folders.len());
        (watching > 0).then(|| Self {
            _watcher: watcher,
            changes,
            watching,
            changed: Vec::new(),
            first: None,
            last: None,
        })
    }

    /// How many of the folders asked for are watched.
    pub fn watching(&self) -> usize {
        self.watching
    }

    /// The folders where something changed, once they have been still for a moment; each
    /// once, and none within another.
    pub fn poll(&mut self) -> Option<Vec<PathBuf>> {
        let now = Instant::now();
        for folder in self.changes.try_iter() {
            self.first.get_or_insert(now);
            self.last = Some(now);
            if !self.changed.contains(&folder) {
                self.changed.push(folder);
            }
        }
        let (first, last) = (self.first?, self.last?);
        if now - last < STILL && now - first < LONGEST {
            return None;
        }
        self.first = None;
        self.last = None;
        Some(outermost(std::mem::take(&mut self.changed)))
    }
}

/// The folder to scan for a change at `path`: the folder itself, or the one a file is in;
/// none for a file the library does not read, such as the system's own.
fn folder_of(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy();
    if name.starts_with('.') {
        return None;
    }
    if path.is_dir() {
        return Some(path.to_owned());
    }
    let read = is_audio(path)
        || is_image(path)
        || path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("cue"))
        // Gone, as a folder taken away is: its parent is looked through.
        || path.extension().is_none();
    read.then(|| path.parent().map(Path::to_owned)).flatten()
}

/// `folders` without those within another of them.
fn outermost(mut folders: Vec<PathBuf>) -> Vec<PathBuf> {
    folders.sort();
    let mut kept: Vec<PathBuf> = Vec::new();
    for folder in folders {
        if !kept.iter().any(|k| folder.starts_with(k)) {
            kept.push(folder);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_each_changed_folder_once() {
        let folders = outermost(vec![
            PathBuf::from("/m/B/Album"),
            PathBuf::from("/m/A"),
            PathBuf::from("/m/A/Album"),
            PathBuf::from("/m/Ab"),
        ]);
        assert_eq!(folders, ["/m/A", "/m/Ab", "/m/B/Album"].map(PathBuf::from));
    }

    #[test]
    fn looks_where_music_changed() {
        let dir = std::env::temp_dir().join(format!("freya-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Album")).unwrap();
        assert_eq!(folder_of(&dir.join("Album")), Some(dir.join("Album")));
        assert_eq!(
            folder_of(&dir.join("Album/01.flac")),
            Some(dir.join("Album"))
        );
        assert_eq!(
            folder_of(&dir.join("Album/cover.JPG")),
            Some(dir.join("Album"))
        );
        assert_eq!(folder_of(&dir.join("Album/.DS_Store")), None);
        assert_eq!(folder_of(&dir.join("Album/notes.txt")), None);
        // Gone: the folder it was in.
        assert_eq!(folder_of(&dir.join("Gone")), Some(dir.clone()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tells_of_a_file_added_once_the_folder_is_still() {
        let dir = std::env::temp_dir().join(format!("freya-watched-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Album")).unwrap();
        let dir = dir.canonicalize().unwrap();
        let Some(mut watch) = FolderWatch::new(std::slice::from_ref(&dir)) else {
            // A system that cannot watch: nothing to tell.
            return;
        };
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(dir.join("Album/01.flac"), b"x").unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let changed = loop {
            if let Some(changed) = watch.poll() {
                break changed;
            }
            assert!(Instant::now() < deadline, "no change was told");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(
            changed.iter().any(|f| dir.join("Album").starts_with(f)),
            "{changed:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
