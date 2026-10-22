//! A small HTTP/1.1 server, over TLS or not: each connection on a thread of its own, its
//! requests read with `httparse`, answered by a handler. Enough for Freya's clients: requests
//! with a body of a known length, answers of a known length or streamed in chunks, and
//! connections kept open between requests.
//!
//! What a client may hold is bounded, so nobody can wear the server out without logging in:
//! connections of strangers in all and from one address, how long a request's head and body
//! may take to arrive and an answer to be taken, in all and inside TLS too, how long a
//! connection may wait for its next request, and how large a body may be, decided from the
//! request's head before any of it is read, and read only as it comes. A connection that sent a
//! request of a device the server knows no longer counts among strangers', so they cannot
//! crowd it out.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The longest request line and headers taken.
const MAX_HEAD: usize = 16 * 1024;
/// The largest body taken from anyone: a picture from a camera fits. A server says what it
/// takes of each request, below this; one that says nothing takes [`DEFAULT_BODY`].
pub const MAX_BODY: usize = 64 * 1024 * 1024;
pub const DEFAULT_BODY: usize = 1024 * 1024;
/// The most headers a request may have.
const MAX_HEADERS: usize = 64;
/// How long a connection may wait for its next request, and a request's head take to arrive
/// once it began; its body may take that and a second for each [`BODY_RATE`] bytes. An answer
/// not taken in [`WRITE_TIME`] ends the connection.
#[cfg(not(test))]
const IDLE: Duration = Duration::from_secs(15);
#[cfg(test)]
const IDLE: Duration = Duration::from_secs(2);
#[cfg(not(test))]
const HEAD_TIME: Duration = Duration::from_secs(10);
#[cfg(test)]
const HEAD_TIME: Duration = Duration::from_secs(1);
const BODY_RATE: usize = 32 * 1024;
const WRITE_TIME: Duration = Duration::from_secs(30);
/// The most connections of strangers served at once, and from one address; and of all.
const MAX_CONNECTIONS: usize = 128;
const MAX_FROM_ONE: usize = 16;
const MAX_ALL: usize = 512;
/// How long a streamed answer may take to be taken: longer than the events stream lasts.
const STREAM_TIME: Duration = Duration::from_secs(90);

/// A request, as the handler sees it.
#[derive(Debug)]
pub struct Request {
    /// Who sent it, and whether over TLS.
    pub peer: Option<SocketAddr>,
    pub secure: bool,
    pub method: String,
    /// Without the query.
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// The value of the header `name`, in any case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The value of `name` in the query, as it is: Freya's clients send only plain values.
    pub fn query_value(&self, name: &str) -> Option<&str> {
        self.query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v)
    }
}

/// What writes a streamed answer, chunk by chunk, until it returns.
pub type Stream = Box<dyn FnOnce(&mut dyn Write) -> io::Result<()> + Send>;

pub enum Body {
    Bytes(Vec<u8>),
    /// `length` bytes read from a reader as they are sent, such as part of a file.
    Read(u64, Box<dyn Read + Send>),
    Stream(Stream),
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub body: Body,
}

impl Response {
    pub fn bytes(status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            headers: Vec::new(),
            body: Body::Bytes(body),
        }
    }

    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        Self::bytes(status, "application/json", value.to_string().into_bytes())
    }

    /// `message` as JSON: `{"error": message}`.
    pub fn error(status: u16, message: impl std::fmt::Display) -> Self {
        Self::json(status, &serde_json::json!({ "error": message.to_string() }))
    }

    /// `length` bytes from `reader`, read as they are sent.
    pub fn read(
        status: u16,
        content_type: &'static str,
        length: u64,
        reader: Box<dyn Read + Send>,
    ) -> Self {
        Self {
            status,
            content_type,
            headers: Vec::new(),
            body: Body::Read(length, reader),
        }
    }

    pub fn stream(content_type: &'static str, stream: Stream) -> Self {
        Self {
            status: 200,
            content_type,
            headers: Vec::new(),
            body: Body::Stream(stream),
        }
    }
}

