//! A file read from a server over HTTP as it plays. Its start and end are asked for at once,
//! so it opens quickly; then a thread fetches the rest, in order from where it is read, as
//! fast as it comes, with Range requests a few seconds long each, into the cache (see
//! `cache.rs`), so playing it again fetches nothing. Where it is read and is not here yet,
//! as after a seek, is fetched first; the reader waits for it. A request that fails is made
//! again, waiting longer each time, until the server has sent nothing for a minute.
//!
//! Whether playing can start, or go on after it ran dry, is told by how fast the network
//! was (see `throughput.rs`).

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use symphonia::core::io::MediaSource;

use crate::cache::{Ranges, Store};
use crate::source::{Bearer, RemoteSource};
use crate::stream::{self, Interrupt};
use crate::throughput::{self, Fetching};

/// How much is asked for of a file's start when it opens: soon here, so it starts at once.
#[cfg(not(test))]
const FIRST_PART: u64 = 512 * 1024;
#[cfg(test)]
const FIRST_PART: u64 = 4 * 1024;
/// How much of a file's end is asked for with its start: tags and indexes are read there.
#[cfg(not(test))]
const TAIL: u64 = 256 * 1024;
#[cfg(test)]
const TAIL: u64 = 1024;
/// The least and most one request asks for; little in tests, so their small files take many.
#[cfg(not(test))]
const SMALLEST_PART: u64 = 256 * 1024;
#[cfg(not(test))]
const LARGEST_PART: u64 = 16 * 1024 * 1024;
#[cfg(test)]
const SMALLEST_PART: u64 = 4 * 1024;
#[cfg(test)]
const LARGEST_PART: u64 = 16 * 1024;
/// About how long one request takes, as fast as the last one came.
const PART_TIME: Duration = Duration::from_secs(4);
/// How long one request may take.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// How often what is asked for at once is asked for, before the file is given up on.
const ATTEMPTS: u32 = 4;
/// How long to wait after a request failed, at first and at most.
const FIRST_WAIT: Duration = Duration::from_millis(250);
const LONGEST_WAIT: Duration = Duration::from_secs(4);
/// A file whose server sent nothing for this long is given up on.
#[cfg(not(test))]
const GIVE_UP: Duration = Duration::from_secs(60);
#[cfg(test)]
const GIVE_UP: Duration = Duration::from_secs(3);
/// A reader that waited this long for the network is heard: playing pauses until enough is
/// here to go on.
const STALL: Duration = Duration::from_millis(250);
/// With too little seen of the network to tell, playing starts with this many seconds here.
const START_AHEAD: f64 = 3.0;
/// How often what is here is noted in the cache's index while it is fetched.
const NOTE_EVERY: Duration = Duration::from_secs(5);
/// The longest file played from a server: an hour and a half of 24-bit, 192 kHz audio in
/// eight channels, and more. A server saying more is not believed.
const LONGEST_FILE: u64 = 64 * 1024 * 1024 * 1024;
/// How much a seek's search looks at, at each place: more than a FLAC frame of a CD's.
const LOOKED_AT: u64 = 32 * 1024;
/// The size of a part whose search goes on halving it: past this, it is read whole.
#[cfg(not(test))]
const SEARCHED_PART: u64 = 1024 * 1024;
#[cfg(test)]
const SEARCHED_PART: u64 = 4 * 1024;
/// How much of an answer is read before looking whether it is still wanted.
const READ_AT_ONCE: usize = 64 * 1024;

