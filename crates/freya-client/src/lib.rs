//! A library elsewhere, as Freya's client asks of it: over HTTP, from a Freya server.

use std::cell::Cell;
use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use freya_protocol::{
    API_PATH, AccountInfo, ApiError, ApiResult, PAGES_PATH, Remote, ServiceEvent, Transport,
};

mod session;
pub use session::{Bearer, Granted, Session, TokenStore};

/// How long the client waits before it looks for the server again, when the event stream
/// ended; and how long a stream may last, a little longer than a server keeps one, so one
/// that stopped saying anything is given up on.
const RECONNECT: Duration = Duration::from_secs(2);
const STREAM_LASTS: Duration = Duration::from_secs(70);
/// How long a server started here may take to answer.
const STARTING: Duration = Duration::from_secs(15);
/// The most the picture cache keeps; beyond it, the pictures used longest ago go.
const CACHE_LIMIT: u64 = 1024 * 1024 * 1024;

/// The thread the window draws on, which a call to the server holds up while it waits.
static WINDOW_THREAD: OnceLock<std::thread::ThreadId> = OnceLock::new();

thread_local! {
    /// Whether a call made on this thread is being timed already.
    static TIMING: Cell<bool> = const { Cell::new(false) };
}

/// Marks this thread as the one the window draws on: each call to a server made on it is
/// logged with how long it held the window up.
pub fn window_draws_on_this_thread() {
    let _ = WINDOW_THREAD.set(std::thread::current().id());
}

/// Logs, when dropped, how long a call made on the window's thread held it up.
struct HeldUp(Option<(String, Instant)>);

impl HeldUp {
    fn by(what: impl FnOnce() -> String) -> Self {
        let on_window = WINDOW_THREAD.get() == Some(&std::thread::current().id());
        let first = on_window && !TIMING.replace(true);
        Self(first.then(|| (what(), Instant::now())))
    }
}

impl Drop for HeldUp {
    fn drop(&mut self) {
        if let Some((what, started)) = self.0.take() {
            TIMING.set(false);
            log::info!(
                target: "freya_client::window",
                "{what} held the window up {:.1} ms",
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}

/// The largest answer taken: every track of a large library, as JSON. A server may be hostile,
/// so what else it sends is taken only as large as it can need: a picture, a page, a token or
/// an event.
const MAX_ANSWER: u64 = 512 * 1024 * 1024;
const MAX_PICTURE: u64 = 32 * 1024 * 1024;
const MAX_SMALL: u64 = 1024 * 1024;
const MAX_EVENT: usize = 1024 * 1024;

/// The server at `address`, such as `http://127.0.0.1:47474`: its library, and what it is
/// asked to do. Its pictures are kept in `cache`, unless it is on this computer.
pub fn connect(address: &str, token: Option<String>, cache: Option<PathBuf>) -> Arc<Remote<Http>> {
    let mut http = Http::new(address).with_token(token);
    if !is_this_computer(address) {
        http.cache = cache.map(PictureCache::new);
    }
    Arc::new(Remote(http))
}

/// Where the server at `address` serves the file of the library's track at `path`, for
/// playing it from there.
pub fn audio_url(address: &str, path: &Path) -> String {
    audio_url_at(
        &format!("{}{API_PATH}/audio", address.trim_end_matches('/')),
        path,
    )
}

/// The address of the file of the track at `path`, at `audio`, where a server serves tracks.
pub fn audio_url_at(audio: &str, path: &Path) -> String {
    let path = path.to_string_lossy();
    let mut encoded = String::with_capacity(path.len() * 3);
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("{audio}?path={encoded}")
}

/// Whether `address` is on this computer.
pub fn is_this_computer(address: &str) -> bool {
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

/// A server on another computer, as this client knows it: where it is, and the certificate it
/// pinned the first time it connected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub address: std::net::SocketAddr,
    pub certificate: Vec<u8>,
}

/// Looks every name up as the pinned server's address: a client calls the server
/// `freya-server.invalid`, the name its certificate holds, whatever its address.
#[derive(Debug)]
struct PinnedAddress(std::net::SocketAddr);

impl ureq::unversioned::resolver::Resolver for PinnedAddress {
    fn resolve(
        &self,
        _uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let mut addresses = self.empty();
        addresses.push(self.0);
        Ok(addresses)
    }
}

/// What the server's certificate names it.
const TLS_NAME: &str = "freya-server.invalid";

/// An agent as `config` says, to a pinned server when `pin` is given: only its certificate is
/// trusted, under the name it holds, at the address it was found at.
fn make_agent(
    config: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>,
    pin: Option<&Pin>,
) -> ureq::Agent {
    // Kept a little less than a server keeps a known device's, so none is used as the server
    // lets it go; enough of them for pictures fetched side by side.
    let config = config
        .max_idle_age(Duration::from_secs(100))
        .max_idle_connections_per_host(8);
    let Some(pin) = pin else {
        return config.build().into();
    };
    let certificate = ureq::tls::Certificate::from_der(&pin.certificate).to_owned();
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::Specific(Arc::new(vec![certificate])))
        .build();
    ureq::Agent::with_parts(
        config.tls_config(tls).build(),
        ureq::unversioned::transport::DefaultConnector::new(),
        PinnedAddress(pin.address),
    )
}

/// What a pinned server's token is kept by in a [`TokenStore`].
pub fn token_key(pin: &Pin) -> String {
    format!("https://{}", pin.address)
}

/// The server `pin` names, on another computer, called as the device whose refresh token
/// `store` keeps for it; its pictures and its library's large answers kept in `cache`, a
/// folder of the client's.
pub fn connect_pinned(pin: Pin, store: TokenStore, cache: Option<PathBuf>) -> Arc<Remote<Http>> {
    let key = token_key(&pin);
    let refresh = store.get(&key).unwrap_or_default();
    let server = hex(ring::digest::digest(&ring::digest::SHA256, &pin.certificate).as_ref());
    let mut http = Http::pinned(pin).with_session(refresh, None, Some((store, key)));
    if let Some(cache) = cache {
        http.cache = Some(PictureCache::new(cache.join("pictures")));
        http.kept_in(cache.join("answers").join(server));
    }
    Arc::new(Remote(http))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether the server `pin` names knows the connection code `typed`, shown on its computer:
/// that it, and not another computer in its place, gave the certificate pinned.
pub fn verify_server(pin: &Pin, typed: &str) -> Result<(), String> {
    let code = freya_protocol::normalize_code(typed)
        .ok_or("That is not a code: ten letters and digits, like K7Q2M-9XD4T.")?;
    let mut nonce = [0; 32];
    // The system's randomness failing leaves nothing safe to do.
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce)
        .expect("the system's random numbers");
    let proof = Http::pinned(pin.clone())
        .enroll(&nonce)
        .map_err(|e| e.to_string())?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &pin.certificate);
    let derived = freya_protocol::connection_key(&code, digest.as_ref());
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &derived);
    let message = freya_protocol::connection_message(&nonce, digest.as_ref());
    ring::hmac::verify(&key, &message, &proof).map_err(|_| {
        "The code does not match: it was mistyped, or another computer answered in the server's \
         place."
            .to_owned()
    })
}

