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
//! - `GET /info`: what the server is called, where it takes clients on other computers and
//!   its certificate's fingerprint, as it answers those looking for it.
//! - `GET /certificate`: the server's TLS certificate, in DER, for a client to pin the first
//!   time it connects.
//! - `POST /quit`: the server stops; only from this computer.
//! - `POST /pair`: `{"key", "device"}`, the pairing key a client on this computer read: tokens
//!   for the device, as `/token` gives them. Only from this computer.
//! - `POST /token`: tokens for a device, `{"access_token", "refresh_token", "token_type",
//!   "expires_in"}` (see [`crate::accounts`]), for `{"grant_type": "authorization_code",
//!   "code", "code_verifier", "redirect_uri", "device"}`, the code the login page gave (see
//!   [`crate::pages`]), or for `{"grant_type": "refresh_token", "refresh_token"}`.
//! - `POST /enroll`: `{"nonce"}`, hex: `{"proof"}`, the HMAC with which the server proves it
//!   knows the connection code its computer shows (see [`crate::accounts`]). No token.
//! - `POST /account/connection-code`: a new connection code, `{"code", "lasts"}` (seconds);
//!   only an admin's.
//! - `GET /account`: the caller's account and devices; `POST /account/password-page`: a
//!   ticket for the page the password is changed on, `{"ticket"}`; `POST /account/revoke`:
//!   `{"device"}`; `POST /account/log-out`: the caller's own token taken back.
//!
//! Every request but `/version`, `/pair`, `/token` and `/enroll` carries a device's access token, as
//! `Authorization: Bearer ...`, or is refused. A listener's device is refused what changes the
//! server: every call but those [`freya_protocol::LISTENER_CALLS`] names, and keeping
//! pictures. The pages a browser opens, under
//! [`freya_protocol::PAGES_PATH`], have their own rules (see [`crate::pages`]).
//!
//! Only Freya's clients are answered, never a web page: until clients log in, a page in the
//! user's browser could otherwise call the server on this computer. A request a browser sends
//! for a page carries an `Origin`; one through a name that leads to this computer by DNS, a
//! `Host` other than this computer's; and a form, a body of another type. Each is refused.

use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use freya_protocol::{
    API_PATH, LISTENER_CALLS, LibraryApi, NETWORK_PORT, Role, Server, ServerFound, ServiceEvent,
    dispatch_control, dispatch_data,
};

use crate::accounts::{Accounts, Caller};
use crate::http::{Handler, Request, Response};

/// How long the event stream says nothing before it sends an empty line, to find a client
/// that is gone; and how long it lasts before it ends, so a client following it notices a
/// server that is gone too, and follows it again.
const HEARTBEAT: Duration = Duration::from_secs(15);
const STREAM_LASTS: Duration = Duration::from_secs(50);

