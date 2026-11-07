//! Tracks made smaller for a slow network, as Opus (see [`freya_transcode`]): `/audio` with
//! `&quality=high` or `normal`. A file is made smaller once, in a thread of its own, into the
//! folder [`keep_in`] names, and kept there for the next time, the least lately played going
//! first beyond [`MOST_KEPT`]. Its length is known before it is made, so it is answered at
//! once: each part as soon as it is encoded.
//!
//! Made smaller at most [`MOST_AT_ONCE`] at a time, and no more than [`MOST_WAITING`] asked
//! for, so a client cannot keep the server's processors or disk busy; beyond, a client is told
//! to ask again. A file that would not be smaller by much, or cannot be made so, is answered
//! as it is, as its own version: a client that kept a part of the smaller one forgets it.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime};

use freya_transcode::{Plan, Quality};

/// The most made smaller at once, and asked for all told.
const MOST_AT_ONCE: usize = 2;
const MOST_WAITING: usize = 16;
/// The most kept, all told.
#[cfg(not(test))]
const MOST_KEPT: u64 = 4_000_000_000;
#[cfg(test)]
const MOST_KEPT: u64 = 1_000_000;
/// How long a part is waited for before the answer is given up on: the file is being made
/// far slower than it plays.
const WAITED_AT_MOST: Duration = Duration::from_secs(60);
/// Which encoding of the files kept this is; another, and they are made again.
const FORMAT: u32 = 1;

/// A file being made smaller: how long it will be, and how much of it is written.
struct Making {
    length: u64,
    path: PathBuf,
    state: Mutex<Made>,
    changed: Condvar,
}

#[derive(Default)]
struct Made {
    written: u64,
    done: bool,
    failed: bool,
}

#[derive(Default)]
struct Smaller {
    dir: Option<PathBuf>,
    /// Those being made, and those waiting to be, by key.
    making: HashMap<String, Arc<Making>>,
    /// Those made now.
    at_once: usize,
    /// Those answered as they are: not much smaller, or failed to be made so. Keyed by the
    /// version of the file too, so a file changed is tried again.
    as_they_are: HashSet<String>,
}

static SMALLER: OnceLock<(Mutex<Smaller>, Condvar)> = OnceLock::new();

fn smaller() -> (MutexGuard<'static, Smaller>, &'static Condvar) {
    let (state, turn) = SMALLER.get_or_init(Default::default);
    (state.lock().unwrap_or_else(|e| e.into_inner()), turn)
}

/// Keeps the files made smaller in `dir`; until it is called, none is.
pub fn keep_in(dir: PathBuf) {
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("no folder for tracks made smaller, {}: {e}", dir.display());
        return;
    }
    // Those begun and not made before the server stopped.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "opus")
                && !path.with_extension("done").exists()
            {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    smaller().0.dir = Some(dir);
    trim();
}

/// What `/audio` answers for a file made smaller.
pub struct Answer {
    pub length: u64,
    pub tag: String,
    body: Body,
}

enum Body {
    Made(PathBuf),
    Making(Arc<Making>),
}

impl Answer {
    /// The bytes from `from` on.
    pub fn from(&self, from: u64) -> io::Result<Box<dyn Read + Send>> {
        match &self.body {
            Body::Made(path) => {
                let mut file = File::open(path)?;
                file.seek(SeekFrom::Start(from))?;
                Ok(Box::new(file))
            }
            Body::Making(making) => {
                let file = File::open(&making.path)?;
                Ok(Box::new(Following {
                    making: Arc::clone(making),
                    file,
                    at: from,
                    seeked: false,
                }))
            }
        }
    }
}

/// Why a file is not answered made smaller.
pub enum Not {
    /// It is answered as it is.
    AsItIs,
    /// Too many are made smaller now: asked again in a while.
    Busy,
}

