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
    collect_matching(
        paths,
        is_audio,
        &AtomicBool::new(false),
        &mut |_| {},
        &mut None,
    )
}

/// Like [`collect`], but also finds CUE sheets, which cut audio files into tracks; see
/// [`crate::cue::apply_sheets`].
pub fn collect_with_sheets(paths: &[PathBuf]) -> Vec<PathBuf> {
    collect_with_sheets_until(paths, &AtomicBool::new(false))
}

/// Like [`collect_with_sheets`], but gives up, with what it found so far, once `cancel` is
/// set: walking a large or slow drive can take minutes.
pub fn collect_with_sheets_until(paths: &[PathBuf], cancel: &AtomicBool) -> Vec<PathBuf> {
    collect_matching(
        paths,
        |path| is_audio(path) || cue::is_sheet(path),
        cancel,
        &mut |_| {},
        &mut None,
    )
}

/// What a walk through the music's folders found: audio files and CUE sheets, and the
/// pictures beside them, which covers may be.
#[derive(Debug, Default)]
pub struct Walked {
    pub files: Vec<PathBuf>,
    pub images: Vec<PathBuf>,
}

/// Like [`collect_with_sheets_until`], with the pictures in the folders too, telling
/// `entering` of each folder as it is walked: the one walk a scan makes through them.
pub fn walk_library(
    paths: &[PathBuf],
    cancel: &AtomicBool,
    entering: &mut dyn FnMut(&Path),
) -> Walked {
    let mut walked = Walked::default();
    let mut images = Some(&mut walked.images);
    let files = collect_matching(
        paths,
        |path| is_audio(path) || cue::is_sheet(path),
        cancel,
        entering,
        &mut images,
    );
    walked.files = files;
    walked
}

fn collect_matching(
    paths: &[PathBuf],
    wanted: fn(&Path) -> bool,
    cancel: &AtomicBool,
    entering: &mut dyn FnMut(&Path),
    images: &mut Option<&mut Vec<PathBuf>>,
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
                images,
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
    images: &mut Option<&mut Vec<PathBuf>>,
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
            walk(&path, wanted, out, visited, cancel, entering, images);
        } else if wanted(&path) {
            out.push(path);
        } else if let Some(images) = images
            && is_image(&path)
        {
            images.push(path);
        }
    }
}

/// A picture a cover may be: JPEG or PNG.
pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["jpg", "jpeg", "png"].contains(&e.to_ascii_lowercase().as_str()))
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

/// How much of each end of a file its fingerprint reads.
const FINGERPRINT_CHUNK: u64 = 64 * 1024;

/// A fingerprint of the file at `path` that stays the same when the file is moved or renamed:
/// its size and the sum of its first and last 64 KiB as 64-bit words, in 16 hex digits. Quick
/// to take, as only those are read.
pub fn fingerprint(path: &Path) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let mut sum = size;
    let mut add = |file: &mut std::fs::File, length: u64| -> std::io::Result<()> {
        let mut chunk = vec![0; length as usize];
        file.read_exact(&mut chunk)?;
        for word in chunk.chunks(8) {
            let mut bytes = [0; 8];
            bytes[..word.len()].copy_from_slice(word);
            sum = sum.wrapping_add(u64::from_le_bytes(bytes));
        }
        Ok(())
    };
    let head = size.min(FINGERPRINT_CHUNK);
    add(&mut file, head)?;
    let tail = size.saturating_sub(FINGERPRINT_CHUNK).max(head);
    file.seek(SeekFrom::Start(tail))?;
    add(&mut file, size - tail)?;
    Ok(format!("{sum:016x}"))
}

#[cfg(test)]
mod fingerprint_tests {
    use super::*;

    #[test]
    fn a_fingerprint_follows_the_content_not_the_name() {
        let dir = std::env::temp_dir().join(format!("freya-fingerprint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let content: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.join("a.flac"), &content).unwrap();
        std::fs::write(dir.join("b.flac"), &content).unwrap();
        let mut other = content.clone();
        other[10] ^= 1;
        std::fs::write(dir.join("c.flac"), &other).unwrap();
        std::fs::write(dir.join("small.flac"), b"tiny").unwrap();

        let a = fingerprint(&dir.join("a.flac")).unwrap();
        assert_eq!(a.len(), 16);
        assert_eq!(a, fingerprint(&dir.join("b.flac")).unwrap());
        assert_ne!(a, fingerprint(&dir.join("c.flac")).unwrap());
        assert!(fingerprint(&dir.join("small.flac")).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
