//! A small HTTP/1.1 server: each connection on a thread of its own, its requests read with
//! `httparse`, answered by a handler. Enough for Freya's clients: requests with a body of a
//! known length, answers of a known length or streamed in chunks, and connections kept open
//! between requests.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// The longest request line and headers taken.
const MAX_HEAD: usize = 16 * 1024;
/// The largest body taken: a picture from a camera fits.
const MAX_BODY: usize = 64 * 1024 * 1024;
/// The most headers a request may have.
const MAX_HEADERS: usize = 64;
/// How long a connection may say nothing before it is closed.
const IDLE: Duration = Duration::from_secs(120);

/// A request, as the handler sees it.
#[derive(Debug)]
pub struct Request {
    /// Who sent it.
    pub peer: Option<SocketAddr>,
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

/// Serves `handler` until dropped.
pub struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Listens on `address`; port 0 takes any free one.
    pub fn start(address: impl ToSocketAddrs, handler: Handler) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("freya-http".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(stream) = stream else {
                        continue;
                    };
                    let handler = Arc::clone(&handler);
                    let started = std::thread::Builder::new()
                        .name("freya-http-connection".into())
                        .spawn(move || {
                            if let Err(e) = Connection::new(stream).serve(&*handler) {
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
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wakes the listener, which looks at `stop` before answering.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_secs(1));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Connection {
    stream: TcpStream,
    /// Read beyond the last request: the start of the next one.
    pending: Vec<u8>,
}

/// Why a request could not be read: the status to answer, and why.
struct Refused(u16, &'static str);

impl Connection {
    fn new(stream: TcpStream) -> Self {
        let _ = stream.set_read_timeout(Some(IDLE));
        let _ = stream.set_nodelay(true);
        Self {
            stream,
            pending: Vec::new(),
        }
    }

    fn serve(mut self, handler: &(dyn Fn(Request) -> Response + Send + Sync)) -> io::Result<()> {
        loop {
            let request = match self.read_request() {
                Ok(Some(request)) => request,
                Ok(None) => return Ok(()),
                Err(Ok(Refused(status, why))) => {
                    self.write(Response::error(status, why), false)?;
                    self.linger();
                    return Ok(());
                }
                Err(Err(e)) => return Err(e),
            };
            let close = request
                .header("connection")
                .is_some_and(|c| c.eq_ignore_ascii_case("close"));
            let response = handler(request);
            // A streamed answer ends with the connection.
            let keep = !close && matches!(response.body, Body::Bytes(_));
            self.write(response, keep)?;
            if !keep {
                return Ok(());
            }
        }
    }

    /// The next request; none when the client closed the connection between requests.
    fn read_request(&mut self) -> Result<Option<Request>, Result<Refused, io::Error>> {
        let mut buffer = std::mem::take(&mut self.pending);
        let (head, request) = loop {
            let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
            let mut parsed = httparse::Request::new(&mut headers);
            match parsed.parse(&buffer) {
                Ok(httparse::Status::Complete(length)) => {
                    let request = Request {
                        peer: self.stream.peer_addr().ok(),
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
                    );
                }
                Ok(httparse::Status::Partial) if buffer.len() < MAX_HEAD => {}
                Ok(httparse::Status::Partial) => {
                    return Err(Ok(Refused(431, "the request's headers are too long")));
                }
                Err(_) => return Err(Ok(Refused(400, "the request could not be read"))),
            }
            let mut more = [0; 4096];
            let read = self.stream.read(&mut more).map_err(Err)?;
            if read == 0 {
                return if buffer.is_empty() {
                    Ok(None)
                } else {
                    Err(Err(io::ErrorKind::UnexpectedEof.into()))
                };
            }
            buffer.extend_from_slice(&more[..read]);
        };
        let mut request = request;
        if request.header("transfer-encoding").is_some() {
            return Err(Ok(Refused(411, "a request's body must have a length")));
        }
        let length = match request.header("content-length") {
            None => 0,
            Some(length) => length
                .trim()
                .parse::<usize>()
                .map_err(|_| Ok(Refused(400, "the request's length could not be read")))?,
        };
        if length > MAX_BODY {
            return Err(Ok(Refused(413, "the request is too large")));
        }
        let mut body = buffer.split_off(head);
        if body.len() > length {
            self.pending = body.split_off(length);
        }
        if body.len() < length {
            let have = body.len();
            body.resize(length, 0);
            self.stream.read_exact(&mut body[have..]).map_err(Err)?;
        }
        request.body = body;
        Ok(Some(request))
    }

    /// Closes the connection gently after a request that was not read whole: what the client
    /// still sends is read and dropped for a moment, so its answer is not lost to a reset.
    fn linger(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Write);
        let _ = self.stream.set_read_timeout(Some(Duration::from_secs(1)));
        let mut dropped = [0; 4096];
        let mut left = MAX_HEAD * 4;
        while left > 0 {
            match self.stream.read(&mut dropped) {
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
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        match response.body {
            Body::Bytes(bytes) => {
                head.push_str(&format!("Content-Length: {}\r\n\r\n", bytes.len()));
                self.stream.write_all(head.as_bytes())?;
                self.stream.write_all(&bytes)?;
                self.stream.flush()
            }
            Body::Stream(stream) => {
                head.push_str("Transfer-Encoding: chunked\r\n\r\n");
                self.stream.write_all(head.as_bytes())?;
                let mut chunked = Chunked(&mut self.stream);
                stream(&mut chunked)?;
                self.stream.write_all(b"0\r\n\r\n")?;
                self.stream.flush()
            }
        }
    }
}

/// Writes what it is given as chunks of a chunked answer, each sent at once.
struct Chunked<'a>(&'a mut TcpStream);

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

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        206 => "Partial Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Content Too Large",
        416 => "Range Not Satisfiable",
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
}