/// The file `file`, at `path`, of version `version`, made smaller at `quality`.
pub fn answer(path: &Path, file: &File, version: &str, quality: Quality) -> Result<Answer, Not> {
    let key = key_of(path, version, quality);
    let tag = format!(
        "\"{}-{}-{FORMAT}\"",
        version.trim_matches('"'),
        quality.name()
    );
    let (state, _) = smaller();
    let Some(dir) = state.dir.clone() else {
        return Err(Not::AsItIs);
    };
    if state.as_they_are.contains(&key) {
        return Err(Not::AsItIs);
    }
    if let Some(making) = state.making.get(&key) {
        return Ok(Answer {
            length: making.length,
            tag,
            body: Body::Making(Arc::clone(making)),
        });
    }
    let made = dir.join(format!("{key}.opus"));
    if made.with_extension("done").exists()
        && let Ok(metadata) = std::fs::metadata(&made)
    {
        // Played now: the last to go.
        let _ = File::options()
            .append(true)
            .open(made.with_extension("done"))
            .and_then(|done| done.set_modified(SystemTime::now()));
        return Ok(Answer {
            length: metadata.len(),
            tag,
            body: Body::Made(made),
        });
    }
    if state.making.len() >= MOST_WAITING {
        return Err(Not::Busy);
    }
    // Planned with the lock let go: a file whose length its format does not say is read
    // through to count it.
    drop(state);
    let extension = path.extension().and_then(|e| e.to_str()).map(str::to_owned);
    let planned = file
        .try_clone()
        .map_err(freya_transcode::Error::from)
        .and_then(|file| Plan::new(file, extension.as_deref(), quality));
    let (mut state, _) = smaller();
    let plan = match planned {
        Ok(Some(plan)) => plan,
        Ok(None) => {
            state.as_they_are.insert(key);
            return Err(Not::AsItIs);
        }
        Err(e) => {
            log::info!("{} cannot be made smaller: {e}", path.display());
            state.as_they_are.insert(key);
            return Err(Not::AsItIs);
        }
    };
    if let Some(making) = state.making.get(&key) {
        // Asked for twice at once.
        return Ok(Answer {
            length: making.length,
            tag,
            body: Body::Making(Arc::clone(making)),
        });
    }
    let making = Arc::new(Making {
        length: plan.length(),
        path: made,
        state: Mutex::new(Made::default()),
        changed: Condvar::new(),
    });
    if let Err(e) = File::create(&making.path) {
        log::warn!("cannot keep a track made smaller: {e}");
        return Err(Not::AsItIs);
    }
    state.making.insert(key.clone(), Arc::clone(&making));
    drop(state);
    let made = Arc::clone(&making);
    let shown = path.display().to_string();
    let started = std::thread::Builder::new()
        .name("smaller".into())
        .spawn(move || make(key, plan, &made, &shown));
    if let Err(e) = started {
        log::warn!("cannot make a track smaller: {e}");
        return Err(Not::AsItIs);
    }
    Ok(Answer {
        length: making.length,
        tag,
        body: Body::Making(making),
    })
}

/// Makes `plan`'s file, in its turn.
fn make(key: String, plan: Plan, making: &Making, shown: &str) {
    {
        let (mut state, turn) = smaller();
        while state.at_once >= MOST_AT_ONCE {
            state = turn.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        state.at_once += 1;
    }
    let started = std::time::Instant::now();
    let quality = plan.quality();
    let made = File::options()
        .write(true)
        .open(&making.path)
        .map_err(freya_transcode::Error::from)
        .and_then(|file| plan.encode(Writing { file, making }));
    let done_mark = making.path.with_extension("done");
    let made = made.and_then(|()| File::create(&done_mark).map(drop).map_err(Into::into));
    let (mut state, turn) = smaller();
    state.at_once -= 1;
    state.making.remove(&key);
    match &made {
        Ok(()) => log::debug!(
            "made {shown} smaller, {}, in {:.1} s",
            quality.name(),
            started.elapsed().as_secs_f64()
        ),
        Err(e) => {
            log::info!("could not make {shown} smaller: {e}");
            state.as_they_are.insert(key);
            let _ = std::fs::remove_file(&making.path);
            let _ = std::fs::remove_file(&done_mark);
        }
    }
    turn.notify_all();
    drop(state);
    {
        let mut made_now = making.state.lock().unwrap_or_else(|e| e.into_inner());
        made_now.done = true;
        made_now.failed = made.is_err();
    }
    making.changed.notify_all();
    trim();
}

/// Writes a file being made, telling those who follow it how far it is.
struct Writing<'a> {
    file: File,
    making: &'a Making,
}

