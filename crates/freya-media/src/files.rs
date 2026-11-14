//! Finding playable files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;

use crate::cue;

/// Size and modification time of a file, to tell whether it changed since it was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub size: i64,
    /// Milliseconds since 1970.
    pub modified: i64,
}

impl FileStamp {
    pub fn of(metadata: &std::fs::Metadata) -> Self {
        let modified = metadata.modified().map_or(0, |time| {
            time.duration_since(std::time::SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64)
        });
        Self {
            size: metadata.len() as i64,
            modified,
        }
    }
}

pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "mp2", "flac", "wav", "aif", "aiff", "ogg", "oga", "opus", "m4a", "mp4", "aac", "caf",
    "mka", "webm", "ape",
];

/// Folders are read by up to this many threads at once: on a network drive, reading a folder
/// and each file's size and time is mostly waiting for an answer, and the waits overlap.
const WALKERS: usize = 16;

/// Expands folders (recursively, sorted) into the audio files they contain. Files are kept
/// as given. Hidden files and folders are skipped, like macOS's `._name.mp3` companion files.
pub fn collect(paths: &[PathBuf]) -> Vec<PathBuf> {
    let want = Want {
        files: is_audio,
        images: false,
        stamps: false,
        given: true,
    };
    collect_matching(paths, &want, &AtomicBool::new(false), &mut |_| {}).files
}

/// Like [`collect`], but also finds CUE sheets, which cut audio files into tracks; see
/// [`crate::cue::apply_sheets`].
pub fn collect_with_sheets(paths: &[PathBuf]) -> Vec<PathBuf> {
    collect_with_sheets_until(paths, &AtomicBool::new(false))
}

/// Like [`collect_with_sheets`], but gives up, with what it found so far, once `cancel` is
/// set: walking a large or slow drive can take minutes.
pub fn collect_with_sheets_until(paths: &[PathBuf], cancel: &AtomicBool) -> Vec<PathBuf> {
    let want = Want {
        files: is_audio_or_sheet,
        images: false,
        stamps: false,
        given: true,
    };
    collect_matching(paths, &want, cancel, &mut |_| {}).files
}

/// What a walk through the music's folders found: audio files and CUE sheets, and the
/// pictures beside them, which covers may be.
#[derive(Debug, Default)]
pub struct Walked {
    pub files: Vec<PathBuf>,
    pub images: Vec<PathBuf>,
    /// The size and time of each file and picture found in a folder, taken as it was walked.
    pub stamps: HashMap<PathBuf, FileStamp>,
}

/// Like [`collect_with_sheets_until`], with the pictures in the folders and the stamps of
/// what was found too, telling `entering` of each folder as it is walked: the one walk a scan
/// makes through them. `paths` are walked, each only when it is within `library`, the music's
/// folders, after links: a scan of one folder of the library's, which may itself be a link
/// someone put there, leaves the library no more than a scan of it all does.
pub fn walk_library(
    paths: &[PathBuf],
    library: &[PathBuf],
    cancel: &AtomicBool,
    entering: &mut dyn FnMut(&Path),
) -> Walked {
    let want = Want {
        files: is_audio_or_sheet,
        images: true,
        stamps: true,
        given: false,
    };
    collect_within(paths, library, &want, cancel, entering)
}

fn is_audio_or_sheet(path: &Path) -> bool {
    is_audio(path) || cue::is_sheet(path)
}

/// What a walk looks for.
struct Want {
    files: fn(&Path) -> bool,
    /// The pictures too.
    images: bool,
    /// The size and time of what it finds.
    stamps: bool,
    /// Whether what is given that is no folder is kept as given: a file the user names, as
    /// for the command line's player. Not for a scan, which is given folders: what is no
    /// folder when it is walked, as one taken away meanwhile or a link put in its place, is
    /// not read as a file.
    given: bool,
}

fn collect_matching(
    paths: &[PathBuf],
    want: &Want,
    cancel: &AtomicBool,
    entering: &mut dyn FnMut(&Path),
) -> Walked {
    collect_within(paths, paths, want, cancel, entering)
}

/// What `want` asks for in `paths`, each walked only where it is within `within` after links.
fn collect_within(
    paths: &[PathBuf],
    within: &[PathBuf],
    want: &Want,
    cancel: &AtomicBool,
    entering: &mut dyn FnMut(&Path),
) -> Walked {
    let mut walked = Walked::default();
    // Links are followed only within what is walked: one out of the music, put in a folder
    // others can write to, does not bring the rest of the computer into the library. Nor is
    // what is walked itself anywhere but within `within`, wherever a link among it leads.
    let within: Vec<PathBuf> = within
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    let roots: Vec<PathBuf> = paths
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .filter(|real| within.iter().any(|within| real.starts_with(within)))
        .collect();
    for path in paths {
        if path.is_dir() {
            let mut files = Vec::new();
            walk(
                path,
                want,
                &roots,
                cancel,
                entering,
                &mut files,
                &mut walked,
            );
            files.sort();
            walked.files.extend(files);
        } else if want.given {
            walked.files.push(path.clone());
        }
    }
    walked.images.sort();
    walked
}

