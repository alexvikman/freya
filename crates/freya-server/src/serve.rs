//! The library's API over HTTP, under [`API_PATH`]:
//!
//! - `POST /call/{method}`: a call of [`freya_protocol::LibraryData`] or
//!   [`freya_protocol::ServerControl`], its arguments in the body as JSON, answered in JSON;
//!   `{"error": ...}` when it failed.
//! - `GET /pictures/{checksum}`: a picture the library keeps.
//! - `POST /pictures`, with `?largest=` to make it smaller: keeps the picture in the body,
//!   answering its checksum.
//! - `POST /album-art`: the cover of the album whose track `{"track", "largest"}` is.
//! - `GET /audio?path=`: a track's file, the path percent-encoded, whole or the part a Range
//!   header asks for; only the library's own music.
//! - `GET /events`: what happens at the server, as it happens: one [`ServiceEvent`] in JSON
//!   a line, and an empty line now and then while nothing does.
//! - `GET /version`: `{"version": ...}`.
//! - `POST /quit`: the server stops; only from this computer.
//!
//! Only Freya's clients are answered, never a web page: until clients log in, a page in the
//! user's browser could otherwise call the server on this computer. A request a browser sends
//! for a page carries an `Origin`; one through a name that leads to this computer by DNS, a
//! `Host` other than this computer's; and a form, a body of another type. Each is refused.

use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use freya_protocol::{API_PATH, LibraryApi, Server, ServiceEvent, dispatch_control, dispatch_data};

use crate::http::{Handler, Request, Response};

/// How long the event stream says nothing before it sends an empty line, to find a client
/// that is gone.
const HEARTBEAT: Duration = Duration::from_secs(15);

/// What a server serves.
#[derive(Clone)]
pub struct Served {
    pub library: Arc<dyn LibraryApi>,
    pub server: Option<Arc<dyn Server>>,
    /// Stops the server, when it may be stopped from this computer.
    pub quit: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Answers requests to the API as `served` says.
pub fn handler(served: Served) -> Handler {
    Arc::new(move |request| answer(&served, request))
}

/// Why `request` is refused, if it is: see the module's documentation.
fn refused(request: &Request) -> Option<&'static str> {
    if request.header("origin").is_some() {
        return Some("web pages are not answered");
    }
    let host = request.header("host").unwrap_or_default();
    let name = match host.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    };
    if !["127.0.0.1", "localhost", "[::1]"].contains(&name.to_ascii_lowercase().as_str()) {
        return Some("only this computer's name is answered");
    }
    let typed = request.header("content-type").is_some_and(|kind| {
        let kind = kind.split(';').next().unwrap_or_default().trim();
        kind.eq_ignore_ascii_case("application/json")
            || kind.eq_ignore_ascii_case("application/octet-stream")
    });
    if !request.body.is_empty() && !typed {
        return Some("a body must be JSON or bytes");
    }
    None
}

fn answer(served: &Served, request: Request) -> Response {
    if let Some(why) = refused(&request) {
        return Response::error(403, why);
    }
    let Some(path) = request.path.strip_prefix(API_PATH) else {
        return Response::error(404, "no such page");
    };
    let library = served.library.as_ref();
    match (request.method.as_str(), path) {
        ("POST", path) if path.starts_with("/call/") => {
            let method = &path["/call/".len()..];
            let args = match serde_json::from_slice(&request.body) {
                Ok(args) => args,
                Err(e) => return Response::error(400, e),
            };
            let answered =
                dispatch_data(library, method, args).or_else(|args| match &served.server {
                    Some(server) => dispatch_control(server.as_ref(), method, args),
                    None => Err(args),
                });
            match answered {
                Err(_) => Response::error(404, format!("no such call: {method}")),
                Ok(Ok(answer)) => Response::json(200, &answer),
                Ok(Err(e)) => Response::error(500, e),
            }
        }
        ("GET", path) if path.starts_with("/pictures/") => {
            match library.picture(&path["/pictures/".len()..]) {
                Ok(Some(bytes)) => Response::bytes(200, "image/jpeg", bytes),
                Ok(None) => Response::error(404, "no such picture"),
                Err(e) => Response::error(500, e),
            }
        }
        ("POST", "/pictures") => {
            let largest = match request.query_value("largest").map(str::parse::<u32>) {
                None => None,
                Some(Ok(largest)) => Some(largest),
                Some(Err(e)) => return Response::error(400, e),
            };
            let kept = match largest {
                Some(largest) => library.take_picture(&request.body, largest),
                None => library.put_picture(&request.body),
            };
            match kept {
                Ok(checksum) => Response::json(200, &serde_json::Value::String(checksum)),
                Err(e) => Response::error(500, e),
            }
        }
        ("POST", "/album-art") => {
            #[derive(serde::Deserialize)]
            struct Asked {
                track: std::path::PathBuf,
                largest: u32,
            }
            let asked: Asked = match serde_json::from_slice(&request.body) {
                Ok(asked) => asked,
                Err(e) => return Response::error(400, e),
            };
            match library.album_art(&asked.track, asked.largest) {
                Ok(Some(bytes)) => Response::bytes(200, "image/jpeg", bytes),
                Ok(None) => Response::error(404, "no cover"),
                Err(e) => Response::error(500, e),
            }
        }
        ("GET", "/audio") => audio(library, &request),
        ("GET", "/events") => {
            let Some(server) = &served.server else {
                return Response::error(404, "no events here");
            };
            let events = match server.events() {
                Ok(events) => events,
                Err(e) => return Response::error(500, e),
            };
            Response::stream(
                "application/x-ndjson",
                Box::new(move |out| {
                    loop {
                        let line = match events.recv_timeout(HEARTBEAT) {
                            Ok(event) => event_line(&event),
                            Err(RecvTimeoutError::Timeout) => "\n".to_owned(),
                            Err(RecvTimeoutError::Disconnected) => return Ok(()),
                        };
                        // Fails once the client is gone.
                        out.write_all(line.as_bytes())?;
                    }
                }),
            )
        }
        ("GET", "/version") => Response::json(
            200,
            &serde_json::json!({ "version": env!("CARGO_PKG_VERSION") }),
        ),
        ("POST", "/quit") => {
            let local = request.peer.is_some_and(|peer| peer.ip().is_loopback());
            match &served.quit {
                Some(quit) if local => {
                    quit();
                    Response::json(200, &serde_json::Value::Null)
                }
                _ => Response::error(403, "the server is not stopped from here"),
            }
        }
        _ => Response::error(404, "no such page"),
    }
}

