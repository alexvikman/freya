//! The library's API over HTTP, under [`API_PATH`]:
//!
//! - `POST /call/{method}`: a call of [`freya_protocol::LibraryData`] or
//!   [`freya_protocol::ServerControl`], its arguments in the body as JSON, answered in JSON;
//!   `{"error": ...}` when it failed.
//! - `GET /pictures/{checksum}`: a picture the library keeps.
//! - `POST /pictures`, with `?largest=` to make it smaller: keeps the picture in the body,
//!   answering its checksum.
//! - `POST /album-art`: the cover of the album whose track `{"track", "largest"}` is.
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
