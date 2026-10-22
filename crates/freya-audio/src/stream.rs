//! Internet radio: HTTP streams with Shoutcast/Icecast ("ICY") metadata.
//!
//! A network thread reads the stream into a buffer of a few seconds, taking out the song
//! titles the station sends between the audio bytes. The decoder reads audio from that buffer
//! and sees each title at the point in the audio where it was sent. Network hiccups are
//! smoothed over by the buffer, and a stalled station never blocks the decoder: it can always
//! be interrupted to play something else.

use std::collections::VecDeque;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

use crate::Error;

/// Audio buffered ahead before playback starts, and again after the buffer ran dry.
const PREBUFFER: Duration = Duration::from_secs(2);
/// Bitrate assumed when the station does not say.
const DEFAULT_KBPS: u32 = 128;
/// At most this much is read ahead of the decoder.
const MAX_BUFFERED: usize = 1024 * 1024;
/// A station that sends nothing for this long is given up on.
const STALL_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a waiting reader checks whether it should give up.
const WAIT_STEP: Duration = Duration::from_millis(50);
/// Most bytes handed to the decoder per read. The decoder reads ahead by one read, so this
/// bounds how early a new title shows: 4 KiB is a quarter of a second at 128 kbps.
const READ_SIZE: usize = 4 * 1024;

/// Counts commands that should stop whatever the decoder is waiting for, like playing
/// something else. The player raises it before sending such a command; the decoder lowers it
/// when it handles one.
#[derive(Clone, Default)]
pub(crate) struct Interrupt(Arc<AtomicUsize>);

impl Interrupt {
    pub fn raise(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }

    pub fn lower(&self) {
        let _ = self
            .0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
    }

    pub fn is_raised(&self) -> bool {
        self.0.load(Ordering::Acquire) > 0
    }
}

/// Whether `path` names an internet stream rather than a file.
pub fn is_url(path: &str) -> bool {
    let lower = path.get(..8).unwrap_or(path).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Whether `ip` is on this computer or its network: a loopback, private, link-local, shared
/// (carrier-grade NAT), multicast or unspecified address, or an IPv6 one standing for one.
pub fn is_near(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [first, second, ..] = ip.octets();
            first == 0
                || ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_multicast()
                || (first == 100 && second & 0xc0 == 64)
        }
        IpAddr::V6(ip) => {
            // An IPv4 address in IPv6: mapped, compatible, or behind NAT64's prefix.
            let nat64 = ip.segments()[..6] == [0x64, 0xff9b, 0, 0, 0, 0];
            let within = ip
                .to_ipv4()
                .or_else(|| nat64.then(|| Ipv4Addr::from_bits(ip.to_bits() as u32)));
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
                || ip.segments()[0] & 0xffc0 == 0xfec0
                || within.is_some_and(|ip| is_near(IpAddr::V4(ip)))
        }
    }
}

/// Resolves names as ureq does, but only to addresses beyond this computer and its network.
/// Each request is resolved, a redirect's too, so whatever a URL says, and wherever it leads,
/// nothing near is asked.
#[derive(Debug, Default)]
struct Far(DefaultResolver);

impl Resolver for Far {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let mut far = self.empty();
        for address in self.0.resolve(uri, config, timeout)?.iter() {
            if !is_near(address.ip()) {
                far.push(*address);
            }
        }
        if far.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(far)
    }
}

/// What the station says about itself in its response headers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StationInfo {
    pub name: Option<String>,
    pub genre: Option<String>,
    pub bitrate_kbps: Option<u32>,
    /// The audio format, e.g. "audio/mpeg".
    pub content_type: Option<String>,
}

