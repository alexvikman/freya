//! A file read from a server over HTTP as it is played, a part at a time with Range requests,
//! the next part fetched on a thread while this one plays, so the network's delay is not
//! heard. The first part is small, so the sound starts at once, and the parts grow while they
//! come quickly; a seek starts small again, and a part fetched ahead that is no longer wanted
//! is given up. Seeking within what was fetched asks nothing. A request that fails is made
//! again a few times before the file is given up on.

use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use symphonia::core::io::MediaSource;

use crate::source::{Bearer, RemoteSource};
use crate::stream::Interrupt;

/// How much the first request asks for, and the most one asks for; little in tests, so their
/// small files take many.
#[cfg(not(test))]
const FIRST_PART: u64 = 1024 * 1024;
#[cfg(not(test))]
const LARGEST_PART: u64 = 4 * 1024 * 1024;
#[cfg(test)]
const FIRST_PART: u64 = 4 * 1024;
#[cfg(test)]
const LARGEST_PART: u64 = 16 * 1024;
/// A part that came quicker than this is followed by one twice as large; one slower than
/// [`SLOW_PART`], by one half as large.
const QUICK_PART: Duration = Duration::from_secs(1);
const SLOW_PART: Duration = Duration::from_secs(5);
/// How long one request may take, and how often one that failed is made again.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const ATTEMPTS: u32 = 4;
const FIRST_WAIT: Duration = Duration::from_millis(250);
/// How much of a file's end is asked for with its start.
#[cfg(not(test))]
const TAIL: u64 = 256 * 1024;
#[cfg(test)]
const TAIL: u64 = 1024;
/// How many parts are kept, the ones read last, and how much of them at most.
const KEPT_PARTS: usize = 64;
const KEPT_BYTES: usize = 16 * 1024 * 1024;
/// How much a seek's search looks at, at each place: more than a FLAC frame of a CD's.
const LOOKED_AT: u64 = 32 * 1024;
/// How much of a part is read before looking whether it is still wanted.
const READ_AT_ONCE: usize = 64 * 1024;

/// A part of the file from where it was asked for, the file's length, and how long it took.
type Fetched = io::Result<(Vec<u8>, u64, Duration)>;

/// Asks the server for parts of the file.
#[derive(Clone)]
struct Fetcher {
    agent: ureq::Agent,
    url: String,
    /// The access token, when the server asks for one.
    bearer: Option<Bearer>,
    interrupt: Interrupt,
}

impl Fetcher {
    /// The `size` bytes from `start` on, or as many as there are, and the file's length; made
    /// again while it fails, until playing something else, or `unwanted`, gives up on it.
    fn fetch(&self, start: u64, size: u64, unwanted: &AtomicBool) -> Fetched {
        let started = Instant::now();
        let mut wait = FIRST_WAIT;
        let mut last = None;
        for _ in 0..ATTEMPTS {
            if self.interrupt.is_raised() || unwanted.load(Ordering::Relaxed) {
                return Err(io::Error::other("interrupted"));
            }
            match self.request(start, size, unwanted) {
                Ok((bytes, length)) => return Ok((bytes, length, started.elapsed())),
                Err(e) => {
                    log::debug!("reading {} at {start}: {e}", self.url);
                    last = Some(e);
                    std::thread::sleep(wait);
                    wait *= 2;
                }
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other("could not be read")))
    }

    fn request(&self, start: u64, size: u64, unwanted: &AtomicBool) -> io::Result<(Vec<u8>, u64)> {
        let range = format!("bytes={start}-{}", start + size - 1);
        match self.range(&range, size, unwanted)? {
            (from, bytes, length) if from == start => Ok((bytes, length)),
            _ => Err(io::Error::other("the server sent another part")),
        }
    }

    /// The last `size` bytes of the file, or all there are: where they start, and them.
    fn tail(&self, size: u64) -> io::Result<(u64, Vec<u8>)> {
        let (from, bytes, _) =
            self.range(&format!("bytes=-{size}"), size, &AtomicBool::new(false))?;
        Ok((from, bytes))
    }