impl Write for Writing<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.file.write(bytes)?;
        let mut state = self.making.state.lock().unwrap_or_else(|e| e.into_inner());
        state.written += written as u64;
        drop(state);
        self.making.changed.notify_all();
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Reads a file being made, as far as it is, waiting for the rest.
struct Following {
    making: Arc<Making>,
    file: File,
    at: u64,
    seeked: bool,
}

impl Read for Following {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.at >= self.making.length {
            return Ok(0);
        }
        let mut state = self.making.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut waited = Duration::ZERO;
        while state.written <= self.at && !state.done {
            let before = state.written;
            let (now, timeout) = self
                .making
                .changed
                .wait_timeout(state, Duration::from_secs(1))
                .unwrap_or_else(|e| e.into_inner());
            state = now;
            if state.written == before && timeout.timed_out() {
                waited += Duration::from_secs(1);
                if waited >= WAITED_AT_MOST {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "not made in time"));
                }
            }
        }
        if state.failed {
            return Err(io::Error::other("could not be made smaller"));
        }
        let ready = state.written.saturating_sub(self.at);
        drop(state);
        if ready == 0 {
            return Ok(0);
        }
        if !self.seeked {
            self.file.seek(SeekFrom::Start(self.at))?;
            self.seeked = true;
        }
        let wanted = buf.len().min(usize::try_from(ready).unwrap_or(usize::MAX));
        let read = self.file.read(&mut buf[..wanted])?;
        self.at += read as u64;
        Ok(read)
    }
}

/// The name a file made smaller is kept by: of the file, its version, and the quality.
fn key_of(path: &Path, version: &str, quality: Quality) -> String {
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(path.to_string_lossy().as_bytes());
    hash.update(&[0]);
    hash.update(version.as_bytes());
    hash.update(&[0]);
    hash.update(quality.name().as_bytes());
    hash.update(&FORMAT.to_le_bytes());
    hash.finish().as_ref()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Lets those played least lately go, beyond [`MOST_KEPT`].
fn trim() {
    let (state, _) = smaller();
    let Some(dir) = state.dir.clone() else {
        return;
    };
    let making: Vec<PathBuf> = state.making.values().map(|m| m.path.clone()).collect();
    drop(state);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut kept: Vec<(SystemTime, u64, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "opus") && !making.contains(path))
        .filter_map(|path| {
            let used = std::fs::metadata(path.with_extension("done"))
                .ok()?
                .modified()
                .ok()?;
            let size = std::fs::metadata(&path).ok()?.len();
            Some((used, size, path))
        })
        .collect();
    kept.sort();
    let mut total: u64 = kept.iter().map(|(_, size, _)| size).sum();
    for (_, size, path) in kept {
        if total <= MOST_KEPT {
            break;
        }
        let _ = std::fs::remove_file(path.with_extension("done"));
        let _ = std::fs::remove_file(&path);
        total -= size;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn those_unfinished_are_dropped_and_the_least_lately_played_go_first() {
        let dir = std::env::temp_dir().join(format!("freya-smaller-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, size: usize, played: Option<u64>| {
            std::fs::write(dir.join(format!("{name}.opus")), vec![0u8; size]).unwrap();
            if let Some(ago) = played {
                let done = File::create(dir.join(format!("{name}.done"))).unwrap();
                done.set_modified(SystemTime::now() - Duration::from_secs(ago))
                    .unwrap();
            }
        };
        file("unfinished", 10, None);
        file("older", 600_000, Some(3600));
        file("newer", 600_000, Some(60));
        keep_in(dir.clone());
        let here = |name: &str| dir.join(format!("{name}.opus")).exists();
        assert!(!here("unfinished"));
        assert!(!here("older") && !dir.join("older.done").exists());
        assert!(here("newer"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_differ_by_file_version_and_quality() {
        let path = Path::new("/music/a.flac");
        let key = key_of(path, "\"1-2\"", Quality::High);
        assert_ne!(key, key_of(path, "\"1-3\"", Quality::High));
        assert_ne!(key, key_of(path, "\"1-2\"", Quality::Normal));
        assert_ne!(
            key,
            key_of(Path::new("/music/b.flac"), "\"1-2\"", Quality::High)
        );
        assert_eq!(key.len(), 32);
    }
}
