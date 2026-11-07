//! Files fetched from servers, kept on disk so playing one again fetches nothing: whole, or
//! the parts of it that were fetched. Each is kept as its bytes, where they were not fetched
//! a hole, beside an index of what of it is here and which version of the file it is, as the
//! server tags it. When they take more than the limit, those used longest ago go first.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

/// What an index starts with, and the version of its format.
const MAGIC: &str = "freya-cache 1";

/// Where files are kept, and how much may be kept.
#[derive(Clone, Debug)]
struct Config {
    dir: PathBuf,
    limit: u64,
}

struct Cache {
    config: Option<Config>,
    /// The files being read now, by their key, and by how many: kept while they are.
    open: BTreeMap<String, usize>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache {
    config: None,
    open: BTreeMap::new(),
});

fn cache() -> MutexGuard<'static, Cache> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Keeps files fetched from servers in `dir`, at most `limit` bytes of them; none with no
/// folder or no room. Files kept beyond a new limit go at once.
pub fn set_cache(dir: Option<PathBuf>, limit: u64) {
    let config = dir.filter(|_| limit > 0).map(|dir| Config { dir, limit });
    cache().config = config;
    trim();
}

/// How much the files kept take.
pub fn cache_used() -> u64 {
    let Some(config) = cache().config.clone() else {
        return 0;
    };
    entries(&config.dir).iter().map(|entry| entry.size).sum()
}

/// Forgets every file kept, but those being read.
pub fn clear_cache() {
    let (config, open) = {
        let cache = cache();
        (cache.config.clone(), cache.open.clone())
    };
    let Some(config) = config else {
        return;
    };
    for entry in entries(&config.dir) {
        if !open.contains_key(&entry.key) {
            remove(&config.dir, &entry.key);
        }
    }
}

/// A kept file, as listed: its key, its size, and when it was last used.
struct Listed {
    key: String,
    size: u64,
    used: SystemTime,
}

/// The files kept in `dir`: each as large as its bytes' file, holes and all, as a disk that
/// does not keep holes apart takes them; used when its index was, or else when its bytes
/// were written last.
fn entries(dir: &Path) -> Vec<Listed> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for item in read.flatten() {
        let path = item.path();
        if path.extension().and_then(|e| e.to_str()) != Some("audio") {
            continue;
        }
        let Some(key) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(bytes) = item.metadata() else {
            continue;
        };
        let used = std::fs::metadata(dir.join(format!("{key}.index")))
            .and_then(|m| m.modified())
            .or_else(|_| bytes.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let size = bytes.len();
        entries.push(Listed {
            key: key.to_owned(),
            size,
            used,
        });
    }
    entries
}

fn remove(dir: &Path, key: &str) {
    let _ = std::fs::remove_file(dir.join(format!("{key}.index")));
    let _ = std::fs::remove_file(dir.join(format!("{key}.audio")));
}

/// Lets go of the files used longest ago, but those being read, until the rest fit.
fn trim() {
    let (config, open) = {
        let cache = cache();
        (cache.config.clone(), cache.open.clone())
    };
    let Some(config) = config else {
        return;
    };
    let mut entries = entries(&config.dir);
    let mut used: u64 = entries.iter().map(|entry| entry.size).sum();
    if used <= config.limit {
        return;
    }
    entries.sort_by_key(|entry| entry.used);
    for entry in entries {
        if used <= config.limit {
            break;
        }
        if open.contains_key(&entry.key) {
            continue;
        }
        remove(&config.dir, &entry.key);
        used = used.saturating_sub(entry.size);
    }
}