    /// The bytes `range` asks for, at most `size`: where they start, them, and the file's
    /// length.
    fn range(
        &self,
        range: &str,
        size: u64,
        unwanted: &AtomicBool,
    ) -> io::Result<(u64, Vec<u8>, u64)> {
        let token = self.bearer.as_ref().and_then(|bearer| bearer(None));
        let mut answer = self.ask(range, token.as_deref())?;
        // Ended or refused: asked once more with a renewed one.
        if answer.status().as_u16() == 401
            && let (Some(bearer), Some(sent)) = (&self.bearer, &token)
            && let Some(renewed) = bearer(Some(sent))
            && &renewed != sent
        {
            answer = self.ask(range, Some(&renewed))?;
        }
        if answer.status().as_u16() != 206 {
            return Err(io::Error::other(format!(
                "the server answered {} to a range",
                answer.status()
            )));
        }
        // "bytes 0-1048575/52428800": where it starts, and the length after the slash.
        let said = answer
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes "))
            .and_then(|v| {
                let (span, length) = v.split_once('/')?;
                let from = span.split_once('-')?.0.trim().parse::<u64>().ok()?;
                Some((from, length.trim().parse::<u64>().ok()?))
            });
        let Some((from, length)) = said else {
            return Err(io::Error::other("the server did not say what it sent"));
        };
        // Read a piece at a time, so a part no longer wanted is given up on.
        let mut body = answer
            .body_mut()
            .with_config()
            // Reaching the limit counts as going over it.
            .limit(size + 1)
            .reader();
        let mut bytes = Vec::new();
        let mut piece = vec![0; READ_AT_ONCE];
        loop {
            if unwanted.load(Ordering::Relaxed) {
                return Err(io::Error::other("no longer wanted"));
            }
            match body.read(&mut piece)? {
                0 => break,
                read => bytes.extend_from_slice(&piece[..read]),
            }
        }
        Ok((from, bytes, length))
    }

    fn ask(
        &self,
        range: &str,
        token: Option<&str>,
    ) -> io::Result<ureq::http::Response<ureq::Body>> {
        let mut request = self
            .agent
            .get(&self.url)
            .header("Range", range)
            .config()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build();
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        request.call().map_err(io::Error::other)
    }
}

/// A part being fetched ahead, on a thread: where it starts, what it is told, and whether it
/// is still wanted.
struct Ahead {
    start: u64,
    fetched: crossbeam_channel::Receiver<Fetched>,
    unwanted: Arc<AtomicBool>,
}

impl Drop for Ahead {
    fn drop(&mut self) {
        self.unwanted.store(true, Ordering::Relaxed);
    }
}

/// What a seek in a FLAC file without a seek table reads, asked for all at once: its search
/// halves the bytes of the frames again and again, each half a round trip to the server
/// otherwise.
pub(crate) struct SeekAhead {
    first_frame: u64,
    len: u64,
    wanted: crossbeam_channel::Sender<Vec<(u64, u64)>>,
}

impl SeekAhead {
    /// Asks for what a seek to `fraction` of the file's audio reads: the places its search
    /// looks at, as Symphonia's FLAC reader halves the frames' bytes, while they are far
    /// apart, then all of the part it ends in. As the bytes a second vary, the search is
    /// followed for an aim a little before and after too.
    pub fn seek_to(&self, fraction: f64) {
        let mut wanted: Vec<(u64, u64)> = Vec::new();
        for off in [0.0, -AIM_OFF, AIM_OFF] {
            let searched = self.searched((fraction + off).clamp(0.0, 1.0));
            // Where it ends is read whole for the aim itself, which is played from.
            let looked = searched.len() - usize::from(off != 0.0);
            for &asked in &searched[..looked] {
                if !wanted.iter().any(|&(start, _)| start == asked.0) {
                    wanted.push(asked);
                }
            }
        }
        let _ = self.wanted.send(wanted);
    }

