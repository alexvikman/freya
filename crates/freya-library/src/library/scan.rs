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

use super::fetch::{Online, fetch_album_covers, fetch_artists, fetch_genres};
use super::{AlbumCover, FileStamp, Library, NewTrack, Result, added, epoch_millis, put, remove};
use crate::cover::ARTWORK_FOLDERS;
use crate::cue::{self, Found};
use crate::files;
use crate::pictures::Pictures;
use crate::tags::{TagInfo, read_tags};
use crate::thumbnails::Thumbnails;

/// Changes are written in batches, at least this often, so the library fills up visibly
/// during a long first scan without a transaction per file.
const COMMIT_INTERVAL: Duration = Duration::from_millis(500);
/// Tags are read by up to this many threads; reading is mostly waiting for the disk.
const MAX_READERS: usize = 8;
/// The folder being read is told at most this often.
const LOOKING_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanEvent {
    /// `done` of `total` new or changed files (audio files or CUE sheets) have been read and
    /// written.
    Progress {
        done: usize,
        total: usize,
    },
    /// The folder being looked through or read now, told every so often.
    Looking(PathBuf),
    /// `done` of `total` albums' covers have been made.
    Covers {
        done: usize,
        total: usize,
    },
    /// The internet was asked what `done` of the `total` artists who lack something lack.
    Artists {
        done: usize,
        total: usize,
    },
    /// And of the `total` genres it has not said anything of.
    Genres {
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

/// How long each part of a scan took, for the log.
struct Laps {
    since: Instant,
    took: Vec<(&'static str, Duration)>,
}

impl Laps {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            took: Vec::new(),
        }
    }

    /// `what` is done: the time since the last lap was its.
    fn lap(&mut self, what: &'static str) {
        self.took.push((what, self.since.elapsed()));
        self.since = Instant::now();
    }
}

impl std::fmt::Display for Laps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self
            .took
            .iter()
            .map(|(what, took)| format!("{what} {:.1}s", took.as_secs_f64()))
            .collect();
        f.write_str(&parts.join(", "))
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
    progress: impl FnMut(ScanEvent),
) -> Result<ScanSummary> {
    scan_with(library, folders, &ScanOptions::default(), cancel, progress)
}

/// What a scan does beyond reading the tracks.
#[derive(Clone, Debug, Default)]
pub struct ScanOptions {
    /// Where albums' covers are kept; without, none are made.
    pub pictures: Option<Pictures>,
    /// Read every file and make every cover again, changed or not.
    pub rescan: bool,
    /// Look through these folders within the library's only, leaving the rest as it is; the
    /// whole library when empty. A file moved from one to another keeps its plays, as with
    /// the whole library.
    pub only: Vec<PathBuf>,
    /// What may be asked of the internet about artists who lack something, once the tracks
    /// and covers are in.
    pub online: Online,
}