pub type Handler = Arc<dyn Fn(Request) -> Response + Send + Sync>;

/// What a request may bring, decided from its head before any of its body is read: how large a
/// body, and whether it comes from a device the server knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Admission {
    pub body: usize,
    pub known: bool,
}

/// How a server admits requests: see [`Admission`].
pub type Admit = Arc<dyn Fn(&Request) -> Admission + Send + Sync>;

/// Serves `handler` until dropped, which ends every connection too.
pub struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    live: Arc<Live>,
}

/// The connections served now, to count them and to end them.
#[derive(Default)]
struct Live {
    connections: Mutex<HashMap<u64, Served>>,
    next: AtomicU64,
}

/// A connection served: where from, its stream to end it, and whether it is a known device's.
struct Served {
    from: IpAddr,
    stream: TcpStream,
    known: bool,
}

impl Live {
    fn connections(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Served>> {
        self.connections.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `stream` counted among those served, unless strangers' are too many, in all or from its
    /// address, or all are.
    fn admit(self: &Arc<Self>, stream: &TcpStream) -> Option<Admitted> {
        let ip = stream.peer_addr().ok()?.ip();
        let kept = stream.try_clone().ok()?;
        let mut connections = self.connections();
        let (all, from_it) = connections
            .values()
            .filter(|c| !c.known)
            .fold((0, 0), |(all, from_it), c| {
                (all + 1, from_it + usize::from(c.from == ip))
            });
        if all >= MAX_CONNECTIONS || from_it >= MAX_FROM_ONE || connections.len() >= MAX_ALL {
            return None;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        connections.insert(
            id,
            Served {
                from: ip,
                stream: kept,
                known: false,
            },
        );
        Some(Admitted {
            live: Arc::clone(self),
            id,
        })
    }

    /// Ends every connection.
    fn end_all(&self) {
        for (_, served) in self.connections().drain() {
            let _ = served.stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// A connection counted among those served, until it ends.
struct Admitted {
    live: Arc<Live>,
    id: u64,
}

impl Admitted {
    /// The connection sent a request of a device the server knows.
    fn know(&self) {
        if let Some(served) = self.live.connections().get_mut(&self.id) {
            served.known = true;
        }
    }
}

impl Drop for Admitted {
    fn drop(&mut self) {
        self.live.connections().remove(&self.id);
    }
}

impl Server {
    /// Listens on `address`; port 0 takes any free one.
    pub fn start(address: impl ToSocketAddrs, handler: Handler) -> io::Result<Self> {
        Self::serve(address, handler, None, stranger())
    }

    /// Listens on `address` over TLS as `tls` says.
    pub fn start_tls(
        address: impl ToSocketAddrs,
        handler: Handler,
        tls: Arc<rustls::ServerConfig>,
    ) -> io::Result<Self> {
        Self::serve(address, handler, Some(tls), stranger())
    }

    /// Listens on `address`, over TLS when `tls` is given, admitting requests as `admit` says.
    pub fn serve(
        address: impl ToSocketAddrs,
        handler: Handler,
        tls: Option<Arc<rustls::ServerConfig>>,
        admit: Admit,
    ) -> io::Result<Self> {
        Self::serve_on(TcpListener::bind(address)?, handler, tls, admit)
    }

    /// Serves on `listener`, bound already, as [`Self::serve`] does.
    pub fn serve_on(
        listener: TcpListener,
        handler: Handler,
        tls: Option<Arc<rustls::ServerConfig>>,
        admit: Admit,
    ) -> io::Result<Self> {
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let live = Arc::new(Live::default());
        let serving = Arc::clone(&live);
        let thread = std::thread::Builder::new()
            .name("freya-http".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    let stream = match stream {
                        Ok(stream) => stream,
                        Err(e) => {
                            // Out of files, say: waits, rather than spinning.
                            log::debug!("could not take a connection: {e}");
                            std::thread::sleep(Duration::from_millis(100));
                            continue;
                        }
                    };
                    let Some(admitted) = serving.admit(&stream) else {
                        log::debug!("too many connections: one was closed");
                        continue;
                    };
                    let handler = Arc::clone(&handler);
                    let admit = Arc::clone(&admit);
                    let tls = tls.clone();
                    let started = std::thread::Builder::new()
                        .name("freya-http-connection".into())
                        .spawn(move || {
                            let served = Connection::new(stream, tls).and_then(|connection| {
                                connection.serve(&*handler, &*admit, &admitted)
                            });
                            if let Err(e) = served {
                                log::debug!("a connection ended: {e}");
                            }
                        });
                    if let Err(e) = started {
                        log::warn!("could not answer a connection: {e}");
                    }
                }
            })?;
        Ok(Self {
            address,
            stop,
            thread: Some(thread),
            live,
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wakes the listener, which looks at `stop` before answering: through this computer
        // when it listens on every address, which Windows does not connect to.
        let mut wake = self.address;
        if wake.ip().is_unspecified() {
            wake.set_ip(match wake.ip() {
                IpAddr::V4(_) => IpAddr::from([127, 0, 0, 1]),
                IpAddr::V6(_) => IpAddr::from(std::net::Ipv6Addr::LOCALHOST),
            });
        }
        let _ = TcpStream::connect_timeout(&wake, Duration::from_secs(1));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.live.end_all();
    }
}

/// A request admitted as a stranger's, with a body of up to [`DEFAULT_BODY`].
fn stranger() -> Admit {
    Arc::new(|_| Admission {
        body: DEFAULT_BODY,
        known: false,
    })
}

/// What a connection is read from and written to: the TCP stream, or TLS over it.
trait Duplex: Read + Write + Send {}

impl<T: Read + Write + Send> Duplex for T {}

/// When what is read now, and what is written now, must be done by.
#[derive(Clone, Copy)]
struct Deadlines {
    read: Instant,
    write: Instant,
}

/// The TCP stream under a connection, each read and write of it within the deadline set for
/// what is read or written: so a client sending or taking a byte now and then, inside TLS
/// too, where one read waits for a whole record, is let go when its time is up.
struct Timed {
    tcp: Arc<TcpStream>,
    deadlines: Arc<Mutex<Deadlines>>,
}

impl Timed {
    fn left(&self, of: impl Fn(&Deadlines) -> Instant) -> io::Result<Duration> {
        let deadlines = *self.deadlines.lock().unwrap_or_else(|e| e.into_inner());
        let left = of(&deadlines).saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(left)
    }
}

impl Read for Timed {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        self.tcp.set_read_timeout(Some(self.left(|d| d.read)?))?;
        (&*self.tcp).read(into)
    }
}

impl Write for Timed {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.tcp.set_write_timeout(Some(self.left(|d| d.write)?))?;
        (&*self.tcp).write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self.tcp).flush()
    }
}

struct Connection {
    stream: Box<dyn Duplex>,
    /// The TCP stream itself, for closing it, and who is at its other end.
    tcp: Arc<TcpStream>,
    peer: Option<SocketAddr>,
    deadlines: Arc<Mutex<Deadlines>>,
    secure: bool,
    /// Read beyond the last request: the start of the next one.
    pending: Vec<u8>,
    /// Whether a request was read on it yet: the first, after TLS's handshake, is waited for
    /// as long as a request's head.
    served: bool,
}

/// Why a request could not be read: the status to answer, and why.
struct Refused(u16, &'static str);

impl Connection {
    fn new(tcp: TcpStream, tls: Option<Arc<rustls::ServerConfig>>) -> io::Result<Self> {
        let _ = tcp.set_nodelay(true);
        let peer = tcp.peer_addr().ok();
        let tcp = Arc::new(tcp);
        let now = Instant::now();
        let deadlines = Arc::new(Mutex::new(Deadlines {
            read: now + HEAD_TIME,
            write: now + WRITE_TIME,
        }));
        let timed = Timed {
            tcp: Arc::clone(&tcp),
            deadlines: Arc::clone(&deadlines),
        };
        let stream: Box<dyn Duplex> = match &tls {
            Some(config) => {
                let session =
                    rustls::ServerConnection::new(Arc::clone(config)).map_err(io::Error::other)?;
                Box::new(rustls::StreamOwned::new(session, timed))
            }
            None => Box::new(timed),
        };
        Ok(Self {
            stream,
            tcp,
            peer,
            deadlines,
            secure: tls.is_some(),
            pending: Vec::new(),
            served: false,
        })
    }

    fn read_by(&self, at: Instant) {
        self.deadlines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .read = at;
    }

    fn write_by(&self, at: Instant) {
        self.deadlines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write = at;
    }

    fn serve(
        mut self,
        handler: &(dyn Fn(Request) -> Response + Send + Sync),
        admit: &(dyn Fn(&Request) -> Admission + Send + Sync),
        admitted: &Admitted,
    ) -> io::Result<()> {
        loop {
            let (request, old) = match self.read_request(admit, admitted) {
                Ok(Some(request)) => request,
                Ok(None) => return Ok(()),
                Err(Ok(Refused(status, why))) => {
                    self.write(Response::error(status, why), false)?;
                    self.linger();
                    return Ok(());
                }
                Err(Err(e)) => return Err(e),
            };
            // HTTP/1.0 is not kept open, nor what asks to be closed.
            let close = old
                || request
                    .header("connection")
                    .is_some_and(|c| c.eq_ignore_ascii_case("close"));
            let response = handler(request);
            // A streamed answer ends with the connection.
            let keep = !close && !matches!(response.body, Body::Stream(_));
            self.write(response, keep)?;
            if !keep {
                return Ok(());
            }
        }
    }

    /// The next request, and whether it is HTTP/1.0; none when the client closed the connection
    /// between requests, or waited too long for its next one.
    fn read_request(
        &mut self,
        admit: &(dyn Fn(&Request) -> Admission + Send + Sync),
        admitted: &Admitted,
    ) -> Result<Option<(Request, bool)>, Result<Refused, io::Error>> {
        let mut buffer = std::mem::take(&mut self.pending);
        // Waiting for the next request at most `IDLE`; once it began, its head in `HEAD_TIME`;
        // the first, with TLS's handshake before it, in `HEAD_TIME` from the start.
        let mut began = (!buffer.is_empty()).then(Instant::now);
        let waited = if self.served {
            Instant::now() + IDLE
        } else {
            self.deadlines
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .read
        };
        self.served = true;
        let (head, request, old) = loop {
            let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
            let mut parsed = httparse::Request::new(&mut headers);
            match parsed.parse(&buffer) {
                Ok(httparse::Status::Complete(length)) => {
                    let old = parsed.version == Some(0);
                    let request = Request {
                        peer: self.peer,
                        secure: self.secure,
                        method: parsed.method.unwrap_or_default().to_owned(),
                        path: String::new(),
                        query: String::new(),
                        headers: parsed
                            .headers
                            .iter()
                            .map(|h| {
                                (
                                    h.name.to_owned(),
                                    String::from_utf8_lossy(h.value).into_owned(),
                                )
                            })
                            .collect(),
                        body: Vec::new(),
                    };
                    let target = parsed.path.unwrap_or("/");
                    let (path, query) = target.split_once('?').unwrap_or((target, ""));
                    break (
                        length,
                        Request {
                            path: path.to_owned(),
                            query: query.to_owned(),
                            ..request
                        },
                        old,
                    );
                }
                Ok(httparse::Status::Partial) if buffer.len() < MAX_HEAD => {}
                Ok(httparse::Status::Partial) => {
                    return Err(Ok(Refused(431, "the request's headers are too long")));
                }
                Err(_) => return Err(Ok(Refused(400, "the request could not be read"))),
            }
            self.read_by(match began {
                None => waited,
                Some(began) => began + HEAD_TIME,
            });
            let mut more = [0; 4096];
            let read = match self.stream.read(&mut more) {
                Ok(read) => read,
                // Waited too long for the next request: closed, as between requests.
                Err(e) if began.is_none() && is_timeout(&e) => return Ok(None),
                Err(e) => return Err(Err(e)),
            };
            if read == 0 {
                return if buffer.is_empty() {
                    Ok(None)
                } else {
                    Err(Err(io::ErrorKind::UnexpectedEof.into()))
                };
            }
            began.get_or_insert_with(Instant::now);
            buffer.extend_from_slice(&more[..read]);
        };
        let mut request = request;
        if request.header("transfer-encoding").is_some() {
            return Err(Ok(Refused(411, "a request's body must have a length")));
        }
        let length = content_length(&request).map_err(Ok)?;
        let admission = admit(&request);
        if admission.known {
            admitted.know();
        }
        if length > admission.body.min(MAX_BODY) {
            return Err(Ok(Refused(413, "the request is too large")));
        }
        let mut body = buffer.split_off(head);
        if body.len() > length {
            self.pending = body.split_off(length);
        }
        // Read as it comes, never set aside ahead of it, at least `BODY_RATE` bytes a second.
        self.read_by(Instant::now() + HEAD_TIME + Duration::from_secs((length / BODY_RATE) as u64));
        let mut more = [0; 16 * 1024];
        while body.len() < length {
            let want = (length - body.len()).min(more.len());
            let read = self.stream.read(&mut more[..want]).map_err(Err)?;
            if read == 0 {
                return Err(Err(io::ErrorKind::UnexpectedEof.into()));
            }
            body.extend_from_slice(&more[..read]);
        }
        request.body = body;
        Ok(Some((request, old)))
    }

    /// Closes the connection gently after a request that was not read whole: what the client
    /// still sends is read and dropped for a moment, so its answer is not lost to a reset.
    fn linger(&mut self) {
        let _ = self.stream.flush();
        let _ = self.tcp.shutdown(std::net::Shutdown::Write);
        let _ = self.tcp.set_read_timeout(Some(Duration::from_secs(1)));
        let mut dropped = [0; 4096];
        let mut left = MAX_HEAD * 4;
        while left > 0 {
            match (&*self.tcp).read(&mut dropped) {
                Ok(0) | Err(_) => break,
                Ok(read) => left = left.saturating_sub(read),
            }
        }
    }

    fn write(&mut self, response: Response, keep: bool) -> io::Result<()> {
        let mut head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nConnection: {}\r\n",
            response.status,
            reason(response.status),
            response.content_type,
            if keep { "keep-alive" } else { "close" },
        );
        for (name, value) in &response.headers {
            // A value with a line break in it would end the head early, and add headers of its
            // own: one is not written.
            if value.contains(['\r', '\n']) {
                log::warn!("a {name} header with a line break in it was not sent");
                continue;
            }
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        // Taken in `WRITE_TIME`, and a second more for each `BODY_RATE` bytes.
        let lasts = |length: u64| WRITE_TIME + Duration::from_secs(length / BODY_RATE as u64);
        self.write_by(
            Instant::now()
                + match &response.body {
                    Body::Bytes(bytes) => lasts(bytes.len() as u64),
                    Body::Read(length, _) => lasts(*length),
                    Body::Stream(_) => STREAM_TIME,
                },
        );
        match response.body {
            Body::Bytes(bytes) => {
                head.push_str(&format!("Content-Length: {}\r\n\r\n", bytes.len()));
                self.stream.write_all(head.as_bytes())?;
                self.stream.write_all(&bytes)?;
                self.stream.flush()
            }
            Body::Read(length, reader) => {
                head.push_str(&format!("Content-Length: {length}\r\n\r\n"));
                self.stream.write_all(head.as_bytes())?;
                let sent = io::copy(&mut reader.take(length), &mut self.stream)?;
                if sent < length {
                    // Shorter than said: the connection cannot be used again.
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                self.stream.flush()
            }
            Body::Stream(stream) => {
                head.push_str("Transfer-Encoding: chunked\r\n\r\n");
                self.stream.write_all(head.as_bytes())?;
                let mut chunked = Chunked(&mut *self.stream);
                stream(&mut chunked)?;
                self.stream.write_all(b"0\r\n\r\n")?;
                self.stream.flush()
            }
        }
    }
}

/// Writes what it is given as chunks of a chunked answer, each sent at once.
struct Chunked<'a>(&'a mut dyn Duplex);

impl Write for Chunked<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.0
            .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())?;
        self.0.write_all(bytes)?;
        self.0.write_all(b"\r\n")?;
        self.0.flush()?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// A request's length, as its one `Content-Length` says it in digits: 0 without one.
fn content_length(request: &Request) -> Result<usize, Refused> {
    let mut said = request
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim());
    let Some(length) = said.next() else {
        return Ok(0);
    };
    if said.next().is_some() {
        return Err(Refused(400, "a request has one length"));
    }
    if length.is_empty() || length.len() > 18 || !length.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Refused(400, "the request's length could not be read"));
    }
    length
        .parse()
        .map_err(|_| Refused(400, "the request's length could not be read"))
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        206 => "Partial Content",
        303 => "See Other",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Content Too Large",
        416 => "Range Not Satisfiable",
        408 => "Request Timeout",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo() -> Server {
        Server::start(
            "127.0.0.1:0",
            Arc::new(|request: Request| {
                let text = format!(
                    "{} {} {} {}",
                    request.method,
                    request.path,
                    request.query,
                    String::from_utf8_lossy(&request.body)
                );
                Response::bytes(200, "text/plain", text.into_bytes())
            }),
        )
        .unwrap()
    }