    /// What the search for `fraction` of the audio's bytes reads: the places it looks at, and
    /// last, the part it ends in.
    fn searched(&self, fraction: f64) -> Vec<(u64, u64)> {
        let (mut start, mut end) = (self.first_frame, self.len);
        let aimed = start + (end.saturating_sub(start) as f64 * fraction) as u64;
        let mut searched = Vec::new();
        while end - start > FIRST_PART {
            let middle = (start + end) / 2;
            searched.push((middle, LOOKED_AT));
            if aimed < middle {
                end = middle;
            } else {
                start = middle;
            }
        }
        searched.push((start, end - start));
        searched
    }
}

/// How far before and after the aim a seek's search is followed too, of the audio's bytes.
const AIM_OFF: f64 = 0.025;

/// Where a FLAC file's frames start, after its metadata in `bytes`, its start; and whether
/// it has a seek table. None for another file, or metadata longer than `bytes`.
fn flac_frames(bytes: &[u8]) -> Option<(u64, bool)> {
    if !bytes.starts_with(b"fLaC") {
        return None;
    }
    let (mut at, mut table) = (4usize, false);
    loop {
        let header = bytes.get(at..at + 4)?;
        let length = u32::from_be_bytes([0, header[1], header[2], header[3]]) as usize;
        table |= header[0] & 0x7f == 3;
        at += 4 + length;
        if header[0] & 0x80 != 0 {
            return Some((at as u64, table));
        }
    }
}

pub(crate) struct RemoteFile {
    fetcher: Fetcher,
    /// What a seek told it will read, asked for all at once before it reads; and what tells
    /// it, until a source takes it.
    wanted: crossbeam_channel::Receiver<Vec<(u64, u64)>>,
    seek_ahead: Option<SeekAhead>,
    len: u64,
    pos: u64,
    /// The parts read last, the last first: where each starts in the file, and its bytes. A
    /// reader of tags at the end of a file, or of an index there, goes back to its start.
    parts: VecDeque<(u64, Vec<u8>)>,
    /// How much the next part asks for.
    size: u64,
    /// The one after it, being fetched on a thread.
    ahead: Option<Ahead>,
}

impl RemoteFile {
    /// The file `remote`, of a server that answers Range requests; its first part read.
    pub fn open(remote: &RemoteSource, interrupt: Interrupt) -> io::Result<Self> {
        let agent = remote.agent.as_deref().cloned().unwrap_or_else(|| {
            ureq::Agent::config_builder()
                .timeout_global(Some(REQUEST_TIMEOUT))
                .user_agent(concat!("Freya/", env!("CARGO_PKG_VERSION")))
                .http_status_as_error(false)
                .build()
                .into()
        });
        let fetcher = Fetcher {
            agent,
            url: remote.url.clone(),
            bearer: remote.bearer.clone(),
            interrupt,
        };
        // Its end asked for beside its start: tags and indexes are read there first.
        let (tell, tail) = crossbeam_channel::bounded(1);
        let asked = fetcher.clone();
        let _ = std::thread::Builder::new()
            .name("freya-remote-file".into())
            .spawn(move || {
                let _ = tell.send(asked.tail(TAIL));
            });
        let (first, len, took) = fetcher.fetch(0, FIRST_PART, &AtomicBool::new(false))?;
        let mut parts = VecDeque::from([(0, first)]);
        if let Ok(Ok((from, bytes))) = tail.recv_timeout(REQUEST_TIMEOUT)
            && from >= parts[0].1.len() as u64
            && from + bytes.len() as u64 == len
        {
            parts.push_back((from, bytes));
        }
        let (tell, wanted) = crossbeam_channel::unbounded();
        let seek_ahead =
            flac_frames(&parts[0].1)
                .filter(|(_, table)| !table)
                .map(|(first_frame, _)| SeekAhead {
                    first_frame,
                    len,
                    wanted: tell,
                });
        let mut file = Self {
            fetcher,
            wanted,
            seek_ahead,
            len,
            pos: 0,
            parts,
            size: FIRST_PART,
            ahead: None,
        };
        file.grow(took);
        file.fetch_ahead();
        Ok(file)
    }