/// Where a server typed in is: "mini.local", "192.168.1.20" or either with ":port", which is
/// [`freya_protocol::NETWORK_PORT`] when left out.
pub fn resolve(typed: &str) -> Result<std::net::SocketAddr, String> {
    use std::net::ToSocketAddrs;
    let typed = typed.trim();
    if typed.is_empty() {
        return Err("Type the server's name or address".into());
    }
    let with_port = match typed.rsplit_once(':') {
        Some((_, port)) if port.parse::<u16>().is_ok() && !typed.ends_with(']') => typed.to_owned(),
        _ => format!("{typed}:{}", freya_protocol::NETWORK_PORT),
    };
    let found: Vec<std::net::SocketAddr> = with_port
        .to_socket_addrs()
        .map_err(|e| format!("Could not find {typed}: {e}"))?
        .collect();
    found
        .iter()
        .find(|a| a.is_ipv4())
        .or(found.first())
        .copied()
        .ok_or_else(|| format!("Could not find {typed}"))
}

/// The certificate of the server at `address`, asked for without checking it: for the user to
/// compare its fingerprint, and to pin it.
pub fn certificate_of(address: std::net::SocketAddr) -> ApiResult<Vec<u8>> {
    let tls = ureq::tls::TlsConfig::builder()
        .disable_verification(true)
        .build();
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .tls_config(tls)
        .build();
    let agent = ureq::Agent::with_parts(
        config,
        ureq::unversioned::transport::DefaultConnector::new(),
        PinnedAddress(address),
    );
    let url = format!(
        "https://{TLS_NAME}:{}{API_PATH}/certificate",
        address.port()
    );
    let answer = agent.get(&url).call().map_err(failed)?;
    match read_at_most(answer, MAX_SMALL)? {
        (200, certificate) => Ok(certificate),
        (status, body) => Err(error(status, &body)),
    }
}

/// A server found on the network: where it answered from, and what it said of itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Where it takes clients, over TLS.
    pub address: std::net::SocketAddr,
    pub server: freya_protocol::ServerFound,
}

/// The servers on the network that answer within `wait`: asked by a broadcast to every
/// computer on it.
pub fn find_servers(wait: Duration) -> Vec<Found> {
    let everyone =
        std::net::SocketAddr::from(([255, 255, 255, 255], freya_protocol::DISCOVERY_PORT));
    ask_for_servers(everyone, wait).unwrap_or_else(|e| {
        log::warn!("could not look for servers: {e}");
        Vec::new()
    })
}

/// The servers that answer `to` within `wait`, each once.
fn ask_for_servers(to: std::net::SocketAddr, wait: Duration) -> std::io::Result<Vec<Found>> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0))?;
    socket.set_broadcast(true)?;
    socket.send_to(freya_protocol::DISCOVERY_QUESTION, to)?;
    let until = Instant::now() + wait;
    let mut found: Vec<Found> = Vec::new();
    let mut answer = [0; 2048];
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(found);
        }
        socket.set_read_timeout(Some(left))?;
        let Ok((read, from)) = socket.recv_from(&mut answer) else {
            return Ok(found);
        };
        let Ok(server) = serde_json::from_slice::<freya_protocol::ServerFound>(&answer[..read])
        else {
            continue;
        };
        let address = std::net::SocketAddr::new(from.ip(), server.port);
        if !found.iter().any(|f| f.address == address) {
            found.push(Found { address, server });
        }
    }
}