/// What a server answered to a Range request: where its bytes start, the file's length and
/// version, and the bytes.
struct Answer {
    from: u64,
    len: u64,
    tag: Option<String>,
    body: ureq::BodyReader<'static>,
}

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
    /// The `size` bytes from `start` on, or as many as there are, read at once; made again a
    /// few times while it fails, until playing something else, or `unwanted`, gives up on it.
    fn fetch(&self, start: u64, size: u64, unwanted: &AtomicBool) -> io::Result<Fetched> {
        let range = format!("bytes={start}-{}", start + size - 1);
        let fetched = self.again(|| self.fetch_once(&range, size, unwanted), unwanted)?;
        if fetched.from != start {
            return Err(io::Error::other("the server sent another part"));
        }
        Ok(fetched)
    }

    /// The last `size` bytes of the file, or all there are.
    fn tail(&self, size: u64, unwanted: &AtomicBool) -> io::Result<Fetched> {
        let range = format!("bytes=-{size}");
        self.again(|| self.fetch_once(&range, size, unwanted), unwanted)
    }

    fn again(
        &self,
        fetch: impl Fn() -> io::Result<Fetched>,
        unwanted: &AtomicBool,
    ) -> io::Result<Fetched> {
        let mut wait = FIRST_WAIT;
        let mut last = None;
        for _ in 0..ATTEMPTS {
            if self.interrupt.is_raised() || unwanted.load(Ordering::Relaxed) {
                return Err(stream::interrupted());
            }
            match fetch() {
                Ok(fetched) => return Ok(fetched),
                Err(e) => {
                    log::debug!("reading {}: {e}", self.url);
                    last = Some(e);
                    std::thread::sleep(wait);
                    wait *= 2;
                }
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other("could not be read")))
    }

    fn fetch_once(&self, range: &str, size: u64, unwanted: &AtomicBool) -> io::Result<Fetched> {
        let mut answer = self.ask_range(range, size)?;
        // Read a piece at a time, so a part no longer wanted is given up on.
        let mut bytes = Vec::new();
        let mut piece = vec![0; READ_AT_ONCE];
        loop {
            if unwanted.load(Ordering::Relaxed) || self.interrupt.is_raised() {
                return Err(stream::interrupted());
            }
            match answer.body.read(&mut piece)? {
                0 => break,
                read => {
                    throughput::came(read as u64);
                    bytes.extend_from_slice(&piece[..read]);
                }
            }
        }
        Ok(Fetched {
            from: answer.from,
            bytes,
            len: answer.len,
            tag: answer.tag,
        })
    }

    /// Asks for the bytes `range` names, at most `size`.
    fn ask_range(&self, range: &str, size: u64) -> io::Result<Answer> {
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
        let Some((from, len)) = said else {
            return Err(io::Error::other("the server did not say what it sent"));
        };
        let tag = answer
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = answer
            .into_body()
            .into_with_config()
            // Reaching the limit counts as going over it.
            .limit(size + 1)
            .reader();
        Ok(Answer {
            from,
            len,
            tag,
            body,
        })
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

/// Bytes read at once: where they start, them, and the file's length and version.
struct Fetched {
    from: u64,
    bytes: Vec<u8>,
    len: u64,
    tag: Option<String>,
}

/// A file being fetched, shared by its reader, the thread that fetches it, and the player,
/// which asks whether to play.
pub(crate) struct Download {
    got: Mutex<Got>,
    /// Told when bytes come, the reader waits for others, or the file is let go.
    changed: Condvar,
}

struct Got {
    store: Store,
    /// What of the file is here.
    ranges: Ranges,
    /// The file's length and version, once the server said; the version as the cache had it
    /// before then.
    len: u64,
    tag: Option<String>,
    /// Where the reader waits for bytes, when it does.
    wanted: Option<u64>,
    /// Where the reader read last: fetching goes on from there.
    read_at: u64,
    /// Why the file was given up on.
    failed: Option<String>,
    /// Whether the reader let it go.
    closed: bool,
    /// The bytes a second of its audio, once known.
    rate: Option<f64>,
    /// Since when the reader waits.
    waiting_since: Option<Instant>,
    /// Whether the reader waited long enough to be heard, since the player last asked.
    stalled: bool,
    /// When what is here was last noted in the cache.
    noted: Instant,
    /// Whether the server has another version of the file than the cache had, found after
    /// it was played from the cache: it goes, once played.
    stale: bool,
}

impl Got {
    /// The bytes of the file `len` long, of version `tag`, came: what is here of another
    /// version, or another length, goes. A file longer than any is refused.
    fn check_version(&mut self, len: u64, tag: Option<&str>) -> io::Result<()> {
        if len > LONGEST_FILE {
            return Err(io::Error::other(format!(
                "the server says the file is {len} bytes long"
            )));
        }
        let other = self.len != len || (self.tag.is_some() && self.tag.as_deref() != tag);
        if other && self.ranges.held() > 0 {
            log::info!("the server has another version of a file kept; fetching it again");
            self.store.forget();
            self.ranges = Ranges::default();
        }
        if self.len != len {
            self.store.fit(len)?;
        }
        self.len = len;
        self.tag = tag.map(str::to_owned);
        Ok(())
    }

    /// Keeps `bytes` that came for `at`.
    fn keep(&mut self, at: u64, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        // Only within the file, as long as the server said it is.
        if at + bytes.len() as u64 > self.len {
            return Err(io::Error::other("the server sent more than the file holds"));
        }
        self.store.write_at(at, bytes)?;
        self.ranges.insert(at, at + bytes.len() as u64);
        if self.noted.elapsed() >= NOTE_EVERY || self.ranges.whole(self.len) {
            self.note();
        }
        Ok(())
    }

    fn note(&mut self) {
        self.store.note(self.len, self.tag.as_deref(), &self.ranges);
        self.noted = Instant::now();
    }

    /// The next bytes to fetch, at most `size` of them: where the reader waits, else after
    /// where it read, else what was skipped before it. None when all of it is here.
    fn next_part(&self, size: u64) -> Option<(u64, u64)> {
        let len = self.len;
        let from = match self
            .wanted
            .filter(|&at| at < len && self.ranges.from(at) == 0)
        {
            Some(at) => at,
            None => self
                .ranges
                .missing_from(self.read_at, len)
                .or_else(|| self.ranges.missing_from(0, len))?,
        };
        let end = self
            .ranges
            .next_held(from)
            .unwrap_or(len)
            .min(len)
            .min(from + size);
        Some((from, end - from))
    }
}

impl Download {
    fn lock(&self) -> MutexGuard<'_, Got> {
        self.got.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether playing waits for the network now: the reader has waited long enough to be
    /// heard.
    pub fn buffering(&self) -> bool {
        self.lock()
            .waiting_since
            .is_some_and(|since| since.elapsed() >= STALL)
    }

    /// The bytes a second of the file's audio.
    pub fn set_rate(&self, rate: f64) {
        if rate.is_finite() && rate > 0.0 {
            self.lock().rate = Some(rate);
        }
    }

    /// Whether the reader waited long enough to be heard since this was last asked.
    pub fn take_stalled(&self) -> bool {
        std::mem::take(&mut self.lock().stalled)
    }

    /// Whether playing can start, or go on after it ran dry, without running dry again: as
    /// simulated with how fast the network was, from what is here ahead of the reader and
    /// what is still to come; with too little seen of it to tell, when a few seconds are
    /// here.
    pub fn can_play(&self) -> bool {
        let (buffered, missing, rate) = {
            let got = self.lock();
            if got.failed.is_some() {
                // The reader fails, and playing goes on with what follows.
                return true;
            }
            let ahead = got.len.saturating_sub(got.read_at);
            let missing = ahead - got.ranges.held_in(got.read_at, got.len).min(ahead);
            (got.ranges.from(got.read_at), missing, got.rate)
        };
        if missing == 0 {
            return true;
        }
        let Some(rate) = rate else {
            return buffered >= FIRST_PART;
        };
        let told = throughput::seen()
            .as_ref()
            .and_then(|seen| seen.plays_through(buffered, missing, rate));
        told.unwrap_or(buffered as f64 >= rate * START_AHEAD)
    }

    /// Waits up to `wait`, or until the file is let go.
    fn pause(&self, wait: Duration) {
        let got = self.lock();
        if !got.closed {
            let _ = self.changed.wait_timeout(got, wait);
        }
    }

    /// Fetches `size` bytes from `start` into the file, as they come. Gives up early when
    /// the reader waits elsewhere, or let the file go. Returns how many came.
    fn fetch_part(&self, fetcher: &Fetcher, start: u64, size: u64) -> io::Result<u64> {
        let range = format!("bytes={start}-{}", start + size - 1);
        let mut answer = fetcher.ask_range(&range, size)?;
        if answer.from != start {
            return Err(io::Error::other("the server sent another part"));
        }
        self.lock()
            .check_version(answer.len, answer.tag.as_deref())?;
        let mut piece = vec![0; READ_AT_ONCE];
        let mut at = start;
        loop {
            let read = match answer.body.read(&mut piece) {
                Ok(0) => break,
                Ok(read) => read,
                // What came counts: the rest is asked for again.
                Err(e) if at > start => {
                    log::debug!("reading {}: {e}", fetcher.url);
                    break;
                }
                Err(e) => return Err(e),
            };
            throughput::came(read as u64);
            let mut got = self.lock();
            got.keep(at, &piece[..read])?;
            at += read as u64;
            self.changed.notify_all();
            let elsewhere = got
                .wanted
                .is_some_and(|wanted| wanted < at || wanted >= start + size);
            if got.closed || elsewhere {
                break;
            }
        }
        Ok(at - start)
    }
}

impl Drop for Download {
    fn drop(&mut self) {
        let got = self.got.get_mut().unwrap_or_else(|e| e.into_inner());
        if got.stale {
            got.store.forget();
        } else {
            got.note();
        }
    }
}

/// Fetches the rest of the file, until all of it is here, the reader lets it go, or the
/// server sent nothing for [`GIVE_UP`]. A file that was whole in the cache is only asked
/// whether the server still has that version.
fn fetch_rest(download: &Download, fetcher: &Fetcher, _fetching: Fetching, was_whole: bool) {
    if was_whole {
        let still = fetcher
            .ask_range("bytes=0-0", 1)
            .map(|answer| (answer.len, answer.tag));
        let mut got = download.lock();
        if let Ok((len, tag)) = still
            && (len != got.len || tag != got.tag)
        {
            log::info!("a file kept was changed on the server; it is fetched again next time");
            got.stale = true;
        }
        return;
    }
    let mut size = SMALLEST_PART.max(FIRST_PART * 2);
    let mut heard = Instant::now();
    let mut wait = FIRST_WAIT;
    loop {
        let part = {
            let got = download.lock();
            if got.closed {
                return;
            }
            got.next_part(size)
        };
        let Some((start, asked)) = part else {
            return;
        };
        let started = Instant::now();
        match download.fetch_part(fetcher, start, asked) {
            Ok(came) => {
                if came > 0 {
                    heard = Instant::now();
                    wait = FIRST_WAIT;
                }
                // The next one about as long, as fast as this came.
                let took = started.elapsed().as_secs_f64().max(0.001);
                if came == asked {
                    let fast = came as f64 / took * PART_TIME.as_secs_f64();
                    size = (fast as u64).clamp(SMALLEST_PART, LARGEST_PART);
                }
            }
            Err(e) => {
                if download.lock().closed {
                    return;
                }
                log::debug!("fetching {}: {e}", fetcher.url);
                if heard.elapsed() >= GIVE_UP {
                    log::warn!("giving up on {}: {e}", fetcher.url);
                    download.lock().failed = Some(e.to_string());
                    download.changed.notify_all();
                    return;
                }
                download.pause(wait);
                wait = (wait * 2).min(LONGEST_WAIT);
            }
        }
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
        while end - start > SEARCHED_PART {
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
    download: Arc<Download>,
    /// What a seek told it will read, asked for all at once before it reads; and what tells
    /// it, until a source takes it.
    wanted: crossbeam_channel::Receiver<Vec<(u64, u64)>>,
    seek_ahead: Option<SeekAhead>,
    len: u64,
    pos: u64,
}

impl RemoteFile {
    /// The file `remote`, of a server that answers Range requests: its start and end here,
    /// from the cache or the server, and the rest being fetched.
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
        let (store, ranges, known) = Store::open(&remote.url)?;
        let (len, tag) = known.map_or((0, None), |(len, tag)| (len, Some(tag)));
        let was_whole = len > 0 && ranges.whole(len);
        let fetching = Fetching::start();
        let download = Arc::new(Download {
            got: Mutex::new(Got {
                store,
                ranges,
                len,
                tag,
                wanted: None,
                read_at: 0,
                failed: None,
                closed: false,
                rate: None,
                waiting_since: None,
                stalled: false,
                noted: Instant::now(),
                stale: false,
            }),
            changed: Condvar::new(),
        });
        if !was_whole {
            Self::fetch_ends(&download, &fetcher)?;
        }
        let len = download.lock().len;
        let (tell, wanted) = crossbeam_channel::unbounded();
        let start = {
            let mut got = download.lock();
            let here = got.ranges.from(0).min(FIRST_PART) as usize;
            let mut start = vec![0; here];
            got.store.read_at(0, &mut start)?;
            start
        };
        let seek_ahead = flac_frames(&start)
            .filter(|(_, table)| !table)
            .map(|(first_frame, _)| SeekAhead {
                first_frame,
                len,
                wanted: tell,
            });
        let (fetching_rest, asking) = (Arc::clone(&download), fetcher.clone());
        std::thread::Builder::new()
            .name("freya-remote-file".into())
            .spawn(move || fetch_rest(&fetching_rest, &asking, fetching, was_whole))?;
        Ok(Self {
            fetcher,
            download,
            wanted,
            seek_ahead,
            len,
            pos: 0,
        })
    }

    /// Fetches the file's start and its end at once, as far as they are not here.
    fn fetch_ends(download: &Download, fetcher: &Fetcher) -> io::Result<()> {
        let unwanted = AtomicBool::new(false);
        let tail_here = {
            let got = download.lock();
            let tail = TAIL.min(got.len);
            got.len > 0 && got.ranges.held_in(got.len - tail, got.len) == tail
        };
        let (head, tail) = std::thread::scope(|threads| {
            let tail = (!tail_here).then(|| threads.spawn(|| fetcher.tail(TAIL, &unwanted)));
            let head = {
                let got = download.lock();
                let here = got.ranges.from(0);
                (here < FIRST_PART && !(got.len > 0 && here >= got.len)).then_some(here)
            }
            .map(|here| fetcher.fetch(here, FIRST_PART - here, &unwanted));
            (head, tail.map(|tail| tail.join()))
        });
        let mut got = download.lock();
        if let Some(head) = head {
            let head = head?;
            got.check_version(head.len, head.tag.as_deref())?;
            got.keep(head.from, &head.bytes)?;
        }
        // The end is only of use as of the same version.
        if let Some(Ok(Ok(tail))) = tail
            && tail.len == got.len
            && tail.tag == got.tag
            && tail.from + tail.bytes.len() as u64 == tail.len
        {
            got.keep(tail.from, &tail.bytes)?;
        }
        if got.len == 0 {
            return Err(io::Error::other(
                "the server did not say how long the file is",
            ));
        }
        Ok(())
    }

    /// The file as it is fetched, for the player to ask whether to play.
    pub fn download(&self) -> Arc<Download> {
        Arc::clone(&self.download)
    }

    /// What a seek in it asks for ahead, when it is a FLAC file without a seek table.
    pub fn take_seek_ahead(&mut self) -> Option<SeekAhead> {
        self.seek_ahead.take()
    }

    /// Asks for what a seek said it will read, all at once, and keeps it.
    fn fetch_wanted(&mut self) {
        let Some(wanted) = self.wanted.try_iter().last() else {
            return;
        };
        let wanted: Vec<(u64, u64)> = {
            let got = self.download.lock();
            wanted
                .into_iter()
                .filter(|&(start, _)| start < self.len && got.ranges.from(start) == 0)
                .collect()
        };
        if wanted.is_empty() {
            return;
        }
        let fetched: Vec<_> = std::thread::scope(|threads| {
            let asked: Vec<_> = wanted
                .iter()
                .map(|&(start, size)| {
                    let fetcher = &self.fetcher;
                    let size = size.min(self.len - start);
                    threads.spawn(move || fetcher.fetch(start, size, &AtomicBool::new(false)))
                })
                .collect();
            asked
                .into_iter()
                .filter_map(|asked| asked.join().ok())
                .collect()
        });
        let mut got = self.download.lock();
        for fetched in fetched.into_iter().flatten() {
            if fetched.len == got.len && fetched.tag == got.tag {
                let _ = got.keep(fetched.from, &fetched.bytes);
            }
        }
        self.download.changed.notify_all();
    }
}

impl Drop for RemoteFile {
    fn drop(&mut self) {
        self.download.lock().closed = true;
        self.download.changed.notify_all();
    }
}

impl Read for RemoteFile {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || out.is_empty() {
            return Ok(0);
        }
        self.fetch_wanted();
        let download = &self.download;
        let mut got = download.lock();
        loop {
            let here = got.ranges.from(self.pos);
            if here > 0 {
                let count = out.len().min(here as usize);
                got.store.read_at(self.pos, &mut out[..count])?;
                self.pos += count as u64;
                got.read_at = self.pos;
                got.wanted = None;
                if let Some(since) = got.waiting_since.take()
                    && since.elapsed() >= STALL
                {
                    got.stalled = true;
                }
                return Ok(count);
            }
            if let Some(why) = &got.failed {
                return Err(io::Error::other(why.clone()));
            }
            if self.fetcher.interrupt.is_raised() {
                got.wanted = None;
                got.waiting_since = None;
                return Err(stream::interrupted());
            }
            got.wanted = Some(self.pos);
            got.read_at = self.pos;
            got.waiting_since.get_or_insert_with(Instant::now);
            download.changed.notify_all();
            got = download
                .changed
                .wait_timeout(got, Duration::from_millis(20))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
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

    /// What is fetched next of a file 100 bytes long, of which `held` is here.
    fn next_of(held: &[(u64, u64)], wanted: Option<u64>, read_at: u64) -> Option<(u64, u64)> {
        let mut ranges = Ranges::default();
        for &(from, to) in held {
            ranges.insert(from, to);
        }
        let got = Got {
            store: Store::open("http://nowhere/").unwrap().0,
            ranges,
            len: 100,
            tag: None,
            wanted,
            read_at,
            failed: None,
            closed: false,
            rate: None,
            waiting_since: None,
            stalled: false,
            noted: Instant::now(),
            stale: false,
        };
        got.next_part(30)
    }

    #[test]
    fn a_server_is_not_believed_beyond_the_files_length() {
        let mut got = Got {
            store: Store::open("http://nowhere/").unwrap().0,
            ranges: Ranges::default(),
            len: 0,
            tag: None,
            wanted: None,
            read_at: 0,
            failed: None,
            closed: false,
            rate: None,
            waiting_since: None,
            stalled: false,
            noted: Instant::now(),
            stale: false,
        };
        assert!(got.check_version(LONGEST_FILE + 1, None).is_err());
        got.check_version(100, None).unwrap();
        assert!(got.keep(90, &[0; 20]).is_err(), "past its end");
        // Far past it, which would make a file as large on a disk without holes.
        assert!(got.keep(1 << 40, &[0; 1]).is_err());
        got.keep(90, &[1; 10]).unwrap();
        assert_eq!(got.ranges.held(), 10);
    }

    #[test]
    fn what_is_read_is_fetched_first_then_what_follows_then_what_was_skipped() {
        // The start and the end are here; the reader reads at 10: on from the start's end.
        assert_eq!(next_of(&[(0, 20), (90, 100)], None, 10), Some((20, 30)));
        // Up to what is here already.
        assert_eq!(next_of(&[(0, 20), (40, 100)], None, 10), Some((20, 20)));
        // The reader waits at 60, after a seek: fetched from there.
        assert_eq!(next_of(&[(0, 20), (90, 100)], Some(60), 60), Some((60, 30)));
        // Read on to the end: what was skipped before.
        assert_eq!(next_of(&[(0, 20), (50, 100)], None, 95), Some((20, 30)));
        assert_eq!(next_of(&[(0, 100)], None, 0), None);
    }
}