    /// What a seek in it asks for ahead, when it is a FLAC file without a seek table.
    pub fn take_seek_ahead(&mut self) -> Option<SeekAhead> {
        self.seek_ahead.take()
    }

    /// Asks for what a seek said it will read, all at once, and keeps it.
    fn fetch_wanted(&mut self) {
        let wanted: Vec<(u64, u64)> = self
            .wanted
            .try_iter()
            .last()
            .unwrap_or_default()
            .into_iter()
            .filter(|(start, _)| *start < self.len && self.part_at(*start).is_none())
            .collect();
        if wanted.is_empty() {
            return;
        }
        let fetched: Vec<_> = std::thread::scope(|threads| {
            let asked: Vec<_> = wanted
                .iter()
                .map(|&(start, size)| {
                    let fetcher = &self.fetcher;
                    threads.spawn(move || {
                        let fetched = fetcher.fetch(start, size, &AtomicBool::new(false));
                        (start, fetched)
                    })
                })
                .collect();
            asked
                .into_iter()
                .filter_map(|asked| asked.join().ok())
                .collect()
        });
        for (start, fetched) in fetched {
            if let Ok((bytes, _, _)) = fetched {
                self.keep(start, bytes);
            }
        }
    }

    /// Keeps the part `bytes`, at `start`, as the one read last; those read longest ago go
    /// beyond [`KEPT_PARTS`] or [`KEPT_BYTES`].
    fn keep(&mut self, start: u64, bytes: Vec<u8>) {
        self.parts.push_front((start, bytes));
        self.parts.truncate(KEPT_PARTS);
        while self.parts.len() > 1
            && self
                .parts
                .iter()
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>()
                > KEPT_BYTES
        {
            self.parts.pop_back();
        }
    }

    /// The parts grow while they come quickly, and shrink when they come slowly.
    fn grow(&mut self, took: Duration) {
        if took < QUICK_PART {
            self.size = (self.size * 2).min(LARGEST_PART);
        } else if took > SLOW_PART {
            self.size = (self.size / 2).max(FIRST_PART);
        }
    }

    /// Starts fetching the part after the one read, unless it is the file's end or kept.
    fn fetch_ahead(&mut self) {
        let Some((start, bytes)) = self.parts.front() else {
            return;
        };
        let next = start + bytes.len() as u64;
        if next >= self.len || bytes.is_empty() || self.part_at(next).is_some() {
            self.ahead = None;
            return;
        }
        let (tell, fetched) = crossbeam_channel::bounded(1);
        let unwanted = Arc::new(AtomicBool::new(false));
        let (fetcher, size, given_up) = (self.fetcher.clone(), self.size, Arc::clone(&unwanted));
        let started = std::thread::Builder::new()
            .name("freya-remote-file".into())
            .spawn(move || {
                let _ = tell.send(fetcher.fetch(next, size, &given_up));
            });
        self.ahead = started.is_ok().then_some(Ahead {
            start: next,
            fetched,
            unwanted,
        });
    }

    /// Makes the part read the one holding `pos`: the one fetched ahead when it is, else
    /// asked for now, small, after a seek.
    fn load(&mut self, pos: u64) -> io::Result<()> {
        let unwanted = AtomicBool::new(false);
        let fetched = match self.ahead.take() {
            Some(ahead) if ahead.start == pos => ahead
                .fetched
                .recv()
                .unwrap_or_else(|_| self.fetcher.fetch(pos, self.size, &unwanted)),
            // Sought elsewhere: what was fetched ahead is given up, by being dropped.
            _ => {
                self.size = FIRST_PART;
                self.fetcher.fetch(pos, self.size, &unwanted)
            }
        }?;
        self.grow(fetched.2);
        self.keep(pos, fetched.0);
        self.fetch_ahead();
        Ok(())
    }