impl StationInfo {
    /// A file extension that helps the decoder recognize the format.
    pub fn extension_hint(&self) -> Option<&'static str> {
        let content_type = self.content_type.as_deref()?.to_ascii_lowercase();
        Some(match content_type.split(';').next()?.trim() {
            "audio/mpeg" | "audio/mp3" | "audio/mpeg3" => "mp3",
            "audio/aac" | "audio/aacp" | "audio/x-aac" => "aac",
            "audio/ogg" | "application/ogg" | "audio/x-ogg" => "ogg",
            "audio/flac" | "audio/x-flac" => "flac",
            "audio/mp4" | "audio/x-m4a" => "m4a",
            "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
            _ => return None,
        })
    }
}

/// The latest song title a stream reached, for whoever decodes it. The stream itself ends up
/// inside the decoder, so titles come out through this.
#[derive(Clone, Default)]
pub(crate) struct Titles(Arc<Mutex<Option<String>>>);

impl Titles {
    /// A title that arrived since the last call.
    pub fn take(&self) -> Option<String> {
        self.0.lock().ok()?.take()
    }

    fn put(&self, title: String) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(title);
        }
    }
}

/// An open stream: audio is read from it with [`Read`]; titles come out through [`Titles`].
pub(crate) struct Stream {
    buffer: Arc<Buffer>,
    interrupt: Interrupt,
    /// Bytes that make up [`PREBUFFER`] of audio.
    prebuffer: usize,
    buffering: bool,
    title: Option<String>,
    titles: Titles,
}

impl Stream {
    /// Connects to `url` and waits until a little audio is buffered. Gives up, with
    /// [`Error::Interrupted`], as soon as `interrupt` is raised. `far_only`, only addresses
    /// beyond this computer and its network are asked, and no proxy, which would resolve the
    /// name itself.
    pub fn open(
        url: &str,
        far_only: bool,
        interrupt: Interrupt,
    ) -> Result<(Self, StationInfo, Titles), Error> {
        let fail = |msg: String| Error::Open(url.into(), msg);
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(CONNECT_TIMEOUT))
            .user_agent(concat!("Freya/", env!("CARGO_PKG_VERSION")));
        let agent = if far_only {
            let config = config.proxy(None).build();
            ureq::Agent::with_parts(config, DefaultConnector::new(), Far::default())
        } else {
            config.build().into()
        };
        // Connecting can take the timeouts twice over; on its own thread, so playing something
        // else, stopping or quitting need not wait for it.
        let (sender, receiver) = crossbeam_channel::bounded(1);
        let request = agent.get(url).header("Icy-MetaData", "1");
        std::thread::Builder::new()
            .name("freya-connect".into())
            .spawn(move || {
                let _ = sender.send(request.call());
            })
            .map_err(|e| fail(e.to_string()))?;
        let response = loop {
            if interrupt.is_raised() {
                return Err(Error::Interrupted);
            }
            match receiver.recv_timeout(WAIT_STEP) {
                Ok(response) => break response,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    return Err(fail("the connection was lost".into()));
                }
            }
        };
        if interrupt.is_raised() {
            return Err(Error::Interrupted);
        }
        let response = response.map_err(|e| fail(e.to_string()))?;

        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let station = StationInfo {
            name: header("icy-name"),
            genre: header("icy-genre"),
            // Some stations send "128,128".
            bitrate_kbps: header("icy-br")
                .and_then(|b| b.split(',').next().and_then(|b| b.trim().parse().ok())),
            content_type: header("content-type"),
        };
        let interval = header("icy-metaint").and_then(|m| m.parse::<usize>().ok());
        let kbps = station.bitrate_kbps.unwrap_or(DEFAULT_KBPS).clamp(8, 2_000);
        let prebuffer = (kbps as usize * 1000 / 8) * PREBUFFER.as_secs() as usize;

        let buffer = Arc::new(Buffer::default());
        let body = response.into_body().into_reader();
        let filling = Arc::clone(&buffer);
        std::thread::Builder::new()
            .name("freya-stream".into())
            .spawn(move || receive(body, interval, &filling))
            .map_err(|e| fail(e.to_string()))?;

        let titles = Titles::default();
        let stream = Self {
            buffer,
            interrupt,
            prebuffer,
            buffering: true,
            title: None,
            titles: titles.clone(),
        };
        Ok((stream, station, titles))
    }

    /// Waits until there is audio to read, refilling the buffer first when it ran dry. Fails
    /// when interrupted or when the station stalls.
    fn wait_for_audio(&mut self) -> std::io::Result<()> {
        let started = Instant::now();
        let mut state = self.buffer.lock();
        loop {
            let enough = if self.buffering {
                state.audio_bytes >= self.prebuffer.min(MAX_BUFFERED / 2)
            } else {
                state.audio_bytes > 0
            };
            if enough || state.ended.is_some() {
                if self.buffering {
                    log::debug!("stream buffered in {:?}", started.elapsed());
                }
                self.buffering = false;
                return Ok(());
            }
            if self.interrupt.is_raised() {
                return Err(interrupted());
            }
            if started.elapsed() >= STALL_TIMEOUT {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the station stopped sending",
                ));
            }
            if !self.buffering {
                log::info!("stream ran dry; buffering");
                self.buffering = true;
            }
            state = self
                .buffer
                .filled
                .wait_timeout(state, WAIT_STEP)
                .map_or_else(|e| e.into_inner().0, |(s, _)| s);
        }
    }
}