/// Calls carried over HTTP.
pub struct Http {
    agent: ureq::Agent,
    /// For the event stream, which stays open.
    streaming: ureq::Agent,
    /// The server's address, and the API's under it.
    root: String,
    base: String,
    /// The tokens this device was given, renewed as they end; or one token to call with, as
    /// in tests; none before it was given any.
    session: Option<Arc<Session>>,
    token: Option<String>,
    /// The server on another computer, over TLS; none for one on this computer.
    pin: Option<Pin>,
    /// Pictures already fetched, kept by their checksum.
    cache: Option<PictureCache>,
    /// The library's answers, kept.
    kept: Arc<Mutex<Kept>>,
}

/// The most of the library's answers kept in memory; and how large one is to be kept on disk
/// too, between runs, so a client started again is told it has it rather than sent it.
const KEPT_LIMIT: usize = 128 * 1024 * 1024;
const KEPT_ON_DISK_FROM: usize = 16 * 1024;

/// Answers to the calls that only read the library ([`freya_protocol::CACHED_CALLS`]), each
/// with the server's tag for it. One is given again without asking while the server tells
/// this client of every change and none came since it was asked; else it is asked for with
/// its tag, and the server answers with no more than that when it is the same.
#[derive(Default)]
struct Kept {
    answers: HashMap<(String, Vec<u8>), KeptAnswer>,
    bytes: usize,
    /// Whether the server tells this client of every change to the library, and the client
    /// follows them.
    told: bool,
    /// Counted at each change: an answer asked for before one is not taken as current.
    changes: u64,
    /// Where large answers are kept between runs, for this server alone.
    dir: Option<PathBuf>,
}

struct KeptAnswer {
    tag: String,
    json: Vec<u8>,
    /// Whether it is the server's answer still, as far as this client was told.
    current: bool,
}

impl Kept {
    /// The answer to `call`, when it is known to be current.
    fn current(&self, call: &(String, Vec<u8>)) -> Option<Vec<u8>> {
        let kept = self.answers.get(call)?;
        (self.told && kept.current).then(|| kept.json.clone())
    }

    /// The tag of the answer kept for `call`, from memory, or from disk into memory.
    fn tag(&mut self, call: &(String, Vec<u8>)) -> Option<String> {
        if let Some(kept) = self.answers.get(call) {
            return Some(kept.tag.clone());
        }
        let file = std::fs::read(self.file(call)?).ok()?;
        let at = file.iter().position(|&b| b == b'\n')?;
        let tag = String::from_utf8(file[..at].to_vec()).ok()?;
        let json = file[at + 1..].to_vec();
        self.hold(call.clone(), tag.clone(), json, false);
        Some(tag)
    }

    /// Keeps `json`, the answer to `call` the server tagged `tag`, asked for when the changes
    /// counted were `asked`.
    fn keep(&mut self, call: (String, Vec<u8>), tag: String, json: Vec<u8>, asked: u64) {
        if json.len() >= KEPT_ON_DISK_FROM
            && let Some(file) = self.file(&call)
        {
            let mut kept = Vec::with_capacity(tag.len() + 1 + json.len());
            kept.extend_from_slice(tag.as_bytes());
            kept.push(b'\n');
            kept.extend_from_slice(&json);
            if let Err(e) = write_private(&file, &kept) {
                log::debug!("could not keep an answer on disk: {e}");
            }
        }
        let current = self.changes == asked;
        self.hold(call, tag, json, current);
    }

    /// The answer kept for `call`, which the server said is current when asked for at
    /// `asked`.
    fn confirmed(&mut self, call: &(String, Vec<u8>), asked: u64) -> Option<Vec<u8>> {
        let current = self.changes == asked;
        let kept = self.answers.get_mut(call)?;
        kept.current |= current;
        Some(kept.json.clone())
    }

    /// Forgets every answer, on disk too.
    fn forget(&mut self) {
        self.changed();
        self.answers.clear();
        self.bytes = 0;
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// What the library holds may have changed: every answer is asked for again.
    fn changed(&mut self) {
        self.changes += 1;
        for kept in self.answers.values_mut() {
            kept.current = false;
        }
    }

    fn hold(&mut self, call: (String, Vec<u8>), tag: String, json: Vec<u8>, current: bool) {
        let size = |call: &(String, Vec<u8>), json: &[u8]| call.0.len() + call.1.len() + json.len();
        if let Some(old) = self.answers.get(&call) {
            self.bytes -= size(&call, &old.json);
        }
        if self.bytes + size(&call, &json) > KEPT_LIMIT {
            // Those that are not current go first, then all.
            self.answers.retain(|_, kept| kept.current);
            if self.bytes + size(&call, &json) > KEPT_LIMIT {
                self.answers.clear();
            }
            self.bytes = self.answers.iter().map(|(c, k)| size(c, &k.json)).sum();
        }
        self.bytes += size(&call, &json);
        self.answers.insert(call, KeptAnswer { tag, json, current });
    }

    fn file(&self, call: &(String, Vec<u8>)) -> Option<PathBuf> {
        let mut named = call.0.as_bytes().to_vec();
        named.push(0);
        named.extend_from_slice(&call.1);
        let name = hex(ring::digest::digest(&ring::digest::SHA256, &named).as_ref());
        Some(self.dir.as_ref()?.join(name))
    }
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
        // Kept as it was: one changed since is not the picture, and goes.
        if !holds(checksum, &bytes) {
            let _ = std::fs::remove_file(&file);
            return None;
        }
        // Used now: kept longer than those not.
        let _ = std::fs::File::options()
            .append(true)
            .open(&file)
            .and_then(|f| f.set_modified(std::time::SystemTime::now()));
        Some(bytes)
    }