/// Like [`scan`], and then makes albums' covers as `options` says: of new albums, of those
/// whose tracks changed, and of those whose folder's pictures changed.
pub fn scan_with(
    library: &mut Library,
    folders: &[PathBuf],
    options: &ScanOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(ScanEvent),
) -> Result<ScanSummary> {
    let partial = !options.only.is_empty();
    let walked = if partial { &options.only[..] } else { folders };
    let within = |path: &Path| !partial || walked.iter().any(|f| path.starts_with(f));
    let mut laps = Laps::new();
    let (found, images, stamps) = find(walked, cancel, &mut progress, &mut laps);
    let known = library.sources()?;
    // Tracks found from now on came into the library now, except on the library's first scan:
    // everything comes in at once then, and the files' own dates say more.
    let first = known.is_empty();
    let now = epoch_millis(SystemTime::now());
    let mut summary = ScanSummary::default();

    // Of the library's folders: a folder within one that is gone was taken away.
    let offline: Vec<&PathBuf> = folders.iter().filter(|f| is_offline(f)).collect();
    laps.lap("offline");
    let gone: Vec<&PathBuf> = {
        let found: HashSet<&Path> = found.iter().map(|(f, _)| f.source()).collect();
        known
            .keys()
            .filter(|p| within(p))
            .filter(|p| !found.contains(p.as_path()))
            .filter(|p| !offline.iter().any(|f| p.starts_with(f)))
            .collect()
    };
    // What the files that are gone were, to know them again if they were only moved.
    let mut moved = Moved::default();
    if !cancel.load(Ordering::Relaxed) {
        for path in &gone {
            moved.note(library, path)?;
        }
    }
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
        .filter(|(found, stamp)| options.rescan || known.get(found.source()) != Some(stamp))
        .collect();
    let changed_sources: Vec<PathBuf> = changed
        .iter()
        .map(|(found, _)| found.source().to_owned())
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
                None => match moved.take(&tx, &tracks)? {
                    // Moved or renamed: it came in when it first did.
                    Some(added) => added,
                    None if first => file_time(source).unwrap_or(now).min(now),
                    None => now,
                },
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
                if let Some(folder) = source.parent() {
                    progress(ScanEvent::Looking(folder.to_owned()));
                }
                progress(ScanEvent::Progress { done, total });
                tx = library.transaction()?;
                committed = Instant::now();
            }
        }
        tx.commit()?;
        Ok(())
    })?;
    laps.lap("tracks");
    if let Some(pictures) = &options.pictures
        && !cancel.load(Ordering::Relaxed)
    {
        if !library.cover_colors_current()? {
            library.make_cover_colors(pictures, cancel)?;
            laps.lap("colors");
        }
        make_covers(
            library,
            &images,
            &stamps,
            &changed_sources,
            options.rescan,
            &within,
            pictures,
            cancel,
            &mut progress,
        )?;
        laps.lap("covers");
        if !cancel.load(Ordering::Relaxed) {
            take_artists_pictures(library, &images, options.rescan, pictures)?;
            laps.lap("folder pictures");
        }
        if !cancel.load(Ordering::Relaxed) {
            fetch_artists(
                library,
                None,
                &options.online,
                pictures,
                cancel,
                &mut progress,
            )?;
            laps.lap("artists online");
        }
        if !cancel.load(Ordering::Relaxed) {
            fetch_genres(library, &options.online, cancel, &mut progress)?;
            laps.lap("genres online");
        }
        if !cancel.load(Ordering::Relaxed) && options.online.any() {
            fetch_album_covers(library, pictures, cancel)?;
            laps.lap("covers online");
        }
    }
    log::info!("scan parts: {laps}");
    if !partial && !cancel.load(Ordering::Relaxed) {
        library.note_full_scan(SystemTime::now())?;
    }
    Ok(summary)
}

/// Files gone from where they were, by their fingerprints, to know them again where they were
/// moved to.
#[derive(Default)]
struct Moved {
    /// Each audio file's path, and when its tracks came in; none for a fingerprint two files
    /// that are gone shared, which tells neither apart.
    by_fingerprint: HashMap<String, Option<(String, i64)>>,
}

impl Moved {
    /// Notes the audio files read from `source`, which is gone.
    fn note(&mut self, library: &Library, source: &Path) -> Result<()> {
        let mut statement = library.db.prepare_cached(
            "SELECT fingerprint, path, min(added) FROM tracks
             WHERE source = ?1 AND fingerprint IS NOT NULL GROUP BY path",
        )?;
        let rows = statement.query_map([source.to_string_lossy()], |row| {
            Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
        })?;
        for row in rows {
            let (fingerprint, path, added) = row?;
            self.by_fingerprint
                .entry(fingerprint)
                .and_modify(|known| *known = None)
                .or_insert(Some((path, added)));
        }
        Ok(())
    }

    /// When `tracks`, new in the library, first came in, if their files were moved from
    /// where they were; their plays then follow them.
    fn take(&mut self, db: &rusqlite::Connection, tracks: &[NewTrack]) -> Result<Option<i64>> {
        let mut first = None;
        let mut paths = HashSet::new();
        for track in tracks {
            let Some(fingerprint) = &track.fingerprint else {
                continue;
            };
            if !paths.insert(&track.path) {
                continue;
            }
            let Some(Some((before, added))) = self.by_fingerprint.remove(fingerprint) else {
                continue;
            };
            db.prepare_cached("UPDATE plays SET path = ?1 WHERE path = ?2")?
                .execute(rusqlite::params![track.path.to_string_lossy(), before])?;
            log::info!("{before} was moved to {}", track.path.display());
            first = Some(first.map_or(added, |first: i64| first.min(added)));
        }
        Ok(first)
    }
}