impl Read for Stream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        self.wait_for_audio()?;
        let mut guard = self.buffer.lock();
        // Borrows the fields separately, which a guard cannot.
        let state = &mut *guard;
        let out_len = out.len().min(READ_SIZE);
        let mut n = 0;
        while n < out_len {
            match state.items.front_mut() {
                Some(Item::Audio(bytes)) => {
                    let take = bytes.len().min(out_len - n);
                    out[n..n + take].copy_from_slice(&bytes[..take]);
                    bytes.drain(..take);
                    n += take;
                    state.audio_bytes -= take;
                    if bytes.is_empty() {
                        state.items.pop_front();
                    }
                }
                // A title is taken only at the start of a read, so it lines up with the
                // audio that follows it.
                Some(Item::Title(_)) if n > 0 => break,
                Some(Item::Title(_)) => {
                    if let Some(Item::Title(title)) = state.items.pop_front()
                        && self.title.as_ref() != Some(&title)
                    {
                        self.title = Some(title.clone());
                        self.titles.put(title);
                    }
                }
                None => break,
            }
        }
        self.buffer.drained.notify_all();
        if n > 0 {
            return Ok(n);
        }
        // Only reached when the stream has ended: otherwise there was audio to read. Kept
        // for every later read too, as a decoder may read on after the end in a frame.
        match &state.ended {
            Some(Err(e)) => Err(std::io::Error::other(e.clone())),
            _ => Ok(0),
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // Tells the network thread to stop at its next read.
        self.buffer.lock().closed = true;
        self.buffer.drained.notify_all();
    }
}

/// The error a stream read returns when the decoder should stop waiting.
pub(crate) fn interrupted() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "interrupted")
}

enum Item {
    Audio(Vec<u8>),
    Title(String),
}

/// What the network thread has received and the decoder has not read yet.
#[derive(Default)]
struct Buffer {
    state: Mutex<BufferState>,
    /// Signalled when something was added.
    filled: Condvar,
    /// Signalled when something was taken, or the reader went away.
    drained: Condvar,
}

#[derive(Default)]
struct BufferState {
    items: VecDeque<Item>,
    audio_bytes: usize,
    /// Set when the stream ended: cleanly, or with an error.
    ended: Option<Result<(), String>>,
    /// Set when the reader is gone.
    closed: bool,
}