    fn put(&self, checksum: &str, bytes: &[u8]) {
        if !holds(checksum, bytes) {
            return;
        }
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

/// Whether `bytes` are the picture `checksum` names: their SHA-256. A server elsewhere may
/// send other bytes under a common cover's name; they are not taken for it.
fn holds(checksum: &str, bytes: &[u8]) -> bool {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    hex.eq_ignore_ascii_case(checksum)
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
    /// The server at `address`, on this computer: `http://127.0.0.1:47474`.
    pub fn new(address: &str) -> Self {
        Self::with(address, None)
    }

    /// The server `pin` names, on another computer, over TLS.
    pub fn pinned(pin: Pin) -> Self {
        let base = format!("https://{TLS_NAME}:{}", pin.address.port());
        Self::with(&base, Some(pin))
    }

    fn with(address: &str, pin: Option<Pin>) -> Self {
        let agent = make_agent(
            ureq::Agent::config_builder()
                .timeout_connect(Some(Duration::from_secs(10)))
                .timeout_global(Some(Duration::from_secs(120)))
                .user_agent(concat!("Freya/", env!("CARGO_PKG_VERSION")))
                // Errors come with a body that says what went wrong.
                .http_status_as_error(false),
            pin.as_ref(),
        );
        let streaming = make_agent(
            ureq::Agent::config_builder()
                .timeout_connect(Some(Duration::from_secs(10)))
                .timeout_global(Some(STREAM_LASTS))
                .user_agent(concat!("Freya/", env!("CARGO_PKG_VERSION"))),
            pin.as_ref(),
        );
        let root = address.trim_end_matches('/').to_owned();
        Self {
            agent,
            streaming,
            base: format!("{root}{API_PATH}"),
            root,
            session: None,
            token: None,
            pin,
            cache: None,
            kept: Arc::default(),
        }
    }

    /// Keeps the library's large answers in `dir` between runs, as well as in memory.
    pub fn kept_in(&mut self, dir: PathBuf) {
        self.kept_answers().dir = Some(dir);
    }

    fn kept_answers(&self) -> std::sync::MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The address tracks are read from, `/audio`, and an agent that reaches it as this
    /// client does: for the player, which reads a track a part at a time, each part in at
    /// most `timeout`.
    pub fn audio(&self, timeout: Duration) -> (String, ureq::Agent) {
        let agent = make_agent(
            ureq::Agent::config_builder()
                .timeout_global(Some(timeout))
                .user_agent(concat!("Freya/", env!("CARGO_PKG_VERSION")))
                .http_status_as_error(false),
            self.pin.as_ref(),
        );
        (self.url("/audio"), agent)
    }

    /// The certificate pinned for the server, when it is reached over TLS.
    pub fn certificate(&self) -> Option<&[u8]> {
        self.pin.as_ref().map(|pin| pin.certificate.as_slice())
    }

    /// The server pinned, when it is reached over TLS.
    pub fn pin(&self) -> Option<&Pin> {
        self.pin.as_ref()
    }

    /// What the player reads tracks with: this device's access token, renewed as it ends.
    pub fn bearer(&self) -> Option<Bearer> {
        if let Some(session) = &self.session {
            return Some(session::bearer(session));
        }
        let token = self.token.clone()?;
        Some(Arc::new(move |_| Some(token.clone())))
    }

    /// Calls with the access token `token` only, never renewed.
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token;
        self
    }

    /// Calls as the device whose refresh token is `refresh`, with `granted`'s access token
    /// until it ends; each new refresh token kept in `store`, under its key.
    pub fn with_session(
        mut self,
        refresh: String,
        granted: Option<&Granted>,
        store: Option<(TokenStore, String)>,
    ) -> Self {
        self.session = Some(Arc::new(Session::new(
            self.agent.clone(),
            self.url("/token"),
            refresh,
            granted,
            store,
        )));
        self
    }

    /// The access token to call with now.
    fn access(&self) -> Option<String> {
        match &self.session {
            Some(session) => session.access(),
            None => self.token.clone(),
        }
    }

    fn authorized<B>(&self, request: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        match self.access() {
            Some(token) => request.header("Authorization", format!("Bearer {token}")),
            None => request,
        }
    }

    /// Sends what `send` makes, and once more with a renewed token when the server no longer
    /// took the one sent.
    fn send(&self, send: impl Fn(&Self) -> Result<Answer, ureq::Error>) -> ApiResult<Answer> {
        let _held = HeldUp::by(|| "a request".into());
        let sent = self.session.as_ref().and_then(|_| self.access());
        let answer = send(self).map_err(failed)?;
        let answer = match (&self.session, sent) {
            (Some(session), Some(sent))
                if answer.status().as_u16() == 401 && session.renew_after(&sent) =>
            {
                send(self).map_err(failed)?
            }
            _ => answer,
        };
        // Refused: what was kept is not given without asking, and is asked for again.
        if answer.status().as_u16() == 401 {
            let mut kept = self.kept_answers();
            kept.changed();
            kept.told = false;
        }
        Ok(answer)
    }

    fn get(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        self.authorized(self.agent.get(&self.url(path)))
    }

    fn post(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        self.authorized(self.agent.post(&self.url(path)))
    }

    /// POSTs `body` as JSON to `path`: what the server answered, and its status.
    fn send_json(&self, path: &str, body: &serde_json::Value) -> ApiResult<(u16, Vec<u8>)> {
        let answer = self.send(|http| {
            http.post(path)
                .header("Content-Type", "application/json")
                .send(body.to_string())
        })?;
        read_at_most(answer, MAX_SMALL)
    }

    /// Tokens for this device, named `device`, given the pairing key a client on the server's
    /// computer reads.
    pub fn pair(&self, key: &str, device: &str) -> ApiResult<Granted> {
        // The key is not sent: an HMAC with it over a nonce of this client's.
        let mut nonce = [0; 32];
        // The system's randomness failing leaves nothing safe to do.
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce)
            .expect("the system's random numbers");
        let signing = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key.trim().as_bytes());
        let proof = ring::hmac::sign(&signing, &freya_protocol::pairing_message(&nonce, device));
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let asked = serde_json::json!({
            "nonce": hex(&nonce),
            "proof": hex(proof.as_ref()),
            "device": device,
        });
        Granted::from_answer(self.send_json("/pair", &asked)?)
    }

    /// Tokens for this device, named `device`, for the code the login page sent `login`'s
    /// browser back with.
    pub fn redeem(&self, login: &Login, code: &str, device: &str) -> ApiResult<Granted> {
        let asked = serde_json::json!({
            "grant_type": "authorization_code",
            "code": code,
            "code_verifier": login.verifier,
            "redirect_uri": login.redirect,
            "device": device,
        });
        Granted::from_answer(self.send_json("/token", &asked)?)
    }

    /// The pages a browser opens on this server.
    pub fn pages(&self) -> Pages {
        Pages {
            root: self.root.clone(),
            pin: self.pin.clone(),
        }
    }

    /// The account this device is connected to; none when its token is not taken.
    pub fn account(&self) -> ApiResult<Option<AccountInfo>> {
        let answer = self.send(|http| http.get("/account").call())?;
        match read_at_most(answer, MAX_SMALL)? {
            (200, body) => serde_json::from_slice(&body).map(Some).map_err(failed),
            (401, _) => Ok(None),
            (status, body) => Err(error(status, &body)),
        }
    }

    /// The ticket that opens the page in [`Self::pages`] where the account's user changes their
    /// password, `/web/password?ticket=`, for a while: hex, as the server gives it.
    pub fn password_ticket(&self) -> ApiResult<String> {
        let answer = self.send(|http| http.post("/account/password-page").send_empty())?;
        small_json(answer)?["ticket"]
            .as_str()
            .filter(|ticket| !ticket.is_empty() && ticket.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(str::to_owned)
            .ok_or_else(|| ApiError("the server gave no ticket".into()))
    }

    /// What the server is called, where it takes clients on other computers and its
    /// certificate's fingerprint.
    pub fn info(&self) -> ApiResult<freya_protocol::ServerFound> {
        let answer = self.send(|http| http.get("/info").call())?;
        serde_json::from_value(small_json(answer)?).map_err(failed)
    }

    /// The server's proof that it knows the connection code its computer shows, for `nonce`.
    pub fn enroll(&self, nonce: &[u8]) -> ApiResult<Vec<u8>> {
        let nonce: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let (status, body) = self.send_json("/enroll", &serde_json::json!({ "nonce": nonce }))?;
        if status != 200 {
            return Err(error(status, &body));
        }
        let answer: serde_json::Value = serde_json::from_slice(&body).map_err(failed)?;
        let proof = answer["proof"].as_str().unwrap_or_default();
        (0..proof.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(proof.get(i..i + 2)?, 16).ok())
            .collect::<Option<Vec<u8>>>()
            .ok_or_else(|| ApiError("the server gave no proof".into()))
    }

    /// A new connection code for a device to type, shown on this server's computer, and how
    /// long it lasts; only for an admin.
    pub fn connection_code(&self) -> ApiResult<(String, Duration)> {
        let answer = self.send(|http| http.post("/account/connection-code").send_empty())?;
        let answer = small_json(answer)?;
        let code = answer["code"]
            .as_str()
            .ok_or_else(|| ApiError("the server gave no code".into()))?;
        let lasts = Duration::from_secs(answer["lasts"].as_u64().unwrap_or(0));
        Ok((code.to_owned(), lasts))
    }

    /// Takes this device's own token back: it has to log in again.
    pub fn log_out(&self) -> ApiResult<()> {
        let answer = self.send(|http| http.post("/account/log-out").send_empty())?;
        small_json(answer)?;
        // A device logged out keeps nothing of the library.
        self.kept_answers().forget();
        Ok(())
    }

    /// Takes the token of the account's device `device` back.
    pub fn revoke(&self, device: &str) -> ApiResult<()> {
        match self.send_json("/account/revoke", &serde_json::json!({ "device": device }))? {
            (200, _) => Ok(()),
            (status, body) => Err(error(status, &body)),
        }
    }

    /// The server's version; an error when none answers.
    pub fn version(&self) -> ApiResult<String> {
        let answer = self.get("/version").call().map_err(failed)?;
        let version = small_json(answer)?;
        Ok(version["version"].as_str().unwrap_or_default().to_owned())
    }

    /// Asks the server to stop: one on this computer only.
    pub fn quit(&self) -> ApiResult<()> {
        let answer = self.send(|http| http.post("/quit").send_empty())?;
        small_json(answer).map(drop)
    }

    /// The event stream, opened.
    fn open_events(&self) -> ApiResult<ureq::BodyReader<'static>> {
        let answer = self.send(|http| {
            http.authorized(http.streaming.get(&http.url("/events")))
                .call()
        })?;
        if answer.status().as_u16() != 200 {
            let (status, body) = read_at_most(answer, MAX_SMALL)?;
            return Err(error(status, &body));
        }
        Ok(answer.into_body().into_reader())
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

/// Logging in in a browser, as OAuth's authorization code with PKCE has it: the page to open,
/// which sends the browser on to `redirect` with a code and the state, and the secret only
/// this client knows, which the code is traded for a token with.
pub struct Login {
    pub redirect: String,
    pub state: String,
    verifier: String,
}

impl Login {
    /// A login that sends the browser back to `redirect`, `http://127.0.0.1:port/...`.
    pub fn new(redirect: &str) -> Self {
        let random = || {
            let mut bytes = [0; 32];
            // The system's randomness failing leaves nothing safe to do.
            ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
                .expect("the system's random numbers");
            URL_SAFE_NO_PAD.encode(bytes)
        };
        Self {
            redirect: redirect.to_owned(),
            state: random(),
            verifier: random(),
        }
    }

    /// The login page, under a server's address.
    pub fn page(&self) -> String {
        let challenge = URL_SAFE_NO_PAD.encode(ring::digest::digest(
            &ring::digest::SHA256,
            self.verifier.as_bytes(),
        ));
        format!(
            "{PAGES_PATH}/login?response_type=code&client_id=freya&redirect_uri={}&state={}&code_challenge={challenge}&code_challenge_method=S256",
            query_escape(&self.redirect),
            self.state,
        )
    }
}

/// The pages a browser opens on a server, reached as its client reaches it: for the client to
/// carry a browser's requests to them.
#[derive(Clone, Debug)]
pub struct Pages {
    root: String,
    pin: Option<Pin>,
}

/// A page as the server answered it.
pub struct Page {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Pages {
    /// The pages of the server `pin` names.
    pub fn of(pin: Pin) -> Self {
        Http::pinned(pin).pages()
    }

    /// Asks for `path`, with its query, by `method`, "GET" or "POST" with `body` of `kind`:
    /// what the server answered, a redirect as it is.
    pub fn ask(
        &self,
        method: &str,
        path: &str,
        kind: Option<&str>,
        body: Vec<u8>,
    ) -> ApiResult<Page> {
        if !path.starts_with(&format!("{PAGES_PATH}/")) {
            return Err(ApiError("not a page".into()));
        }
        let agent = make_agent(
            ureq::Agent::config_builder()
                .timeout_connect(Some(Duration::from_secs(10)))
                .timeout_global(Some(Duration::from_secs(60)))
                .user_agent(concat!("Freya/", env!("CARGO_PKG_VERSION")))
                .http_status_as_error(false)
                .max_redirects(0),
            self.pin.as_ref(),
        );
        let url = format!("{}{path}", self.root);
        let answer = match method {
            "GET" => agent.get(&url).call(),
            "POST" => {
                let request = agent.post(&url);
                match kind {
                    Some(kind) => request.header("Content-Type", kind).send(body),
                    None => request.send(body),
                }
            }
            _ => return Err(ApiError("pages are only got and posted".into())),
        }
        .map_err(failed)?;
        let headers = answer
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
            })
            .collect();
        let (status, body) = read_at_most(answer, MAX_SMALL)?;
        Ok(Page {
            status,
            headers,
            body,
        })
    }
}