/// Gives artists without a picture the one in their folder (artist.jpg, fanart.jpg), found
/// among the pictures the walk found: the folders are not looked in again. A picture an artist
/// has stays theirs, as the user chose it or Freya found it first; with `rescan`, one from
/// their folder is taken in again.
fn take_artists_pictures(
    library: &mut Library,
    images: &[PathBuf],
    rescan: bool,
    pictures: &Pictures,
) -> Result<()> {
    use crate::artist_image::FolderImages;
    use crate::library::{FOLDER, PictureKind};

    let found = FolderImages::new(images);
    for artist in library.artists()? {
        let Some(name) = artist.name else {
            continue;
        };
        let tracks: Vec<PathBuf> = library
            .albums(Some(&name))?
            .into_iter()
            .map(|album| album.track)
            .collect();
        let pictures_found = [
            (PictureKind::Portrait, found.portrait(&tracks), 1400),
            (PictureKind::Backdrop, found.backdrop(&tracks), 3840),
        ];
        for (kind, file, largest) in pictures_found {
            let Some(file) = file else {
                continue;
            };
            let had = library.artist_picture(&name, kind)?;
            let again = rescan && had.as_ref().is_some_and(|row| row.source == FOLDER);
            if had.is_some_and(|row| row.picture.is_some()) && !again {
                continue;
            }
            if again {
                library.forget_artist_picture(&name, kind)?;
            }
            match pictures.take_file(&file, largest) {
                Ok(picture) => library.set_artist_picture(
                    &name,
                    kind,
                    FOLDER,
                    Some(&picture),
                    SystemTime::now(),
                )?,
                Err(e) => log::warn!("could not take in {}: {e}", file.display()),
            }
        }
    }
    Ok(())
}

