//! A file read from a server over HTTP as it is played: a megabyte at a time with Range
//! requests, the next one fetched on a thread while this one plays, so the network's delay is
//! not heard. Seeking within what was fetched asks nothing; seeking elsewhere asks from there.
//! A request that fails is made again a few times before the file is given up on.

use std::io::{self, Read, Seek, SeekFrom};
use std::time::Duration;

use symphonia::core::io::MediaSource;

use crate::source::{Bearer, RemoteSource};
use crate::stream::Interrupt;

/// How much each request asks for; little in tests, so their small files take many.
#[cfg(not(test))]
const CHUNK: u64 = 1024 * 1024;
#[cfg(test)]
const CHUNK: u64 = 4 * 1024;
/// How long one request may take, and how often one that failed is made again.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const ATTEMPTS: u32 = 4;
const FIRST_WAIT: Duration = Duration::from_millis(250);

/// A part of the file from where it was asked for, and the file's length.
type Fetched = io::Result<(Vec<u8>, u64)>;

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
    /// The bytes from `start` on, up to a chunk, and the file's length; made again while it
    /// fails, until playing something else gives up on it.
    fn fetch(&self, start: u64) -> Fetched {
        let mut wait = FIRST_WAIT;
        let mut last = None;
        for _ in 0..ATTEMPTS {
            if self.interrupt.is_raised() {
                return Err(io::Error::other("interrupted"));
            }
            match self.request(start) {
                Ok(fetched) => return Ok(fetched),
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

    fn request(&self, start: u64) -> Fetched {
        let token = self.bearer.as_ref().and_then(|bearer| bearer(None));
        let mut answer = self.ask(start, token.as_deref())?;
        // Ended or refused: asked once more with a renewed one.
        if answer.status().as_u16() == 401
            && let (Some(bearer), Some(sent)) = (&self.bearer, &token)
            && let Some(renewed) = bearer(Some(sent))
            && &renewed != sent
        {
            answer = self.ask(start, Some(&renewed))?;
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
        let length = match said {
            Some((from, length)) if from == start => length,
            Some(_) => return Err(io::Error::other("the server sent another part")),
            None => return Err(io::Error::other("the server did not say what it sent")),
        };
        let bytes = answer
            .body_mut()
            .with_config()
            // Reaching the limit counts as going over it.
            .limit(CHUNK + 1)
            .read_to_vec()
            .map_err(io::Error::other)?;
        Ok((bytes, length))
    }

    fn ask(&self, start: u64, token: Option<&str>) -> io::Result<ureq::http::Response<ureq::Body>> {
        let range = format!("bytes={start}-{}", start + CHUNK - 1);
        let mut request = self.agent.get(&self.url).header("Range", &range);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        request.call().map_err(io::Error::other)
    }
}

pub(crate) struct RemoteFile {
    fetcher: Fetcher,
    len: u64,
    pos: u64,
    /// The chunk read last: where it starts in the file, and its bytes.
    chunk: (u64, Vec<u8>),
    /// The one after it, being fetched on a thread: where it starts.
    ahead: Option<(u64, crossbeam_channel::Receiver<Fetched>)>,
}

impl RemoteFile {
    /// The file `remote`, of a server that answers Range requests; its first chunk read.
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
        let (first, len) = fetcher.fetch(0)?;
        let mut file = Self {
            fetcher,
            len,
            pos: 0,
            chunk: (0, first),
            ahead: None,
        };
        file.fetch_ahead();
        Ok(file)
    }

    /// Starts fetching the chunk after the one read, unless it is the file's end.
    fn fetch_ahead(&mut self) {
        let next = self.chunk.0 + self.chunk.1.len() as u64;
        if next >= self.len || self.chunk.1.is_empty() {
            self.ahead = None;
            return;
        }
        let (tell, fetched) = crossbeam_channel::bounded(1);
        let fetcher = self.fetcher.clone();
        let started = std::thread::Builder::new()
            .name("freya-remote-file".into())
            .spawn(move || {
                let _ = tell.send(fetcher.fetch(next));
            });
        self.ahead = started.is_ok().then_some((next, fetched));
    }

    /// Makes the chunk read the one holding `pos`.
    fn load(&mut self, pos: u64) -> io::Result<()> {
        let fetched = match self.ahead.take() {
            Some((start, fetched)) if start == pos => {
                fetched.recv().unwrap_or_else(|_| self.fetcher.fetch(pos))
            }
            _ => self.fetcher.fetch(pos),
        }?;
        self.chunk = (pos, fetched.0);
        self.fetch_ahead();
        Ok(())
    }
}

impl Read for RemoteFile {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || out.is_empty() {
            return Ok(0);
        }
        let (start, bytes) = &self.chunk;
        let within = self.pos >= *start && self.pos < start + bytes.len() as u64;
        if !within {
            self.load(self.pos)?;
        }
        let (start, bytes) = &self.chunk;
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