    fn exchange(server: &Server, raw: &[u8]) -> String {
        let mut stream = TcpStream::connect(server.address()).unwrap();
        stream.write_all(raw).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        answer
    }

    #[test]
    fn answers_requests_one_after_another_on_a_connection() {
        let server = echo();
        let answer = exchange(
            &server,
            b"POST /a?x=1 HTTP/1.1\r\nContent-Length: 5\r\n\r\nhelloGET /b HTTP/1.1\r\nConnection: close\r\n\r\n",
        );
        assert!(answer.contains("POST /a x=1 hello"), "{answer}");
        assert!(answer.contains("GET /b  "), "{answer}");
        assert_eq!(answer.matches("HTTP/1.1 200 OK").count(), 2);
    }

    #[test]
    fn refuses_what_it_cannot_take() {
        let server = echo();
        let chunked = exchange(
            &server,
            b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        );
        assert!(chunked.starts_with("HTTP/1.1 411"), "{chunked}");
        let huge = exchange(
            &server,
            format!(
                "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                MAX_BODY + 1
            )
            .as_bytes(),
        );
        assert!(huge.starts_with("HTTP/1.1 413"), "{huge}");
        let long = exchange(
            &server,
            format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(MAX_HEAD)).as_bytes(),
        );
        assert!(long.starts_with("HTTP/1.1 431"), "{long}");
        assert!(exchange(&server, b"NOT HTTP\r\n\r\n").starts_with("HTTP/1.1 400"));
        // Bodies over the default, and lengths said twice, or not in digits.
        let large = exchange(
            &server,
            format!(
                "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                DEFAULT_BODY + 1
            )
            .as_bytes(),
        );
        assert!(large.starts_with("HTTP/1.1 413"), "{large}");
        let twice = exchange(
            &server,
            b"POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab",
        );
        assert!(twice.starts_with("HTTP/1.1 400"), "{twice}");
        let signed = exchange(&server, b"POST / HTTP/1.1\r\nContent-Length: +1\r\n\r\na");
        assert!(signed.starts_with("HTTP/1.1 400"), "{signed}");
        // HTTP/1.0 is closed after its answer.
        let old = exchange(&server, b"GET / HTTP/1.0\r\n\r\n");
        assert!(old.contains("Connection: close"), "{old}");
    }

    #[test]
    fn takes_bodies_as_the_server_says() {
        let server = Server::serve(
            "127.0.0.1:0",
            Arc::new(|request: Request| {
                Response::bytes(200, "text/plain", request.body.len().to_string().into())
            }),
            None,
            Arc::new(|request: &Request| Admission {
                body: if request.path == "/large" { 100 } else { 10 },
                known: false,
            }),
        )
        .unwrap();
        let small = exchange(
            &server,
            b"POST /small HTTP/1.1\r\nContent-Length: 11\r\n\r\n",
        );
        assert!(small.starts_with("HTTP/1.1 413"), "{small}");
        let large = exchange(
            &server,
            format!(
                "POST /large HTTP/1.1\r\nContent-Length: 50\r\n\r\n{}",
                "a".repeat(50)
            )
            .as_bytes(),
        );
        assert!(large.ends_with("50"), "{large}");
    }

    #[test]
    fn a_slow_or_quiet_client_is_let_go() {
        let server = echo();
        // Its head never ends: let go after HEAD_TIME.
        let mut slow = TcpStream::connect(server.address()).unwrap();
        slow.write_all(b"GET / HTTP/1.1\r\n").unwrap();
        let started = Instant::now();
        let mut answer = String::new();
        let _ = slow.read_to_string(&mut answer);
        assert!(started.elapsed() < HEAD_TIME + Duration::from_secs(2));
        // Nothing said: let go after IDLE.
        let mut quiet = TcpStream::connect(server.address()).unwrap();
        let started = Instant::now();
        let _ = quiet.read_to_string(&mut answer);
        assert!(started.elapsed() < IDLE + Duration::from_secs(2));
    }

    #[test]
    fn connections_are_counted_and_ended_with_the_server() {
        let server = echo();
        let mut held: Vec<TcpStream> = (0..MAX_FROM_ONE)
            .map(|_| TcpStream::connect(server.address()).unwrap())
            .collect();
        // Admitted, in time.
        std::thread::sleep(Duration::from_millis(200));
        let mut over = TcpStream::connect(server.address()).unwrap();
        over.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut answer = Vec::new();
        assert_eq!(
            over.read_to_end(&mut answer).ok(),
            Some(0),
            "closed at once"
        );
        held[0]
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        drop(server);
        assert_eq!(
            held[0].read_to_end(&mut answer).ok(),
            Some(0),
            "ended with it"
        );
    }

    #[test]
    fn streams_an_answer_in_chunks() {
        let server = Server::start(
            "127.0.0.1:0",
            Arc::new(|_| {
                Response::stream(
                    "text/plain",
                    Box::new(|out| {
                        out.write_all(b"one")?;
                        out.write_all(b"two")
                    }),
                )
            }),
        )
        .unwrap();
        let answer = exchange(&server, b"GET / HTTP/1.1\r\n\r\n");
        assert!(
            answer.ends_with("3\r\none\r\n3\r\ntwo\r\n0\r\n\r\n"),
            "{answer}"
        );
    }

    #[test]
    fn a_byte_now_and_then_inside_tls_is_let_go() {
        let identity = crate::tls::Identity::make().unwrap();
        let server = Server::start_tls(
            "127.0.0.1:0",
            Arc::new(|_| Response::bytes(200, "text/plain", Vec::new())),
            identity.server_config().unwrap(),
        )
        .unwrap();
        // A TLS record that never ends, a byte at a time, well within each read's wait.
        let mut slow = TcpStream::connect(server.address()).unwrap();
        slow.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let started = Instant::now();
        let mut closed = false;
        let record = [0x16, 0x03, 0x01, 0x40, 0x00];
        for byte in record.iter().cycle().take(200) {
            if slow.write_all(&[*byte]).is_err() {
                closed = true;
                break;
            }
            let mut answer = [0; 64];
            if matches!(slow.read(&mut answer), Ok(0)) {
                closed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(closed, "still held");
        assert!(started.elapsed() < HEAD_TIME + Duration::from_secs(3));
    }

    #[test]
    fn a_known_device_is_not_crowded_out_by_strangers() {
        let live = Arc::new(Live::default());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accept = || {
            let _client = TcpStream::connect(address).unwrap();
            listener.accept().unwrap().0
        };
        let mut held: Vec<Admitted> = (0..MAX_FROM_ONE)
            .map(|_| live.admit(&accept()).unwrap())
            .collect();
        assert!(live.admit(&accept()).is_none(), "strangers are full");
        held[0].know();
        assert!(
            live.admit(&accept()).is_some(),
            "a known one does not count"
        );
        held.clear();
    }
}
