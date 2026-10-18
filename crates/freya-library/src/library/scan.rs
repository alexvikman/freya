//! Scanning music folders into the library: new and changed files are read, files that are
//! gone are removed, and unchanged files are left alone. An audio file that a CUE sheet cuts
//! into tracks is read as part of the sheet.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use super::{FileStamp, Library, NewTrack, Result, added, epoch_millis, put, remove};
use crate::cue::{self, Found};
use crate::files;
use crate::tags::{TagInfo, read_tags};

/// Changes are written in batches, at least this often, so the library fills up visibly
/// during a long first scan without a transaction per file.
const COMMIT_INTERVAL: Duration = Duration::from_millis(500);
/// Tags are read by up to this many threads; reading is mostly waiting for the disk.
const MAX_READERS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanEvent {
    /// `done` of `total` new or changed files (audio files or CUE sheets) have been read and
    /// written.
    Progress {
        done: usize,
        total: usize,
    },
    Finished(ScanSummary),
    Failed(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanSummary {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
}

/// Whether `folder` looks like a drive that is not there, rather than one whose music is gone:
/// missing, unreadable, or empty, as a mount point is while nothing is mounted on it. A folder
/// emptied on purpose keeps its tracks until the user removes the folder from the library.
fn is_offline(folder: &Path) -> bool {
    match std::fs::read_dir(folder) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => true,
    }
}

/// Brings the library in line with `folders`. Files under a folder that is offline are kept,
/// so they need not be read again when it comes back: one that does not exist (an unplugged
/// drive), cannot be read, or is empty (a mount point with nothing mounted on it). Files
/// outside all `folders` are removed. Returns early, with what was done so far, when `cancel`
/// is set.
pub fn scan(
    library: &mut Library,
    folders: &[PathBuf],
    cancel: &AtomicBool,
    mut progress: impl FnMut(ScanEvent),
) -> Result<ScanSummary> {
    let found = find(folders, cancel);
    let known = library.sources()?;
    // Tracks found from now on came into the library now, except on the library's first scan:
    // everything comes in at once then, and the files' own dates say more.
    let first = known.is_empty();
    let now = epoch_millis(SystemTime::now());
    let mut summary = ScanSummary::default();

    let offline: Vec<&PathBuf> = folders.iter().filter(|f| is_offline(f)).collect();
    let gone: Vec<&PathBuf> = {
        let found: HashSet<&Path> = found.iter().map(|(f, _)| f.source()).collect();
        known
            .keys()
            .filter(|p| !found.contains(p.as_path()))
            .filter(|p| !offline.iter().any(|f| p.starts_with(f)))
            .collect()
    };
    if !gone.is_empty() && !cancel.load(Ordering::Relaxed) {
        let tx = library.transaction()?;
        for path in &gone {
            remove(&tx, path)?;
        }
        tx.commit()?;
        summary.removed = gone.len();
    }

    let found_count = found.len();
    let changed: Vec<(Found, FileStamp)> = found
        .into_iter()
        .filter(|(found, stamp)| known.get(found.source()) != Some(stamp))
        .collect();
    summary.unchanged = found_count - changed.len();
    let total = changed.len();
    if total > 0 {
        progress(ScanEvent::Progress { done: 0, total });
    }

    let next = AtomicUsize::new(0);
    let (results, read) = channel::<(usize, Vec<NewTrack>)>();
    let readers = std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .clamp(1, MAX_READERS)
        .min(total.max(1));
    std::thread::scope(|threads| -> Result<()> {
        for _ in 0..readers {
            let results = results.clone();
            let (changed, next) = (&changed, &next);
            threads.spawn(move || {
                while !cancel.load(Ordering::Relaxed) {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((found, _)) = changed.get(i) else {
                        break;
                    };
                    if results.send((i, read_tracks(found))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(results);

        let mut tx = library.transaction()?;
        let mut committed = Instant::now();
        for (done, (i, tracks)) in (1..).zip(read) {
            let (found, stamp) = &changed[i];
            let source = found.source();
            let added = match added(&tx, source)? {
                Some(added) => added,
                None if first => file_time(source).unwrap_or(now).min(now),
                None => now,
            };
            remove(&tx, source)?;
            for track in &tracks {
                put(&tx, source, *stamp, added, track)?;
            }
            if known.contains_key(source) {
                summary.updated += 1;
            } else {
                summary.added += 1;
            }
            if committed.elapsed() >= COMMIT_INTERVAL {
                tx.commit()?;
                progress(ScanEvent::Progress { done, total });
                tx = library.transaction()?;
                committed = Instant::now();
            }
        }
        tx.commit()?;
        Ok(())
    })?;
    Ok(summary)
}

/// What there is to read under `folders`: audio files, and the discs of CUE sheets in place of
/// the files they cut, each with the stamp of what it is read from. Each is listed once, even
/// when folders overlap.
fn find(folders: &[PathBuf], cancel: &AtomicBool) -> Vec<(Found, FileStamp)> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for item in cue::apply_sheets(files::collect_with_sheets(folders)) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !seen.insert(item.source().to_owned()) {
            continue;
        }
        if let Some(stamp) = stamp_of(&item) {
            found.push((item, stamp));
        }
    }
    found
}

/// The stamp of what `found` is read from. For a disc, that is its sheet and its audio files
/// together, so that a change to any of them is noticed.
fn stamp_of(found: &Found) -> Option<FileStamp> {
    let stamp = |path: &Path| Some(FileStamp::of(&std::fs::metadata(path).ok()?));
    match found {
        Found::File(path) => stamp(path),
        Found::Disc(disc) => {
            let mut total = stamp(&disc.sheet)?;
            for file in &disc.files {
                let file = stamp(file)?;
                total.size += file.size;
                total.modified = total.modified.max(file.modified);
            }
            Some(total)
        }
    }
}

/// When a file came into the user's folders, as near as the system can tell: when it was made
/// there, else when it last changed.
fn file_time(path: &Path) -> Option<i64> {
    let metadata = std::fs::metadata(path).ok()?;
    let time = metadata.created().or_else(|_| metadata.modified()).ok()?;
    Some(epoch_millis(time))
}

/// Reads the tracks of what was found: an audio file is one, a disc has its sheet's.
fn read_tracks(found: &Found) -> Vec<NewTrack> {
    match found {
        // Unreadable files are still listed, under their file name.
        Found::File(path) => vec![NewTrack {
            path: path.clone(),
            part: None,
            tags: read_tags(path).unwrap_or_default(),
        }],
        Found::Disc(disc) => {
            let files: HashMap<&Path, TagInfo> = disc
                .files
                .iter()
                .map(|file| (file.as_path(), read_tags(file).unwrap_or_default()))
                .collect();
            disc.tracks
                .iter()
                .map(|track| NewTrack {
                    path: track.path.clone(),
                    part: Some(track.part),
                    tags: track.tags_with(&files[track.path.as_path()]),
                })
                .collect()
        }
    }
}

/// Runs scans on a background thread, one at a time.
pub struct Scanner {
    events_out: Sender<ScanEvent>,
    events: Receiver<ScanEvent>,
    running: Option<(JoinHandle<()>, Arc<AtomicBool>)>,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    pub fn new() -> Self {
        let (events_out, events) = channel();
        Self {
            events_out,
            events,
            running: None,
        }
    }

    /// Scans `folders` into the library at `database`, stopping a scan that is still
    /// running first.
    pub fn start(&mut self, database: PathBuf, folders: Vec<PathBuf>) {
        self.stop();
        let cancel = Arc::new(AtomicBool::new(false));
        let events = self.events_out.clone();
        let stop = Arc::clone(&cancel);
        let thread = std::thread::Builder::new()
            .name("freya-scan".into())
            .spawn(move || {
                let started = Instant::now();
                let result = Library::open(&database).and_then(|mut library| {
                    scan(&mut library, &folders, &stop, |e| {
                        let _ = events.send(e);
                    })
                });
                let event = match result {
                    Ok(summary) => {
                        log::info!("library scan took {:?}: {summary:?}", started.elapsed());
                        ScanEvent::Finished(summary)
                    }
                    Err(e) => ScanEvent::Failed(e.to_string()),
                };
                let _ = events.send(event);
            })
            .expect("spawn scan thread");
        self.running = Some((thread, cancel));
    }

    /// Stops a running scan and waits for it; what it wrote so far is kept.
    pub fn stop(&mut self) {
        if let Some((thread, cancel)) = self.running.take() {
            cancel.store(true, Ordering::Relaxed);
            let _ = thread.join();
            // Events from the stopped scan would only confuse whoever reads the next one.
            while self.events.try_recv().is_ok() {}
        }
    }

    pub fn is_running(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|(thread, _)| !thread.is_finished())
    }

    /// Events that have arrived since the last call.
    pub fn poll(&self) -> impl Iterator<Item = ScanEvent> + '_ {
        self.events.try_iter()
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        self.stop();
    }
}