/// The file of the library's track `?path=`, whole or the part `Range` asks for.
fn audio(library: &dyn LibraryApi, request: &Request) -> Response {
    let Some(path) = request
        .query_value("path")
        .and_then(percent_decode)
        .map(std::path::PathBuf::from)
    else {
        return Response::error(400, "which file?");
    };
    match library.knows_file(&path) {
        Ok(true) => {}
        Ok(false) => return Response::error(404, "no such track"),
        Err(e) => return Response::error(500, e),
    }
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) => return Response::error(404, e),
    };
    let length = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(e) => return Response::error(500, e),
    };
    let kind = audio_type(&path);
    let Some(range) = request.header("range") else {
        let mut whole = Response::read(200, kind, length, Box::new(file));
        whole.headers.push(("Accept-Ranges", "bytes".into()));
        return whole;
    };
    let Some((from, to)) = byte_range(range, length) else {
        let mut refused = Response::error(416, "no such part");
        refused
            .headers
            .push(("Content-Range", format!("bytes */{length}")));
        return refused;
    };
    use std::io::{Seek, SeekFrom};
    if let Err(e) = file.seek(SeekFrom::Start(from)) {
        return Response::error(500, e);
    }
    let mut part = Response::read(206, kind, to + 1 - from, Box::new(file));
    part.headers.push(("Accept-Ranges", "bytes".into()));
    part.headers
        .push(("Content-Range", format!("bytes {from}-{to}/{length}")));
    part
}

/// The first and last byte a `Range` header asks for of a file of `length` bytes: "bytes=0-99",
/// "bytes=100-" or "bytes=-100". None for one that cannot be had, or of several parts.
fn byte_range(range: &str, length: u64) -> Option<(u64, u64)> {
    let spec = range.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || length == 0 {
        return None;
    }
    let (from, to) = spec.split_once('-')?;
    let (from, to) = (from.trim(), to.trim());
    let last = length - 1;
    let (from, to) = if from.is_empty() {
        // The last `to` bytes.
        let count: u64 = to.parse().ok()?;
        (length.saturating_sub(count), last)
    } else {
        let from: u64 = from.parse().ok()?;
        let to = if to.is_empty() {
            last
        } else {
            to.parse::<u64>().ok()?.min(last)
        };
        (from, to)
    };
    (from <= to && from < length).then_some((from, to))
}

/// The text `value` stands for, with its `%XX` escapes undone; none when they are not valid,
/// or not UTF-8.
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// The media type of an audio file, by its extension.
fn audio_type(path: &std::path::Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "m4a" | "mp4" | "aac" => "audio/mp4",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

fn event_line(event: &ServiceEvent) -> String {
    let mut line = serde_json::to_string(event).unwrap_or_default();
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(headers: &[(&str, &str)], body: &[u8]) -> Request {
        Request {
            peer: None,
            method: "POST".into(),
            path: format!("{API_PATH}/call/stats"),
            query: String::new(),
            headers: headers
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect(),
            body: body.to_vec(),
        }
    }

    #[test]
    fn reads_ranges_and_escapes() {
        assert_eq!(byte_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(byte_range("bytes=900-", 1000), Some((900, 999)));
        assert_eq!(byte_range("bytes=990-2000", 1000), Some((990, 999)));
        assert_eq!(byte_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(byte_range("bytes=1000-", 1000), None);
        assert_eq!(byte_range("bytes=5-1", 1000), None);
        assert_eq!(byte_range("bytes=0-1,5-6", 1000), None);
        assert_eq!(byte_range("lines=0-1", 1000), None);
        assert_eq!(
            percent_decode("%2Fm%C3%BAsica%20x%2B1").as_deref(),
            Some("/música x+1")
        );
        assert_eq!(percent_decode("%2"), None);
        assert_eq!(percent_decode("%FF"), None);
    }

    #[test]
    fn only_freyas_clients_are_answered() {
        let host = ("Host", "127.0.0.1:47474");
        let json = ("Content-Type", "application/json");
        assert_eq!(refused(&request(&[host, json], b"null")), None);
        assert_eq!(refused(&request(&[("Host", "localhost:47474")], b"")), None);
        assert_eq!(refused(&request(&[("Host", "[::1]:47474")], b"")), None);
        // A page's own call, or a form it sends.
        assert!(
            refused(&request(
                &[host, json, ("Origin", "https://evil.example")],
                b"null"
            ))
            .is_some()
        );
        assert!(refused(&request(&[host, ("Content-Type", "text/plain")], b"null")).is_some());
        assert!(refused(&request(&[host], b"null")).is_some());
        // A name that leads here by DNS.
        assert!(refused(&request(&[("Host", "evil.example:47474"), json], b"null")).is_some());
        assert!(refused(&request(&[json], b"null")).is_some());
    }
}