/// What a server serves.
#[derive(Clone)]
pub struct Served {
    pub library: Arc<dyn LibraryApi>,
    pub server: Option<Arc<dyn Server>>,
    /// Stops the server, when it may be stopped from this computer.
    pub quit: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Who may call; without, anyone may, as in tests.
    pub accounts: Option<Arc<Accounts>>,
    /// The server's TLS certificate, in DER, when clients on other computers may connect.
    pub certificate: Option<Vec<u8>>,
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
    // Over TLS a browser does not get as far as asking: it does not take the server's own
    // certificate without the user's say.
    if !is_here(request) && !request.secure {
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

/// Whether `request` calls this computer by a name only it has, not one that leads here by DNS.
pub(crate) fn is_here(request: &Request) -> bool {
    let host = request.header("host").unwrap_or_default();
    let name = match host.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    };
    ["127.0.0.1", "localhost", "[::1]"].contains(&name.to_ascii_lowercase().as_str())
}

fn answer(served: &Served, request: Request) -> Response {
    if let Some(page) = crate::pages::answer(served, &request) {
        return page;
    }
    if let Some(why) = refused(&request) {
        return Response::error(403, why);
    }
    let Some(path) = request.path.strip_prefix(API_PATH) else {
        return Response::error(404, "no such page");
    };
    if (request.method.as_str(), path) == ("GET", "/certificate") {
        return match &served.certificate {
            Some(certificate) => Response::bytes(200, "application/pkix-cert", certificate.clone()),
            None => Response::error(404, "no certificate here"),
        };
    }
    let local = request.peer.is_some_and(|peer| peer.ip().is_loopback());
    let caller = match &served.accounts {
        None => None,
        Some(accounts) => match (request.method.as_str(), path) {
            ("GET", "/version") => None,
            ("POST", "/pair") => return pair(accounts, &request, local),
            ("POST", "/token") => return token(accounts, &request),
            ("POST", "/enroll") => return enroll(accounts, served, &request),
            _ => {
                let token = request
                    .header("authorization")
                    .and_then(|value| value.strip_prefix("Bearer "));
                match token.and_then(|token| accounts.caller(token.trim())) {
                    Some(caller) => Some(caller),
                    None => return Response::error(401, "log in first"),
                }
            }
        },
    };
    if let Some(caller) = &caller
        && caller.role == Role::Listener
        && !listener_may(&request, path)
    {
        return Response::error(403, "only the server's admin may do that");
    }
    if let (Some(accounts), Some(caller)) = (&served.accounts, &caller)
        && let Some(answer) = account(accounts, caller, &request, path, local)
    {
        return answer;
    }
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
                    let ends = std::time::Instant::now() + STREAM_LASTS;
                    while std::time::Instant::now() < ends {
                        let line = match events.recv_timeout(HEARTBEAT) {
                            Ok(event) => event_line(&event),
                            Err(RecvTimeoutError::Timeout) => "\n".to_owned(),
                            Err(RecvTimeoutError::Disconnected) => return Ok(()),
                        };
                        // Fails once the client is gone.
                        out.write_all(line.as_bytes())?;
                    }
                    Ok(())
                }),
            )
        }
        ("GET", "/info") => match serde_json::to_value(found(served)) {
            Ok(info) => Response::json(200, &info),
            Err(e) => Response::error(500, e),
        },
        ("GET", "/version") => Response::json(
            200,
            &serde_json::json!({ "version": env!("CARGO_PKG_VERSION") }),
        ),
        ("POST", "/quit") => match &served.quit {
            Some(quit) if local => {
                quit();
                Response::json(200, &serde_json::Value::Null)
            }
            _ => Response::error(403, "the server is not stopped from here"),
        },
        _ => Response::error(404, "no such page"),
    }
}