/// The key a file is kept by: its address, hashed (FNV-1a), which names its files.
fn key_of(url: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in url.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Which bytes of a file are here: ranges from their first byte to after their last, in
/// order, apart.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Ranges(Vec<(u64, u64)>);

impl Ranges {
    /// Adds the bytes from `start` to `end`.
    pub fn insert(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let (mut start, mut end) = (start, end);
        let mut kept = Vec::with_capacity(self.0.len() + 1);
        let mut placed = false;
        for &(from, to) in &self.0 {
            if to < start {
                kept.push((from, to));
            } else if from > end {
                if !placed {
                    kept.push((start, end));
                    placed = true;
                }
                kept.push((from, to));
            } else {
                start = start.min(from);
                end = end.max(to);
            }
        }
        if !placed {
            kept.push((start, end));
        }
        self.0 = kept;
    }

    /// How many bytes from `at` on are here, one after another.
    pub fn from(&self, at: u64) -> u64 {
        self.0
            .iter()
            .find(|&&(from, to)| from <= at && at < to)
            .map_or(0, |&(_, to)| to - at)
    }

    /// The first byte from `at` on that is not here, before `len`.
    pub fn missing_from(&self, at: u64, len: u64) -> Option<u64> {
        let mut at = at;
        for &(from, to) in &self.0 {
            if from <= at && at < to {
                at = to;
            }
        }
        (at < len).then_some(at)
    }

    /// Where the next bytes here start after `at`, if any do.
    pub fn next_held(&self, at: u64) -> Option<u64> {
        self.0.iter().map(|&(from, _)| from).find(|&from| from > at)
    }

    /// How many bytes are here.
    pub fn held(&self) -> u64 {
        self.0.iter().map(|(from, to)| to - from).sum()
    }

    /// Where the last bytes here end.
    pub fn end(&self) -> u64 {
        self.0.last().map_or(0, |&(_, to)| to)
    }

    /// How many of the bytes from `start` to `end` are here.
    pub fn held_in(&self, start: u64, end: u64) -> u64 {
        self.0
            .iter()
            .map(|&(from, to)| to.min(end).saturating_sub(from.max(start)))
            .sum()
    }

    /// Whether all of `len` bytes are here.
    pub fn whole(&self, len: u64) -> bool {
        len == 0 || self.0.first() == Some(&(0, len))
    }

    fn to_text(&self) -> String {
        self.0
            .iter()
            .map(|(from, to)| format!("{from}-{to}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn parse(text: &str) -> Option<Self> {
        let mut ranges = Self::default();
        for range in text.split_whitespace() {
            let (from, to) = range.split_once('-')?;
            ranges.insert(from.parse().ok()?, to.parse().ok()?);
        }
        Some(ranges)
    }
}

/// What a kept file's index says.
#[derive(Debug, PartialEq)]
struct Index {
    url: String,
    len: u64,
    tag: String,
    ranges: Ranges,
}

impl Index {
    fn to_text(&self) -> String {
        format!(
            "{MAGIC}\nurl {}\nlength {}\ntag {}\nranges {}\n",
            self.url,
            self.len,
            self.tag,
            self.ranges.to_text()
        )
    }

    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != MAGIC {
            return None;
        }
        let mut field = |name: &str| {
            lines
                .next()?
                .strip_prefix(name)?
                .strip_prefix(' ')
                .map(str::to_owned)
        };
        let url = field("url")?;
        let len = field("length")?.parse().ok()?;
        let tag = field("tag")?;
        let ranges = Ranges::parse(&field("ranges").unwrap_or_default())?;
        Some(Self {
            url,
            len,
            tag,
            ranges,
        })
    }
}

/// A file's store, what of it is here, and its length and version when they are known.
pub(crate) type Opened = (Store, Ranges, Option<(u64, String)>);

/// Where a file's bytes go as they come: a file kept on disk, or one that goes once read.
pub(crate) enum Store {
    Kept(Kept),
    Passing(Passing),
}

impl Store {
    /// For the file at `url`: the one kept, with what of it is here and its tag, when there
    /// is one; else a new one, empty, kept when a cache is set.
    pub fn open(url: &str) -> io::Result<Opened> {
        Ok(match Kept::open(url) {
            Some((kept, index)) => {
                let known = (!index.tag.is_empty()).then(|| (index.len, index.tag.clone()));
                (Store::Kept(kept), index.ranges, known)
            }
            None => (Store::Passing(Passing::new()?), Ranges::default(), None),
        })
    }

    /// The file is `len` long: one larger than all the cache may keep is not kept, but goes
    /// once read.
    pub fn fit(&mut self, len: u64) -> io::Result<()> {
        let limit = cache().config.as_ref().map_or(0, |config| config.limit);
        if matches!(self, Store::Kept(_)) && len > limit {
            *self = Store::Passing(Passing::new()?);
        }
        Ok(())
    }

    fn file(&mut self) -> &mut File {
        match self {
            Store::Kept(kept) => &mut kept.file,
            Store::Passing(passing) => &mut passing.file,
        }
    }

    /// Puts `bytes` at `at`.
    pub fn write_at(&mut self, at: u64, bytes: &[u8]) -> io::Result<()> {
        let file = self.file();
        file.seek(SeekFrom::Start(at))?;
        file.write_all(bytes)
    }

    /// Fills `out` from `at`, which must be here.
    pub fn read_at(&mut self, at: u64, out: &mut [u8]) -> io::Result<()> {
        let file = self.file();
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(out)
    }

    /// Notes what of the file is here, of which version: kept with it from now on. Without a
    /// tag, there is no telling the version, and it is not kept.
    pub fn note(&mut self, len: u64, tag: Option<&str>, ranges: &Ranges) {
        if let Store::Kept(kept) = self {
            kept.index = tag.map(|tag| Index {
                url: kept.url.clone(),
                len,
                tag: tag.to_owned(),
                ranges: ranges.clone(),
            });
            kept.save();
        }
    }

    /// The bytes held were of another version of the file: none are kept.
    pub fn forget(&mut self) {
        let _ = self.file().set_len(0);
        if let Store::Kept(kept) = self {
            kept.index = None;
            kept.save();
        }
    }
}

/// A file read from a server, not kept: on disk while it is read, so even a long one takes
/// no memory, then gone.
pub(crate) struct Passing {
    path: PathBuf,
    file: File,
}

impl Passing {
    fn new() -> io::Result<Self> {
        static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let made = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("freya-{}-{made}.playing", std::process::id()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        Ok(Self { path, file })
    }
}

impl Drop for Passing {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A file kept on disk while it is read.
pub(crate) struct Kept {
    dir: PathBuf,
    key: String,
    url: String,
    file: File,
    /// What to write to the index, when it is to be kept.
    index: Option<Index>,
}

impl Kept {
    fn open(url: &str) -> Option<(Self, Index)> {
        let dir = cache().config.clone()?.dir;
        std::fs::create_dir_all(&dir).ok()?;
        let key = key_of(url);
        let data = dir.join(format!("{key}.audio"));
        let on_disk = std::fs::metadata(&data).map_or(0, |m| m.len());
        // An index that says more is here than its file holds is of no use.
        let index = std::fs::read_to_string(dir.join(format!("{key}.index")))
            .ok()
            .and_then(|text| Index::parse(&text))
            .filter(|index| index.url == url && index.ranges.end() <= on_disk);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(index.is_none())
            .open(data)
            .ok()?;
        *cache().open.entry(key.clone()).or_default() += 1;
        let index = index.unwrap_or(Index {
            url: url.to_owned(),
            len: 0,
            tag: String::new(),
            ranges: Ranges::default(),
        });
        let kept = Self {
            dir,
            key,
            url: url.to_owned(),
            file,
            index: None,
        };
        // Used now: last to go.
        kept.touch();
        Some((kept, index))
    }

    fn touch(&self) {
        let index = self.dir.join(format!("{}.index", self.key));
        if let Ok(file) = File::options().append(true).open(index) {
            let _ = file.set_modified(SystemTime::now());
        }
    }

    /// Writes the index: what is here, or that nothing is to be kept.
    fn save(&self) {
        let path = self.dir.join(format!("{}.index", self.key));
        match &self.index {
            Some(index) => {
                let fresh = self.dir.join(format!("{}.index.new", self.key));
                if std::fs::write(&fresh, index.to_text()).is_ok() {
                    let _ = std::fs::rename(&fresh, &path);
                }
            }
            None => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

impl Drop for Kept {
    fn drop(&mut self) {
        let keep = self
            .index
            .as_ref()
            .is_some_and(|index| index.ranges.held() > 0);
        if keep {
            self.save();
        } else if cache().open.get(&self.key) == Some(&1) {
            remove(&self.dir, &self.key);
        }
        {
            let mut cache = cache();
            if let Some(count) = cache.open.get_mut(&self.key) {
                *count -= 1;
                if *count == 0 {
                    cache.open.remove(&self.key);
                }
            }
        }
        trim();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_join_and_tell_what_is_missing() {
        let mut ranges = Ranges::default();
        ranges.insert(100, 200);
        ranges.insert(300, 400);
        ranges.insert(0, 50);
        assert_eq!(ranges.0, vec![(0, 50), (100, 200), (300, 400)]);
        // Touching or overlapping ones become one.
        ranges.insert(200, 300);
        assert_eq!(ranges.0, vec![(0, 50), (100, 400)]);
        ranges.insert(40, 120);
        assert_eq!(ranges.0, vec![(0, 400)]);
        assert_eq!(ranges.from(10), 390);
        assert_eq!(ranges.from(400), 0);
        assert_eq!(ranges.missing_from(10, 1000), Some(400));
        assert_eq!(ranges.missing_from(10, 400), None);
        assert!(ranges.whole(400));
        assert!(!ranges.whole(401));
        ranges.insert(600, 700);
        assert_eq!(ranges.next_held(450), Some(600));
        assert_eq!(ranges.next_held(650), None);
        assert_eq!(ranges.held(), 500);
        assert_eq!(Ranges::parse(&ranges.to_text()), Some(ranges));
    }

    #[test]
    fn an_index_is_read_back_as_written() {
        let mut ranges = Ranges::default();
        ranges.insert(0, 1024);
        ranges.insert(4096, 8192);
        let index = Index {
            url: "https://192.168.1.2:4747/audio?path=%2FMusic%2Fa.flac".into(),
            len: 8192,
            tag: "\"2000-17a\"".into(),
            ranges,
        };
        assert_eq!(Index::parse(&index.to_text()), Some(index));
        assert_eq!(Index::parse("something else\n"), None);
    }

    #[test]
    fn keys_differ_by_address() {
        assert_eq!(key_of("a"), key_of("a"));
        assert_ne!(
            key_of("http://x/audio?path=a"),
            key_of("http://x/audio?path=b")
        );
        assert_eq!(key_of("a").len(), 16);
    }
}