/// Makes the covers of the albums that have none yet: new ones, and those without a cover
/// whose tracks are in `changed_sources` or whose folder's `images` changed. A cover found once
/// stays the album's; all are made again only when `rescan`, as the user asks. A few at a time,
/// as reading a cover is mostly waiting for the disk. Only albums `within` the folders
/// looked through, whose pictures are in `images`.
#[allow(clippy::too_many_arguments)]
fn make_covers(
    library: &mut Library,
    images: &[PathBuf],
    stamps: &HashMap<PathBuf, FileStamp>,
    changed_sources: &[PathBuf],
    rescan: bool,
    within: &dyn Fn(&Path) -> bool,
    pictures: &Pictures,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(ScanEvent),
) -> Result<()> {
    let mut changed = HashSet::new();
    for source in changed_sources {
        let mut statement = library
            .db
            .prepare_cached("SELECT DISTINCT album_artist, album FROM tracks WHERE source = ?1")?;
        let albums = statement.query_map([source.to_string_lossy()], |row| {
            let (artist, album): (Option<String>, Option<String>) = (row.get(0)?, row.get(1)?);
            Ok(Thumbnails::key(artist.as_deref(), album.as_deref()))
        })?;
        for album in albums {
            changed.insert(album?);
        }
    }
    let stamps = ImageStamps::new(images, stamps);
    let kept = library.album_covers()?;
    let mut keys = HashSet::new();
    let mut work = Vec::new();
    for album in library.all_albums()? {
        let key = Thumbnails::key(album.artist.as_deref(), album.name.as_deref());
        let track = album.track.to_string_lossy().into_owned();
        let images = album.track.parent().map_or(0, |folder| stamps.of(folder));
        let made = kept.get(&key);
        // A cover found once, in its music or on the internet, stays the album's: only one
        // still missing is looked for again, when its music or pictures changed.
        let missing = made.is_none_or(|made| made.cover.is_none());
        let looked = made.is_some_and(|made| made.track == track && made.images == images);
        let needed =
            within(&album.track) && (rescan || (missing && (changed.contains(&key) || !looked)));
        if needed {
            work.push((key.clone(), album.track, images));
        }
        keys.insert(key);
    }
    library.keep_album_covers(&keys)?;
    let total = work.len();
    if total == 0 {
        return Ok(());
    }
    progress(ScanEvent::Covers { done: 0, total });
    let next = AtomicUsize::new(0);
    let (results, made) = channel::<(
        usize,
        std::result::Result<Option<(String, [u8; 3])>, String>,
    )>();
    let workers = std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .clamp(1, MAX_READERS)
        .min(total);
    std::thread::scope(|threads| -> Result<()> {
        for _ in 0..workers {
            let results = results.clone();
            let (work, next) = (&work, &next);
            threads.spawn(move || {
                while !cancel.load(Ordering::Relaxed) {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((_, track, _)) = work.get(i) else {
                        break;
                    };
                    let cover = match crate::cover::album_art(track) {
                        Some((jpeg, color)) => pictures
                            .put(&jpeg)
                            .map(|checksum| Some((checksum, color)))
                            .map_err(|e| e.to_string()),
                        None => Ok(None),
                    };
                    if results.send((i, cover)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(results);
        let mut told = Instant::now();
        for (done, (i, cover)) in (1..).zip(made) {
            let (key, track, images) = &work[i];
            // Not noted: made again at the next scan.
            let cover = match cover {
                Ok(cover) => cover,
                Err(e) => {
                    log::warn!("could not keep the cover of {}: {e}", track.display());
                    continue;
                }
            };
            let (cover, color) = cover.unzip();
            library.set_album_cover(
                key,
                &AlbumCover {
                    cover,
                    color,
                    track: track.to_string_lossy().into_owned(),
                    images: *images,
                },
            )?;
            if told.elapsed() >= COMMIT_INTERVAL || done == total {
                told = Instant::now();
                progress(ScanEvent::Covers { done, total });
            }
        }
        Ok(())
    })
}

/// A stamp of the pictures in each folder of the music, and in its artwork folders, such as
/// "Covers": it changes when one is added, taken away or changed.
struct ImageStamps<'a> {
    by_folder: HashMap<&'a Path, Vec<&'a Path>>,
    stamps: &'a HashMap<PathBuf, FileStamp>,
}

impl<'a> ImageStamps<'a> {
    /// With the `stamps` the walk took of `images`.
    fn new(images: &'a [PathBuf], stamps: &'a HashMap<PathBuf, FileStamp>) -> Self {
        let mut by_folder: HashMap<&Path, Vec<&Path>> = HashMap::new();
        for image in images {
            if let Some(folder) = image.parent() {
                by_folder.entry(folder).or_default().push(image);
                // An artwork folder's pictures count for the album's folder too.
                let artwork = folder
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| ARTWORK_FOLDERS.contains(&n.to_ascii_lowercase().as_str()));
                if artwork && let Some(album) = folder.parent() {
                    by_folder.entry(album).or_default().push(image);
                }
            }
        }
        Self { by_folder, stamps }
    }

    /// The stamp of `folder`'s pictures: their names, sizes and times, hashed.
    fn of(&self, folder: &Path) -> i64 {
        let Some(images) = self.by_folder.get(folder) else {
            return 0;
        };
        // FNV-1a: stable between runs, unlike the standard library's hasher.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut images = images.clone();
        images.sort();
        for image in images {
            let stamp = stamp_at(image, self.stamps).unwrap_or(FileStamp {
                size: 0,
                modified: 0,
            });
            let text = format!("{}\0{}\0{}\0", image.display(), stamp.size, stamp.modified);
            for byte in text.bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
        }
        hash as i64
    }
}

/// What there is to read under `folders`: audio files, and the discs of CUE sheets in place of
/// the files they cut, each with the stamp of what it is read from. Each is listed once, even
/// when folders overlap. With the pictures in the folders, which covers may be, and the stamps
/// of all of them.
#[allow(clippy::type_complexity)]
fn find(
    folders: &[PathBuf],
    cancel: &AtomicBool,
    progress: &mut impl FnMut(ScanEvent),
    laps: &mut Laps,
) -> (
    Vec<(Found, FileStamp)>,
    Vec<PathBuf>,
    HashMap<PathBuf, FileStamp>,
) {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    let mut told: Option<Instant> = None;
    let mut entering = |dir: &Path| {
        if told.is_none_or(|at| at.elapsed() >= LOOKING_INTERVAL) {
            told = Some(Instant::now());
            progress(ScanEvent::Looking(dir.to_owned()));
        }
    };
    let walked = files::walk_library(folders, cancel, &mut entering);
    laps.lap("walk");
    for item in cue::apply_sheets(walked.files) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !seen.insert(item.source().to_owned()) {
            continue;
        }
        if let Some(stamp) = stamp_of(&item, &walked.stamps) {
            found.push((item, stamp));
        }
    }
    laps.lap("sheets");
    (found, walked.images, walked.stamps)
}

/// The stamp of `path`: the one the walk took, else the file's now.
fn stamp_at(path: &Path, walked: &HashMap<PathBuf, FileStamp>) -> Option<FileStamp> {
    walked
        .get(path)
        .copied()
        .or_else(|| Some(FileStamp::of(&std::fs::metadata(path).ok()?)))
}

/// The stamp of what `found` is read from. For a disc, that is its sheet and its audio files
/// together, so that a change to any of them is noticed.
fn stamp_of(found: &Found, walked: &HashMap<PathBuf, FileStamp>) -> Option<FileStamp> {
    let stamp = |path: &Path| stamp_at(path, walked);
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
    let fingerprint = |path: &Path| match files::fingerprint(path) {
        Ok(fingerprint) => Some(fingerprint),
        Err(e) => {
            log::debug!("no fingerprint of {}: {e}", path.display());
            None
        }
    };
    match found {
        // Unreadable files are still listed, under their file name.
        Found::File(path) => vec![NewTrack {
            path: path.clone(),
            part: None,
            tags: read_tags(path).unwrap_or_default(),
            fingerprint: fingerprint(path),
        }],
        Found::Disc(disc) => {
            let files: HashMap<&Path, (TagInfo, Option<String>)> = disc
                .files
                .iter()
                .map(|file| {
                    let tags = read_tags(file).unwrap_or_default();
                    (file.as_path(), (tags, fingerprint(file)))
                })
                .collect();
            disc.tracks
                .iter()
                .map(|track| {
                    let (tags, fingerprint) = &files[track.path.as_path()];
                    NewTrack {
                        path: track.path.clone(),
                        part: Some(track.part),
                        tags: track.tags_with(tags),
                        fingerprint: fingerprint.clone(),
                    }
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

    /// Scans `folders` into the library at `database` as `options` say, stopping a scan that
    /// is still running first.
    pub fn start(&mut self, database: PathBuf, folders: Vec<PathBuf>, options: ScanOptions) {
        self.stop();
        let cancel = Arc::new(AtomicBool::new(false));
        let events = self.events_out.clone();
        let stop = Arc::clone(&cancel);
        let thread = std::thread::Builder::new()
            .name("freya-scan".into())
            .spawn(move || {
                let started = Instant::now();
                let result = Library::open(&database).and_then(|mut library| {
                    scan_with(&mut library, &folders, &options, &stop, |e| {
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

    /// Asks the internet what `artists` lack, as `online` allows, without scanning: after the
    /// user matched them to another artist, or allowed another service. Stops a scan that is
    /// still running first.
    pub fn fetch(
        &mut self,
        database: PathBuf,
        artists: Option<Vec<String>>,
        online: Online,
        pictures: Pictures,
    ) {
        self.stop();
        let cancel = Arc::new(AtomicBool::new(false));
        let events = self.events_out.clone();
        let stop = Arc::clone(&cancel);
        let thread = std::thread::Builder::new()
            .name("freya-fetch".into())
            .spawn(move || {
                let result = Library::open(&database).and_then(|library| {
                    fetch_artists(
                        &library,
                        artists.as_deref(),
                        &online,
                        &pictures,
                        &stop,
                        &mut |e| {
                            let _ = events.send(e);
                        },
                    )?;
                    fetch_album_covers(&library, &pictures, &stop)
                });
                let event = match result {
                    Ok(()) => ScanEvent::Finished(ScanSummary::default()),
                    Err(e) => ScanEvent::Failed(e.to_string()),
                };
                let _ = events.send(event);
            })
            .expect("spawn fetch thread");
        self.running = Some((thread, cancel));
    }

    /// Makes every album's color again from its cover, as Freya makes them now, without
    /// scanning: when they were made otherwise before. Stops a scan that is still running.
    pub fn make_colors(&mut self, database: PathBuf, pictures: Pictures) {
        self.stop();
        let cancel = Arc::new(AtomicBool::new(false));
        let events = self.events_out.clone();
        let stop = Arc::clone(&cancel);
        let thread = std::thread::Builder::new()
            .name("freya-colors".into())
            .spawn(move || {
                let result = Library::open(&database)
                    .and_then(|library| library.make_cover_colors(&pictures, &stop));
                let event = match result {
                    Ok(made) => {
                        log::info!("made the colors of {made} albums again");
                        ScanEvent::Finished(ScanSummary::default())
                    }
                    Err(e) => ScanEvent::Failed(e.to_string()),
                };
                let _ = events.send(event);
            })
            .expect("spawn colors thread");
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