    /// Which of the parts kept holds `pos`.
    fn part_at(&self, pos: u64) -> Option<usize> {
        self.parts
            .iter()
            .position(|(start, bytes)| pos >= *start && pos < start + bytes.len() as u64)
    }
}

impl Read for RemoteFile {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || out.is_empty() {
            return Ok(0);
        }
        self.fetch_wanted();
        match self.part_at(self.pos) {
            Some(0) => {}
            // Kept: read first from now on, and what follows it fetched ahead.
            Some(at) => {
                if let Some(part) = self.parts.remove(at) {
                    self.parts.push_front(part);
                }
                if self.ahead.is_none() {
                    self.fetch_ahead();
                }
            }
            None => self.load(self.pos)?,
        }
        let Some((start, bytes)) = self.parts.front() else {
            return Err(io::Error::other("nothing was read"));
        };
        let from = (self.pos - start) as usize;
        let count = out.len().min(bytes.len() - from);
        if count == 0 {
            return Err(io::Error::other("the server sent less than it said"));
        }
        out[..count].copy_from_slice(&bytes[from..from + count]);
        self.pos += count as u64;
        Ok(count)
    }
}

impl Seek for RemoteFile {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(pos) => Some(pos),
            SeekFrom::End(delta) => self.len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        };
        // Fetched only once read.
        self.pos = pos.ok_or_else(|| io::Error::other("sought before the start"))?;
        Ok(self.pos)
    }
}

impl MediaSource for RemoteFile {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A FLAC file's start: its marker, then metadata blocks, each a kind and a length.
    fn flac_start(blocks: &[(u8, usize)]) -> Vec<u8> {
        let mut bytes = b"fLaC".to_vec();
        for (i, &(kind, length)) in blocks.iter().enumerate() {
            let last = if i + 1 == blocks.len() { 0x80 } else { 0 };
            bytes.push(last | kind);
            bytes.extend_from_slice(&(length as u32).to_be_bytes()[1..]);
            bytes.extend(std::iter::repeat_n(0, length));
        }
        bytes
    }

    #[test]
    fn a_flac_files_frames_are_found_after_its_metadata() {
        // Stream info, a picture, a seek table.
        let start = flac_start(&[(0, 34), (6, 1000), (3, 18)]);
        assert_eq!(flac_frames(&start), Some((start.len() as u64, true)));
        let start = flac_start(&[(0, 34), (4, 100)]);
        assert_eq!(flac_frames(&start), Some((start.len() as u64, false)));
        // Only the blocks' heads are read: with one cut short, where the frames start is not
        // known; nor in a file that is not FLAC.
        assert_eq!(flac_frames(&start[..44]), None);
        assert_eq!(flac_frames(b"ID3\x04"), None);
    }

    #[test]
    fn a_seek_asks_for_where_its_search_looks_at_once() {
        let (tell, wanted) = crossbeam_channel::unbounded();
        let ahead = SeekAhead {
            first_frame: 8192,
            len: 100 * 1024 * 1024,
            wanted: tell,
        };
        ahead.seek_to(0.63);
        let asked = wanted.try_recv().unwrap();
        // Halving the frames' bytes, as the search does: first their middle.
        assert_eq!(asked[0].0, (8192 + 100 * 1024 * 1024) / 2);
        assert!(asked.iter().all(|&(start, size)| start + size <= ahead.len));
        // All of the part it ends in, around the aim, to play from.
        let aimed = 8192 + ((100 * 1024 * 1024 - 8192) as f64 * 0.63) as u64;
        assert!(
            asked
                .iter()
                .any(|&(start, size)| (start..start + size).contains(&aimed))
        );
        // Each place once.
        let mut starts: Vec<u64> = asked.iter().map(|&(start, _)| start).collect();
        starts.dedup();
        assert_eq!(starts.len(), asked.len());
    }
}
