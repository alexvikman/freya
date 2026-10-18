//! Finding playable files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::cue;

pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "mp2", "flac", "wav", "aif", "aiff", "ogg", "oga", "opus", "m4a", "mp4", "aac", "caf",
    "mka", "webm", "ape",
];

/// Expands folders (recursively, sorted) into the audio files they contain. Files are kept
/// as given. Hidden files and folders are skipped, like macOS's `._name.mp3` companion files.
pub fn collect(paths: &[PathBuf]) -> Vec<PathBuf> {
    collect_matching(paths, is_audio, &AtomicBool::new(false), &mut |_| {})
}

/// Like [`collect`], but also finds CUE sheets, which cut audio files into tracks; see
/// [`crate::cue::apply_sheets`].
pub fn collect_with_sheets(paths: &[PathBuf]) -> Vec<PathBuf> {
    collect_with_sheets_until(paths, &AtomicBool::new(false))
}

/// Like [`collect_with_sheets`], but gives up, with what it found so far, once `cancel` is
/// set: walking a large or slow drive can take minutes.
pub fn collect_with_sheets_until(paths: &[PathBuf], cancel: &AtomicBool) -> Vec<PathBuf> {
    collect_with_sheets_watched(paths, cancel, &mut |_| {})
}

/// Like [`collect_with_sheets_until`], telling `entering` of each folder as it is walked.
pub fn collect_with_sheets_watched(
    paths: &[PathBuf],
    cancel: &AtomicBool,
    entering: &mut dyn FnMut(&Path),
) -> Vec<PathBuf> {
    collect_matching(
        paths,
        |path| is_audio(path) || cue::is_sheet(path),
        cancel,
        entering,
    )
}

fn collect_matching(
    paths: &[PathBuf],
    wanted: fn(&Path) -> bool,
    cancel: &AtomicBool,
    entering: &mut dyn FnMut(&Path),
) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut found = Vec::new();
            walk(
                path,
                wanted,
                &mut found,
                &mut HashSet::new(),
                cancel,
                entering,
            );
            found.sort();
            files.extend(found);
        } else {
            files.push(path.clone());
        }
    }
    files
}

/// `visited` holds the folders already walked, so links that lead back up the tree end.
fn walk(
    dir: &Path,
    wanted: fn(&Path) -> bool,
    out: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
    cancel: &AtomicBool,
    entering: &mut dyn FnMut(&Path),
) {
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    entering(dir);
    let Ok(real) = dir.canonicalize() else {
        return;
    };
    if !visited.insert(real) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            walk(&path, wanted, out, visited, cancel, entering);
        } else if wanted(&path) {
            out.push(path);
        }
    }
}

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_walk_stops() {
        // It could not be stopped, and the window waited for it when the library folders
        // changed during a scan of a large drive.
        let dir = std::env::temp_dir().join(format!("freya-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["a", "b/c"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            std::fs::write(dir.join(sub).join("x.mp3"), b"").unwrap();
        }
        let folders = [dir.clone()];
        assert_eq!(collect_with_sheets(&folders).len(), 2);
        assert!(collect_with_sheets_until(&folders, &AtomicBool::new(true)).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
