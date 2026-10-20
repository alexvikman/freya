//! A library elsewhere, as Freya's client asks of it: over HTTP, from a Freya server.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use freya_protocol::{API_PATH, ApiError, ApiResult, Remote, ServiceEvent, Transport};

/// How long the client waits before it looks for the server again, when the event stream
/// ended.
const RECONNECT: Duration = Duration::from_secs(2);
/// How long a server started here may take to answer.
const STARTING: Duration = Duration::from_secs(15);
/// The most the picture cache keeps; beyond it, the pictures used longest ago go.
const CACHE_LIMIT: u64 = 1024 * 1024 * 1024;

/// The largest answer taken: every track of a large library, as JSON.
const MAX_ANSWER: u64 = 512 * 1024 * 1024;

/// The server at `address`, such as `http://127.0.0.1:47474`: its library, and what it is
/// asked to do. Its pictures are kept in `cache`, unless it is on this computer.
pub fn connect(address: &str, cache: Option<PathBuf>) -> Arc<Remote<Http>> {
    let mut http = Http::new(address);
    if !is_this_computer(address) {
        http.cache = cache.map(PictureCache::new);
    }
    Arc::new(Remote(http))
}

/// Whether `address` is on this computer.
fn is_this_computer(address: &str) -> bool {
    let host = address
        .split("://")
        .last()
        .unwrap_or(address)
        .split('/')
        .next()
        .unwrap_or_default();
    let name = match host.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    };
    ["127.0.0.1", "localhost", "[::1]"].contains(&name)
}

/// Calls carried over HTTP.
pub struct Http {
    agent: ureq::Agent,
    /// For the event stream, which stays open.
    streaming: ureq::Agent,
    base: String,
    /// Pictures already fetched, kept by their checksum.
    cache: Option<PictureCache>,
}

/// Pictures kept on this computer by their checksum, which names one picture forever: what is
/// kept is never out of date.
pub struct PictureCache {
    dir: PathBuf,
}

impl PictureCache {
    /// Kept in `dir`, with those used longest ago taken away once it holds more than
    /// [`CACHE_LIMIT`].
    pub fn new(dir: PathBuf) -> Self {
        let tidied = dir.clone();
        let _ = std::thread::Builder::new()
            .name("freya-picture-cache".into())
            .spawn(move || tidy(&tidied, CACHE_LIMIT));
        Self { dir }
    }

    fn file(&self, checksum: &str) -> PathBuf {
        self.dir.join(checksum)
    }

    fn get(&self, checksum: &str) -> Option<Vec<u8>> {
        let file = self.file(checksum);
        let bytes = std::fs::read(&file).ok()?;
        // Used now: kept longer than those not.
        let _ = std::fs::File::options()
            .append(true)
            .open(&file)
            .and_then(|f| f.set_modified(std::time::SystemTime::now()));
        Some(bytes)
    }

    fn put(&self, checksum: &str, bytes: &[u8]) {
        let file = self.file(checksum);
        let partial = file.with_extension("partial");
        let written = std::fs::create_dir_all(&self.dir)
            .and_then(|()| std::fs::write(&partial, bytes))
            .and_then(|()| std::fs::rename(&partial, &file));
        if let Err(e) = written {
            log::debug!("could not keep a picture in the cache: {e}");
        }
    }
}

