//! Finding playable files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::cue;

pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "mp2", "flac", "wav", "aif", "aiff", "ogg", "oga", "opus", "m4a", "mp4", "aac", "caf",
    "mka", "webm", "ape",
];

/// Expands folders (recursively, sorted) into the audio files they contain. Files are kept
/// as given. Hidden files and folders are skipped, like macOS's `._name.mp3` companion files.
pub fn collect(paths: &[PathBuf]) -> Vec<PathBuf> {
    collect_matching(paths, is_audio)
}

/// Like [`collect`], but also finds CUE sheets, which cut audio files into tracks; see
/// [`crate::cue::apply_sheets`].
pub fn collect_with_sheets(paths: &[PathBuf]) -> Vec<PathBuf> {
    collect_matching(paths, |path| is_audio(path) || cue::is_sheet(path))
}

fn collect_matching(paths: &[PathBuf], wanted: fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut found = Vec::new();
            walk(path, wanted, &mut found, &mut HashSet::new());
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
) {
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
            walk(&path, wanted, out, visited);
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