/// `text` in a URL's query, all but its unreserved characters escaped.
fn query_escape(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn failed(e: impl std::fmt::Display) -> ApiError {
    ApiError(format!("could not reach the server: {e}"))
}

type Answer = ureq::http::Response<ureq::Body>;

/// The body of `answer`, and its status, when it is at most `limit` bytes.
pub(crate) fn read_at_most(mut answer: Answer, limit: u64) -> ApiResult<(u16, Vec<u8>)> {
    let status = answer.status().as_u16();
    let body = answer
        .body_mut()
        .with_config()
        .limit(limit)
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

/// The bytes of a picture in `answer`, none when the server has none (404).
fn bytes_or_none(answer: Answer) -> ApiResult<Option<Vec<u8>>> {
    match read_at_most(answer, MAX_PICTURE)? {
        (200, body) => Ok(Some(body)),
        (404, _) => Ok(None),
        (status, body) => Err(error(status, &body)),
    }
}

/// The JSON of an answer that is small: about the server, the account, or what it did.
fn small_json(answer: Answer) -> ApiResult<serde_json::Value> {
    json_at_most(answer, MAX_SMALL)
}

fn json_at_most(answer: Answer, limit: u64) -> ApiResult<serde_json::Value> {
    match read_at_most(answer, limit)? {
        (200, body) => serde_json::from_slice(&body).map_err(failed),
        (status, body) => Err(error(status, &body)),
    }
}

impl Transport for Http {
    fn call(&self, method: &str, args: Vec<u8>) -> ApiResult<Vec<u8>> {
        let _held = HeldUp::by(|| method.to_owned());
        let send = |tag: Option<&str>| {
            self.send(|http| {
                let mut request = http
                    .post(&format!("/call/{method}"))
                    .header("Content-Type", "application/json");
                if let Some(tag) = tag {
                    request = request.header("If-None-Match", tag);
                }
                request.send(&args[..])
            })
        };
        if !freya_protocol::CACHED_CALLS.contains(&method) {
            let answered = send(None).and_then(|answer| read_at_most(answer, MAX_ANSWER));
            // Whatever it did, it may have changed the library.
            self.kept_answers().changed();
            return match answered? {
                (200, body) => Ok(body),
                (status, body) => Err(error(status, &body)),
            };
        }
        let call = (method.to_owned(), args.clone());
        let (asked, tag) = {
            let mut kept = self.kept_answers();
            if let Some(json) = kept.current(&call) {
                return Ok(json);
            }
            (kept.changes, kept.tag(&call))
        };
        let answer = send(tag.as_deref())?;
        let tagged = answer
            .headers()
            .get("etag")
            .and_then(|tag| tag.to_str().ok())
            .map(str::to_owned);
        match read_at_most(answer, MAX_ANSWER)? {
            (304, _) => self.kept_answers().confirmed(&call, asked).ok_or_else(|| {
                ApiError("the server said this client has an answer it has not".into())
            }),
            (200, body) => {
                if let Some(tag) = tagged {
                    self.kept_answers().keep(call, tag, body.clone(), asked);
                }
                Ok(body)
            }
            (status, body) => Err(error(status, &body)),
        }
    }

    fn picture(&self, checksum: &str) -> ApiResult<Option<Vec<u8>>> {
        let _held = HeldUp::by(|| "a picture".into());
        if !freya_protocol::is_checksum(checksum) {
            return Ok(None);
        }
        if let Some(bytes) = self.cache.as_ref().and_then(|cache| cache.get(checksum)) {
            return Ok(Some(bytes));
        }
        let answer = self.send(|http| http.get(&format!("/pictures/{checksum}")).call())?;
        let bytes = bytes_or_none(answer)?;
        if bytes.as_ref().is_some_and(|bytes| !holds(checksum, bytes)) {
            return Err(ApiError(format!(
                "the server sent another picture than {checksum}"
            )));
        }
        if let (Some(cache), Some(bytes)) = (&self.cache, &bytes) {
            cache.put(checksum, bytes);
        }
        Ok(bytes)
    }

    fn keep_picture(&self, bytes: &[u8], largest: Option<u32>) -> ApiResult<String> {
        let answer = self.send(|http| {
            let mut request = http.post("/pictures");
            if let Some(largest) = largest {
                request = request.query("largest", largest.to_string());
            }
            request
                .header("Content-Type", "application/octet-stream")
                .send(bytes)
        })?;
        serde_json::from_value(small_json(answer)?).map_err(failed)
    }

    fn album_art(&self, track: &Path, largest: u32) -> ApiResult<Option<Vec<u8>>> {
        let _held = HeldUp::by(|| "album art".into());
        let asked = serde_json::json!({ "track": track, "largest": largest });
        let answer = self.send(|http| {
            http.post("/album-art")
                .header("Content-Type", "application/json")
                .send(asked.to_string())
        })?;
        bytes_or_none(answer)
    }

    fn events(&self) -> ApiResult<Receiver<ServiceEvent>> {
        let (tell, events) = channel();
        // Opened now, so a server that is not there is told now.
        let first = self.open_events()?;
        let http = Http {
            agent: self.agent.clone(),
            streaming: self.streaming.clone(),
            root: self.root.clone(),
            base: self.base.clone(),
            session: self.session.clone(),
            token: self.token.clone(),
            pin: self.pin.clone(),
            cache: None,
            kept: Arc::clone(&self.kept),
        };
        std::thread::Builder::new()
            .name("freya-server-events".into())
            .spawn(move || follow_events(&http, first, &tell))
            .map_err(failed)?;
        Ok(events)
    }
}

/// Tells `tell` of the events read from `stream`, and opens it again when it ends, until
/// nobody listens; [`ServiceEvent::Lost`] while it cannot be.
fn follow_events(http: &Http, mut stream: ureq::BodyReader<'static>, tell: &Sender<ServiceEvent>) {
    loop {
        let mut reader = std::io::BufReader::new(stream);
        let mut line = Vec::new();
        loop {
            line.clear();
            // A line is at most `MAX_EVENT` bytes: one longer ends the stream.
            let read =
                std::io::Read::take(&mut reader, MAX_EVENT as u64 + 1).read_until(b'\n', &mut line);
            match read {
                Ok(0) | Err(_) => break,
                Ok(_) if line.len() > MAX_EVENT => {
                    log::warn!("the server sent an event too long to read");
                    break;
                }
                Ok(_) => {}
            }
            let Ok(line) = std::str::from_utf8(&line) else {
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(event) => {
                    match &event {
                        ServiceEvent::Changed => http.kept_answers().changed(),
                        ServiceEvent::Connected { changes, .. } => {
                            let mut kept = http.kept_answers();
                            kept.changed();
                            kept.told = *changes;
                        }
                        _ => {}
                    }
                    if tell.send(event).is_err() {
                        return;
                    }
                }
                Err(e) => log::debug!("an event that could not be read: {e}"),
            }
        }
        // Not told of changes while the stream is closed.
        http.kept_answers().told = false;
        // A stream the server ended, as it does now and then, is opened again at once.
        if let Ok(again) = http.open_events() {
            stream = again;
            continue;
        }
        log::info!("lost the server's events; looking for it again");
        if tell.send(ServiceEvent::Lost).is_err() {
            return;
        }
        stream = loop {
            std::thread::sleep(RECONNECT);
            if let Ok(stream) = http.open_events() {
                break stream;
            }
        };
    }
}

/// Freya's server on this computer at `address`, connected to as this device, named
/// `device`, over TLS with the certificate the server keeps in `certificate`, a file of the
/// user's own: so a program of another user listening there in the server's place is found
/// out, and given nothing. The server is started from `program` when nothing answers there,
/// and one of another version stopped first, so an update does not leave the old one running.
/// A client without a refresh token for it, or with one it no longer takes, pairs with the key
/// the server keeps in `pairing_key`; the refresh token is kept in `store`.
pub fn connect_locally(
    address: std::net::SocketAddr,
    program: &Path,
    pairing_key: &Path,
    certificate: &Path,
    store: &TokenStore,
    device: &str,
) -> Result<Arc<Remote<Http>>, String> {
    // Kept by the address it had before it was reached over TLS, so the token stays.
    let key = format!("http://{address}");
    let pinned = || -> Result<Pin, String> {
        let certificate = std::fs::read(certificate).map_err(|_| foreign(address))?;
        Ok(Pin {
            address,
            certificate,
        })
    };
    let pair = |http: &Http| -> Result<Granted, String> {
        let key_now = std::fs::read_to_string(pairing_key)
            .map_err(|e| format!("could not read the pairing key: {e}"))?;
        let granted = http
            .pair(&key_now, device)
            .map_err(|e| format!("could not pair with the server: {e}"))?;
        store.set(&key, &granted.refresh);
        Ok(granted)
    };
    let kept = || Some((store.clone(), key.clone()));
    let as_kept = |pin: &Pin| {
        let refresh = store.get(&key).unwrap_or_default();
        Http::pinned(pin.clone()).with_session(refresh, None, kept())
    };
    if !answers(address) {
        start(address, program, certificate)?;
    }
    // What answers there without this user's certificate is someone else's, given nothing.
    let mut pin = pinned()?;
    if Http::pinned(pin.clone()).version().is_err() {
        return Err(foreign(address));
    }
    let mut paired = None;
    let version = Http::pinned(pin.clone())
        .version()
        .map_err(|_| foreign(address))?;
    if version != env!("CARGO_PKG_VERSION") {
        log::info!("the server here is version {version}; starting this version's");
        if as_kept(&pin).quit().is_err() {
            // It does not take this client's token: paired anew to stop it.
            let granted = pair(&Http::pinned(pin.clone()))?;
            Http::pinned(pin.clone())
                .with_session(granted.refresh.clone(), Some(&granted), kept())
                .quit()
                .map_err(|e| format!("could not stop the server: {e}"))?;
            paired = Some(granted);
        }
        wait_until_gone(address)?;
        start(address, program, certificate)?;
        pin = pinned()?;
    }
    let mut http = match &paired {
        Some(granted) => {
            Http::pinned(pin.clone()).with_session(granted.refresh.clone(), Some(granted), kept())
        }
        None => as_kept(&pin),
    };
    if http.account().map_err(|e| e.to_string())?.is_none() {
        let granted = pair(&Http::pinned(pin.clone()))?;
        http = Http::pinned(pin).with_session(granted.refresh.clone(), Some(&granted), kept());
    }
    Ok(Arc::new(Remote(http)))
}

/// What is said of a program answering at `address` that is not this user's server.
fn foreign(address: std::net::SocketAddr) -> String {
    format!(
        "Another program answers at {address} in the place of Freya's server, perhaps another \
         user's; Freya gives it nothing. Quit it, or log the other user out, and open Freya again."
    )
}

/// Whether anything takes connections at `address`.
fn answers(address: std::net::SocketAddr) -> bool {
    std::net::TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_ok()
}

/// Waits until nothing answers at `address`.
fn wait_until_gone(address: std::net::SocketAddr) -> Result<(), String> {
    let gone = Instant::now() + STARTING;
    while answers(address) {
        if Instant::now() > gone {
            return Err("the server did not stop".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// Starts the server `program`, and waits until it answers at `address` with the certificate
/// it keeps in `certificate`.
fn start(address: std::net::SocketAddr, program: &Path, certificate: &Path) -> Result<(), String> {
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
        if let Ok(certificate) = std::fs::read(certificate) {
            let pin = Pin {
                address,
                certificate,
            };
            if Http::pinned(pin).version().is_ok() {
                return Ok(());
            }
        }
        if Instant::now() > deadline {
            return Err(if answers(address) {
                foreign(address)
            } else {
                format!("{} did not answer", program.display())
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Writes `bytes` to `file` through a new file renamed over it, readable and writable by the
/// user only.
pub(crate) fn write_private(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = file.with_extension("partial");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut out = options.open(&partial)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        out.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    out.write_all(bytes)?;
    std::fs::rename(&partial, file)
}

#[cfg(test)]
mod tests;