/// A folder to read, and where it really is, to know links that lead back up the tree.
struct Folder {
    path: PathBuf,
    real: PathBuf,
}

/// What is in one folder.
#[derive(Default)]
struct Read {
    folders: Vec<Folder>,
    files: Vec<PathBuf>,
    images: Vec<PathBuf>,
    stamps: Vec<(PathBuf, FileStamp)>,
}

/// Walks `root` a level at a time, the folders of each level read side by side, into `files`
/// and `walked`'s pictures and stamps.
fn walk(
    root: &Path,
    want: &Want,
    roots: &[PathBuf],
    cancel: &AtomicBool,
    entering: &mut dyn FnMut(&Path),
    files: &mut Vec<PathBuf>,
    walked: &mut Walked,
) {
    let Ok(real) = root.canonicalize() else {
        return;
    };
    // Where the walk starts is itself within what it may read, after links.
    if !roots.iter().any(|within| real.starts_with(within)) {
        return;
    }
    // The folders already walked, so links that lead back up the tree end.
    let mut visited = HashSet::from([real.clone()]);
    let mut level = vec![Folder {
        path: root.to_owned(),
        real,
    }];
    while !level.is_empty() && !cancel.load(Ordering::Relaxed) {
        let mut below = Vec::new();
        let next = AtomicUsize::new(0);
        std::thread::scope(|threads| {
            let (send, reads) = channel();
            for _ in 0..WALKERS.min(level.len()) {
                let send = send.clone();
                let (level, next) = (&level, &next);
                threads.spawn(move || {
                    while !cancel.load(Ordering::Relaxed) {
                        let Some(folder) = level.get(next.fetch_add(1, Ordering::Relaxed)) else {
                            break;
                        };
                        if send
                            .send((folder, read_folder(folder, want, roots)))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
            drop(send);
            for (folder, read) in reads {
                entering(&folder.path);
                files.extend(read.files);
                walked.images.extend(read.images);
                walked.stamps.extend(read.stamps);
                below.extend(
                    read.folders
                        .into_iter()
                        .filter(|sub| visited.insert(sub.real.clone())),
                );
            }
        });
        level = below;
    }
}

/// Reads `folder`: what `want` asks for in it, and the folders in it; a link only to what is
/// within `roots`.
fn read_folder(folder: &Folder, want: &Want, roots: &[PathBuf]) -> Read {
    let mut read = Read::default();
    let Ok(entries) = std::fs::read_dir(&folder.path) else {
        return read;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        // The folder's listing tells what each entry is, without asking again; a link is
        // followed to what it leads to.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let linked = kind.is_symlink();
        if linked {
            let within = path
                .canonicalize()
                .is_ok_and(|real| roots.iter().any(|root| real.starts_with(root)));
            if !within {
                continue;
            }
        }
        if (linked && path.is_dir()) || kind.is_dir() {
            let real = if linked {
                let Ok(real) = path.canonicalize() else {
                    continue;
                };
                real
            } else {
                folder.real.join(&name)
            };
            read.folders.push(Folder { path, real });
            continue;
        }
        let file = (want.files)(&path);
        if !file && !(want.images && is_image(&path)) {
            continue;
        }
        // A file, or a link to one: not a FIFO or a device named as a song, which reading
        // would wait on for ever.
        if !(kind.is_file() || (linked && path.is_file())) {
            continue;
        }
        if want.stamps {
            // Right after the listing, while a network drive still has it at hand.
            let metadata = if linked {
                std::fs::metadata(&path)
            } else {
                entry.metadata()
            };
            if let Ok(metadata) = metadata {
                read.stamps.push((path.clone(), FileStamp::of(&metadata)));
            }
        }
        if file {
            read.files.push(path);
        } else {
            read.images.push(path);
        }
    }
    read
}

/// `f` of each of `items`, in their order, on up to [`WALKERS`] threads: for work that is
/// mostly waiting for a drive.
pub(crate) fn side_by_side<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let mut done: Vec<Option<R>> = std::iter::repeat_with(|| None).take(items.len()).collect();
    std::thread::scope(|threads| {
        let (send, results) = channel();
        for _ in 0..WALKERS.min(items.len()) {
            let send = send.clone();
            let (next, f) = (&next, &f);
            threads.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else {
                        break;
                    };
                    if send.send((i, f(item))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(send);
        for (i, result) in results {
            done[i] = Some(result);
        }
    });
    done.into_iter().flatten().collect()
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

    #[cfg(unix)]
    #[test]
    fn a_walk_follows_links_within_the_music_but_not_back_up_and_stamps_what_it_finds() {
        let dir = std::env::temp_dir().join(format!("freya-walk-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (music, elsewhere) = (dir.join("music"), dir.join("elsewhere"));
        for sub in ["b/deep", "a", ".hidden"] {
            std::fs::create_dir_all(music.join(sub)).unwrap();
        }
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(music.join("b/deep/2.flac"), b"two").unwrap();
        std::fs::write(music.join("b/deep/cover.jpg"), b"jpeg").unwrap();
        std::fs::write(music.join("a/1.mp3"), b"one").unwrap();
        std::fs::write(music.join(".hidden/3.mp3"), b"").unwrap();
        std::fs::write(music.join("a/notes.txt"), b"").unwrap();
        std::fs::write(elsewhere.join("4.ogg"), b"four").unwrap();
        std::os::unix::fs::symlink(&music, music.join("b/up")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, music.join("linked")).unwrap();
        std::os::unix::fs::symlink(elsewhere.join("4.ogg"), music.join("a/5.ogg")).unwrap();
        std::os::unix::fs::symlink(music.join("a/1.mp3"), music.join("b/6.mp3")).unwrap();

        let mut entered = 0;
        let walked = walk_library(
            std::slice::from_ref(&music),
            std::slice::from_ref(&music),
            &AtomicBool::new(false),
            &mut |_| entered += 1,
        );
        assert_eq!(
            walked.files,
            [
                music.join("a/1.mp3"),
                music.join("b/6.mp3"),
                music.join("b/deep/2.flac"),
            ],
            "links out of the music are not followed"
        );
        assert_eq!(walked.images, [music.join("b/deep/cover.jpg")]);
        assert_eq!(entered, 4, "music, a, b, deep; not up again, nor out");
        assert_eq!(walked.stamps.len(), 4);
        assert_eq!(walked.stamps[&music.join("b/deep/2.flac")].size, 3);

        // One folder of the music's scanned alone, as when something changed in it: a link
        // out of the music is not walked for being where the scan starts.
        let library = std::slice::from_ref(&music);
        let walk = |folder: &str| {
            walk_library(
                &[music.join(folder)],
                library,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .files
        };
        assert!(walk("linked").is_empty(), "a link out, scanned by itself");
        assert!(
            walk("a/5.ogg").is_empty(),
            "nor is a link to a file read for being what a scan was given"
        );
        assert_eq!(walk("b"), [music.join("b/deep/2.flac")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn only_files_are_found_and_read() {
        let dir = std::env::temp_dir().join(format!("freya-walk-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("song.mp3"), b"one").unwrap();
        let fifo = dir.join("pipe.mp3");
        let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        #[allow(unsafe_code, reason = "makes a FIFO for the test, as mkfifo(1) would")]
        // SAFETY: `name` is a NUL-terminated path, which mkfifo only reads.
        let made = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
        assert_eq!(made, 0);
        // A FIFO named as a song is no song: opening it would wait for ever for a writer.
        assert_eq!(collect(std::slice::from_ref(&dir)), [dir.join("song.mp3")]);
        assert!(open_regular(&fifo).is_err());
        assert!(fingerprint(&fifo).is_err());
        assert!(read_at_most(&fifo, 10).is_err());
        assert!(open_regular(&dir).is_err(), "nor is a folder");
        // A file is read whole when it is no larger than asked, and not at all otherwise.
        assert_eq!(read_at_most(&dir.join("song.mp3"), 3).unwrap(), b"one");
        assert!(read_at_most(&dir.join("song.mp3"), 2).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn work_side_by_side_keeps_its_order() {
        let items: Vec<u32> = (0..100).collect();
        assert_eq!(
            side_by_side(&items, |i| i * 2),
            items.iter().map(|i| i * 2).collect::<Vec<_>>()
        );
        assert!(side_by_side(&[] as &[u32], |i| *i).is_empty());
    }
}

/// How much of each end of a file its fingerprint reads.
const FINGERPRINT_CHUNK: u64 = 64 * 1024;

/// The regular file at `path`, opened for reading without waiting. What a music folder others
/// can write to holds may be anything: a FIFO named as a song or a picture, which opening
/// would wait on for ever, is refused, as is anything else that is not a file.
pub fn open_regular(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

/// The largest picture file read from a folder: as large as the largest a server takes.
pub const LARGEST_PICTURE_FILE: u64 = 64 * 1024 * 1024;
/// The largest CUE sheet or playlist file read: text, of a few kilobytes as a rule.
pub const LARGEST_TEXT_FILE: u64 = 1024 * 1024;

/// All of the regular file at `path`, when it holds at most `most` bytes: an error for a
/// larger one, before any of it is read, so a file as large as the memory takes none of it.
pub fn read_at_most(path: &Path, most: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;

    let too_large =
        || std::io::Error::new(std::io::ErrorKind::InvalidData, "the file is too large");
    let file = open_regular(path)?;
    if file.metadata()?.len() > most {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    // Grown since it was measured, it is still read no further.
    file.take(most + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > most {
        return Err(too_large());
    }
    Ok(bytes)
}

/// A fingerprint of the file at `path` that stays the same when the file is moved or renamed:
/// its size and the sum of its first and last 64 KiB as 64-bit words, in 16 hex digits. Quick
/// to take, as only those are read.
pub fn fingerprint(path: &Path) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = open_regular(path)?;
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