/// Whether a listener may ask `request`: what reads the library and plays it, and the calls
/// [`LISTENER_CALLS`] names; not keeping pictures, which only editing the library does.
fn listener_may(request: &Request, path: &str) -> bool {
    match (request.method.as_str(), path) {
        ("POST", "/pictures") => false,
        ("POST", call) if call.starts_with("/call/") => {
            LISTENER_CALLS.contains(&&call["/call/".len()..])
        }
        _ => true,
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
pub(crate) fn percent_decode(value: &str) -> Option<String> {
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

/// What the server is called, where it takes clients on other computers and its certificate's
/// fingerprint: as it answers those looking for servers.
pub fn found(served: &Served) -> ServerFound {
    let name = served
        .server
        .as_ref()
        .and_then(|server| server.settings().ok())
        .map(|settings| settings.name)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(computer_name);
    ServerFound {
        name,
        port: NETWORK_PORT,
        fingerprint: served
            .certificate
            .as_deref()
            .map(crate::tls::fingerprint)
            .unwrap_or_default(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

/// What this computer is called.
fn computer_name() -> String {
    let named = std::env::var("COMPUTERNAME").ok().or_else(|| {
        let out = std::process::Command::new("hostname").output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    });
    named
        .map(|name| name.trim_end_matches(".local").to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Freya".into())
}

/// `{"key", "device"}`: a token for the device, given the pairing key; only from this computer.
fn pair(accounts: &Accounts, request: &Request, local: bool) -> Response {
    #[derive(serde::Deserialize)]
    struct Pairing {
        key: String,
        device: String,
    }
    if !local {
        return Response::error(403, "pairing is only from the server's computer");
    }
    let Ok(pairing) = serde_json::from_slice::<Pairing>(&request.body) else {
        return Response::error(400, "a key and a device");
    };
    match accounts.pair(pairing.key.trim(), &pairing.device) {
        Some(granted) => Response::json(200, &granted.json()),
        None => Response::error(403, "not the pairing key"),
    }
}

/// `{"nonce"}`: the proof that this server knows the connection code its computer shows, over
/// the nonce and the server's certificate.
fn enroll(accounts: &Accounts, served: &Served, request: &Request) -> Response {
    if !accounts.may_ask_proof(request.peer.map(|peer| peer.ip())) {
        return Response::error(
            429,
            "too many questions for the connection code: wait a while",
        );
    }
    #[derive(serde::Deserialize)]
    struct Asked {
        nonce: String,
    }
    let nonce = serde_json::from_slice::<Asked>(&request.body)
        .ok()
        .and_then(|asked| unhex(&asked.nonce))
        .filter(|nonce| nonce.len() == 32);
    let Some(nonce) = nonce else {
        return Response::error(400, "a nonce of 32 bytes, in hex");
    };
    let Some(certificate) = &served.certificate else {
        return Response::error(404, "this server takes no devices elsewhere");
    };
    match accounts.prove(&nonce, certificate) {
        Some(proof) => Response::json(200, &serde_json::json!({ "proof": hex(&proof) })),
        None => Response::error(
            404,
            "no connection code now: show one in Settings on the server's computer",
        ),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Tokens for a device: given the code the login page sent the browser on with, and the
/// secret the login's challenge was made from; or given the device's refresh token.
fn token(accounts: &Accounts, request: &Request) -> Response {
    #[derive(serde::Deserialize)]
    #[serde(tag = "grant_type", rename_all = "snake_case")]
    enum Traded {
        AuthorizationCode {
            code: String,
            code_verifier: String,
            redirect_uri: String,
            device: String,
        },
        RefreshToken {
            refresh_token: String,
        },
    }
    let granted = match serde_json::from_slice::<Traded>(&request.body) {
        Ok(Traded::AuthorizationCode {
            code,
            code_verifier,
            redirect_uri,
            device,
        }) => accounts.redeem(&code, &code_verifier, &redirect_uri, &device),
        Ok(Traded::RefreshToken { refresh_token }) => accounts.renew(&refresh_token),
        Err(_) => return Response::error(400, "a code or a refresh token"),
    };
    match granted {
        Some(granted) => Response::json(200, &granted.json()),
        None => Response::error(400, "invalid_grant: log in again"),
    }
}

/// The caller's account: what it is, the page its password is changed on, and taking its
/// devices' tokens back.
fn account(
    accounts: &Accounts,
    caller: &Caller,
    request: &Request,
    path: &str,
    local: bool,
) -> Option<Response> {
    #[derive(serde::Deserialize)]
    struct Revoked {
        device: String,
    }
    Some(match (request.method.as_str(), path) {
        ("GET", "/account") => match serde_json::to_value(accounts.info(caller)) {
            Ok(info) => Response::json(200, &info),
            Err(e) => Response::error(500, e),
        },
        ("POST", "/account/password-page") => Response::json(
            200,
            &serde_json::json!({ "ticket": accounts.ticket(caller, local) }),
        ),
        ("POST", "/account/connection-code") => {
            if caller.role != Role::Admin {
                return Some(Response::error(403, "only the server's admin may do that"));
            }
            let (code, lasts) = accounts.connection_code();
            Response::json(
                200,
                &serde_json::json!({ "code": code, "lasts": lasts.as_secs() }),
            )
        }
        ("POST", "/account/log-out") => {
            accounts.revoke(caller, &caller.device);
            Response::json(200, &serde_json::Value::Null)
        }
        ("POST", "/account/revoke") => match serde_json::from_slice::<Revoked>(&request.body) {
            Ok(revoked) if accounts.revoke(caller, &revoked.device) => {
                Response::json(200, &serde_json::Value::Null)
            }
            Ok(_) => Response::error(404, "no such device"),
            Err(e) => Response::error(400, e),
        },
        _ => return None,
    })
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
            secure: false,
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