impl Buffer {
    fn lock(&self) -> MutexGuard<'_, BufferState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The network thread: reads the response body into `buffer` until it ends or nobody reads.
fn receive(mut body: impl Read, interval: Option<usize>, buffer: &Buffer) {
    let mut parser = IcyParser::new(interval);
    let mut chunk = vec![0; 16 * 1024];
    let result = loop {
        let n = match body.read(&mut chunk) {
            Ok(0) => break Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => break Err(e.to_string()),
        };
        let mut items = Vec::new();
        parser.feed(&chunk[..n], &mut items);

        let mut state = buffer.lock();
        while state.audio_bytes >= MAX_BUFFERED && !state.closed {
            state = buffer
                .drained
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
        if state.closed {
            return;
        }
        for item in items {
            if let Item::Audio(bytes) = &item {
                state.audio_bytes += bytes.len();
            }
            state.items.push_back(item);
        }
        drop(state);
        buffer.filled.notify_all();
    };
    if let Err(e) = &result {
        log::warn!("stream ended: {e}");
    }
    buffer.lock().ended = Some(result);
    buffer.filled.notify_all();
}

/// Splits a stream with ICY metadata into audio and titles. Every `interval` bytes of audio
/// the station sends one length byte, then that many times 16 bytes of text like
/// `StreamTitle='Artist - Title';`.
struct IcyParser {
    interval: Option<usize>,
    /// Audio bytes left before the next metadata block.
    audio_left: usize,
    /// The metadata block being read, and how long it will be.
    meta: Vec<u8>,
    meta_len: Option<usize>,
}

impl IcyParser {
    fn new(interval: Option<usize>) -> Self {
        let interval = interval.filter(|&i| i > 0);
        Self {
            interval,
            audio_left: interval.unwrap_or(usize::MAX),
            meta: Vec::new(),
            meta_len: None,
        }
    }

    fn feed(&mut self, mut bytes: &[u8], out: &mut Vec<Item>) {
        let Some(interval) = self.interval else {
            out.push(Item::Audio(bytes.to_vec()));
            return;
        };
        while !bytes.is_empty() {
            if self.audio_left > 0 {
                let n = self.audio_left.min(bytes.len());
                match out.last_mut() {
                    Some(Item::Audio(audio)) => audio.extend_from_slice(&bytes[..n]),
                    _ => out.push(Item::Audio(bytes[..n].to_vec())),
                }
                self.audio_left -= n;
                bytes = &bytes[n..];
                continue;
            }
            let Some(len) = self.meta_len else {
                self.meta_len = Some(usize::from(bytes[0]) * 16);
                bytes = &bytes[1..];
                continue;
            };
            let n = (len - self.meta.len()).min(bytes.len());
            self.meta.extend_from_slice(&bytes[..n]);
            bytes = &bytes[n..];
            if self.meta.len() == len {
                if let Some(title) = stream_title(&self.meta) {
                    out.push(Item::Title(title));
                }
                self.meta.clear();
                self.meta_len = None;
                self.audio_left = interval;
            }
        }
    }
}

/// The `StreamTitle` in a metadata block. Stations mostly send UTF-8, some Latin-1.
fn stream_title(block: &[u8]) -> Option<String> {
    let text = match std::str::from_utf8(block) {
        Ok(text) => text.to_owned(),
        Err(_) => block.iter().map(|&b| char::from(b)).collect(),
    };
    let start = text.find("StreamTitle='")? + "StreamTitle='".len();
    let rest = &text[start..];
    let end = rest.find("';").unwrap_or(rest.trim_end_matches('\0').len());
    Some(rest[..end].trim().to_owned())
}

/// "Artist - Title" as stations send it, split into artist and title.
pub fn split_title(text: &str) -> (Option<String>, Option<String>) {
    let text = text.trim();
    if text.is_empty() {
        return (None, None);
    }
    match text.split_once(" - ") {
        Some((artist, title)) if !artist.trim().is_empty() && !title.trim().is_empty() => (
            Some(artist.trim().to_owned()),
            Some(title.trim().to_owned()),
        ),
        _ => (None, Some(text.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(text: &str) -> Vec<u8> {
        let mut block = text.as_bytes().to_vec();
        block.resize(text.len().div_ceil(16) * 16, 0);
        let mut out = vec![(block.len() / 16) as u8];
        out.extend(block);
        out
    }

    fn collect(parser: &mut IcyParser, input: &[u8], step: usize) -> (Vec<u8>, Vec<String>) {
        let mut items = Vec::new();
        for piece in input.chunks(step) {
            parser.feed(piece, &mut items);
        }
        let mut audio = Vec::new();
        let mut titles = Vec::new();
        for item in items {
            match item {
                Item::Audio(bytes) => audio.extend(bytes),
                Item::Title(title) => titles.push(title),
            }
        }
        (audio, titles)
    }

    #[test]
    fn separates_audio_and_titles_whatever_the_chunk_size() {
        let mut input = vec![1u8; 10];
        input.extend(metadata("StreamTitle='Björk - Hyperballad';StreamUrl='';"));
        input.extend(vec![2u8; 10]);
        input.push(0); // An empty block: no new title.
        input.extend(vec![3u8; 10]);
        input.extend(metadata("StreamTitle='Ad break';"));
        input.extend(vec![4u8; 4]);
        for step in [1, 3, 7, 64] {
            let (audio, titles) = collect(&mut IcyParser::new(Some(10)), &input, step);
            let mut expected = vec![1u8; 10];
            expected.extend(vec![2u8; 10]);
            expected.extend(vec![3u8; 10]);
            expected.extend(vec![4u8; 4]);
            assert_eq!(audio, expected, "step {step}");
            assert_eq!(titles, ["Björk - Hyperballad", "Ad break"], "step {step}");
        }
    }

    #[test]
    fn streams_without_metadata_are_all_audio() {
        let (audio, titles) = collect(&mut IcyParser::new(None), &[5u8; 100], 33);
        assert_eq!(audio, vec![5u8; 100]);
        assert!(titles.is_empty());
    }

    #[test]
    fn reads_titles_in_latin1_and_with_quotes() {
        assert_eq!(
            stream_title(b"StreamTitle='Sigur R\xf3s - Hopp\xedpolla';").as_deref(),
            Some("Sigur Rós - Hoppípolla")
        );
        assert_eq!(
            stream_title(b"StreamTitle='Guns N' Roses - Don't Cry';StreamUrl='x';").as_deref(),
            Some("Guns N' Roses - Don't Cry")
        );
        assert_eq!(stream_title(b"StreamUrl='x';"), None);
    }

    #[test]
    fn splits_artist_and_title() {
        assert_eq!(
            split_title("Massive Attack - Teardrop"),
            (Some("Massive Attack".into()), Some("Teardrop".into()))
        );
        assert_eq!(
            split_title("Jay-Z - 99 Problems - Live"),
            (Some("Jay-Z".into()), Some("99 Problems - Live".into()))
        );
        assert_eq!(split_title("Station ID"), (None, Some("Station ID".into())));
        assert_eq!(split_title("  "), (None, None));
    }

    #[test]
    fn knows_urls_and_formats() {
        assert!(is_url("https://ice1.somafm.com/groovesalad-128-mp3"));
        assert!(is_url("HTTP://example.com"));
        assert!(!is_url("/music/http.mp3"));
    }

    #[test]
    fn near_addresses_are_this_computers_and_its_networks() {
        for near in [
            "127.0.0.1",
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.5",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "255.255.255.255",
            "224.0.0.1",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "fec0::1",
            "ff02::1",
            "::ffff:192.168.1.1",
            "::127.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(is_near(near.parse().unwrap()), "{near}");
        }
        for far in [
            "8.8.8.8",
            "100.128.0.1",
            "2001:db8::1",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
        ] {
            assert!(!is_near(far.parse().unwrap()), "{far}");
        }
        let station = StationInfo {
            content_type: Some("audio/aacp; charset=x".into()),
            ..Default::default()
        };
        assert_eq!(station.extension_hint(), Some("aac"));
    }
}