/// Takes away the pictures in `dir` used longest ago, until it holds at most `limit` bytes.
fn tidy(dir: &Path, limit: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            Some((metadata.modified().ok()?, metadata.len(), entry.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    files.sort();
    for (_, size, file) in files {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(&file).is_ok() {
            total -= size;
        }
    }
}

impl Http {
    pub fn new(address: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(120)))
            .user_agent(concat!("Freya/", env!("CARGO_PKG_VERSION")))
            // Errors come with a body that says what went wrong.
            .http_status_as_error(false)
            .build()
            .into();
        let streaming = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .user_agent(concat!("Freya/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self {
            agent,
            streaming,
            base: format!("{}{API_PATH}", address.trim_end_matches('/')),
            cache: None,
        }
    }

    /// The server's version; an error when none answers.
    pub fn version(&self) -> ApiResult<String> {
        let answer = self
            .agent
            .get(&self.url("/version"))
            .call()
            .map_err(failed)?;
        let version = json(answer)?;
        Ok(version["version"].as_str().unwrap_or_default().to_owned())
    }

    /// Asks the server to stop: one on this computer only.
    pub fn quit(&self) -> ApiResult<()> {
        let answer = self
            .agent
            .post(&self.url("/quit"))
            .send_empty()
            .map_err(failed)?;
        json(answer).map(drop)
    }

    /// The event stream, opened.
    fn open_events(&self) -> ApiResult<ureq::BodyReader<'static>> {
        let answer = self
            .streaming
            .get(&self.url("/events"))
            .call()
            .map_err(failed)?;
        if answer.status().as_u16() != 200 {
            let (status, body) = read(answer)?;
            return Err(error(status, &body));
        }
        Ok(answer.into_body().into_reader())
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

fn failed(e: impl std::fmt::Display) -> ApiError {
    ApiError(format!("could not reach the server: {e}"))
}

type Answer = ureq::http::Response<ureq::Body>;

/// The body of `answer`; what went wrong when it is not one of `wanted`'s statuses.
fn read(mut answer: Answer) -> ApiResult<(u16, Vec<u8>)> {
    let status = answer.status().as_u16();
    let body = answer
        .body_mut()
        .with_config()
        .limit(MAX_ANSWER)
        .read_to_vec()
        .map_err(failed)?;
    Ok((status, body))
}

/// What the server said went wrong in `body`, answered with `status`.
fn error(status: u16, body: &[u8]) -> ApiError {
    let said = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(str::to_owned));
    ApiError(said.unwrap_or_else(|| format!("the server answered {status}")))
}

/// The bytes of `answer`, none when the server has none (404).
fn bytes_or_none(answer: Answer) -> ApiResult<Option<Vec<u8>>> {
    match read(answer)? {
        (200, body) => Ok(Some(body)),
        (404, _) => Ok(None),
        (status, body) => Err(error(status, &body)),
    }
}

/// The JSON of `answer`.
fn json(answer: Answer) -> ApiResult<serde_json::Value> {
    match read(answer)? {
        (200, body) => serde_json::from_slice(&body).map_err(failed),
        (status, body) => Err(error(status, &body)),
    }
}

impl Transport for Http {
    fn call(&self, method: &str, args: serde_json::Value) -> ApiResult<serde_json::Value> {
        let answer = self
            .agent
            .post(&self.url(&format!("/call/{method}")))
            .header("Content-Type", "application/json")
            .send(args.to_string())
            .map_err(failed)?;
        json(answer)
    }

    fn picture(&self, checksum: &str) -> ApiResult<Option<Vec<u8>>> {
        if !freya_protocol::is_checksum(checksum) {
            return Ok(None);
        }
        if let Some(bytes) = self.cache.as_ref().and_then(|cache| cache.get(checksum)) {
            return Ok(Some(bytes));
        }
        let answer = self
            .agent
            .get(&self.url(&format!("/pictures/{checksum}")))
            .call()
            .map_err(failed)?;
        let bytes = bytes_or_none(answer)?;
        if let (Some(cache), Some(bytes)) = (&self.cache, &bytes) {
            cache.put(checksum, bytes);
        }
        Ok(bytes)
    }

    fn keep_picture(&self, bytes: &[u8], largest: Option<u32>) -> ApiResult<String> {
        let mut request = self.agent.post(&self.url("/pictures"));
        if let Some(largest) = largest {
            request = request.query("largest", largest.to_string());
        }
        let answer = request
            .header("Content-Type", "application/octet-stream")
            .send(bytes)
            .map_err(failed)?;
        serde_json::from_value(json(answer)?).map_err(failed)
    }

    fn album_art(&self, track: &Path, largest: u32) -> ApiResult<Option<Vec<u8>>> {
        let asked = serde_json::json!({ "track": track, "largest": largest });
        let answer = self
            .agent
            .post(&self.url("/album-art"))
            .header("Content-Type", "application/json")
            .send(asked.to_string())
            .map_err(failed)?;
        bytes_or_none(answer)
    }

    fn events(&self) -> ApiResult<Receiver<ServiceEvent>> {
        let (tell, events) = channel();
        // Opened now, so a server that is not there is told now.
        let first = self.open_events()?;
        let http = Http {
            agent: self.agent.clone(),
            streaming: self.streaming.clone(),
            base: self.base.clone(),
            cache: None,
        };
        std::thread::Builder::new()
            .name("freya-server-events".into())
            .spawn(move || follow_events(&http, first, &tell))
            .map_err(failed)?;
        Ok(events)
    }
}

/// Tells `tell` of the events read from `stream`, and opens it again when it ends, until
/// nobody listens.
fn follow_events(http: &Http, mut stream: ureq::BodyReader<'static>, tell: &Sender<ServiceEvent>) {
    loop {
        for line in std::io::BufReader::new(stream).lines() {
            let Ok(line) = line else {
                break;
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(&line) {
                Ok(event) => {
                    if tell.send(event).is_err() {
                        return;
                    }
                }
                Err(e) => log::debug!("an event that could not be read: {e}"),
            }
        }
        log::info!("lost the server's events; looking for it again");
        stream = loop {
            std::thread::sleep(RECONNECT);
            if let Ok(stream) = http.open_events() {
                break stream;
            }
        };
    }
}

/// Makes sure Freya's server answers at `address` on this computer, as the same version as
/// this client: starts `program` when none does, and an older or newer one is stopped first.
pub fn start_local_server(address: &str, program: &Path) -> Result<(), String> {
    let http = Http::new(address);
    match http.version() {
        Ok(version) if version == env!("CARGO_PKG_VERSION") => return Ok(()),
        Ok(version) => {
            log::info!("the server here is version {version}; starting this version's");
            http.quit()
                .map_err(|e| format!("could not stop the server: {e}"))?;
            let gone = Instant::now() + STARTING;
            while http.version().is_ok() {
                if Instant::now() > gone {
                    return Err("the server did not stop".into());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Err(_) => {}
    }
    log::info!("starting {}", program.display());
    let mut command = std::process::Command::new(program);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // A console program, started without a console window of its own.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", program.display()))?;
    let deadline = Instant::now() + STARTING;
    loop {
        if http.version().is_ok() {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("{} did not answer", program.display()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests;
