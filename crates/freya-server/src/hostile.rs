//! Hostile and random input against what the server reads before anyone logged in: the API's
//! routes and the pages, called as the HTTP server calls them, and the HTTP server itself over
//! the network. Nothing may panic, nothing may be let in without a token, and the server must
//! still answer after.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use freya_protocol::{API_PATH, PAGES_PATH};

use crate::accounts::Accounts;
use crate::http::{Request, Response, Server};
use crate::{LocalLibrary, Served, admit, handler};

/// A small random number generator, the same each run: xorshift.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
        from[self.below(from.len())]
    }

    fn bytes(&mut self, most: usize) -> Vec<u8> {
        let length = self.below(most + 1);
        (0..length).map(|_| self.next() as u8).collect()
    }

    /// Text with the characters that break parsers most often among it.
    fn text(&mut self, most: usize) -> String {
        const PIECES: [&str; 24] = [
            "a",
            "%",
            "%2",
            "%zz",
            "%00",
            "%ff",
            "%e2%9c%93",
            "+",
            "&",
            "=",
            "?",
            "#",
            "/",
            "..",
            "\u{0}",
            "\r\n",
            "\"",
            "<script>",
            "'",
            " ",
            "é",
            "✓",
            "-",
            "9999999999999999999",
        ];
        let count = self.below(most + 1);
        (0..count)
            .map(|_| PIECES[self.below(PIECES.len())])
            .collect()
    }
}

fn served() -> (Served, Arc<Accounts>) {
    let accounts = Arc::new(Accounts::open(None, None));
    let served = Served {
        library: Arc::new(LocalLibrary::new(
            freya_library::Library::open_in_memory().unwrap(),
            None,
        )),
        server: None,
        quit: None,
        accounts: Some(Arc::clone(&accounts)),
        certificate: Some(crate::tls::Identity::make().unwrap().certificate),
        callback_port: freya_protocol::CALLBACK_PORT,
    };
    (served, accounts)
}

/// A request a stranger might send: to a route that is there, or near one, with headers and a
/// body made up, many of them wrong.
fn stranger(random: &mut Random) -> Request {
    const ROUTES: [&str; 16] = [
        "/call/stats",
        "/call/set_settings",
        "/call/",
        "/pictures/",
        "/pictures",
        "/album-art",
        "/audio",
        "/events",
        "/account",
        "/account/password-page",
        "/account/connection-code",
        "/account/revoke",
        "/account/log-out",
        "/quit",
        "/info",
        "/certificate",
    ];
    const PAGES: [&str; 5] = ["/login", "/password", "/logged-in", "/", "/../etc"];
    let path = match random.below(3) {
        0 => format!("{API_PATH}{}{}", random.pick(&ROUTES), random.text(3)),
        1 => format!("{PAGES_PATH}{}{}", random.pick(&PAGES), random.text(2)),
        _ => format!("/{}", random.text(6)),
    };
    let mut headers = Vec::new();
    for _ in 0..random.below(5) {
        let name = random.pick(&[
            "Host",
            "Origin",
            "Content-Type",
            "Authorization",
            "Range",
            "Cookie",
            "Content-Length",
        ]);
        let value = match name {
            "Host" => random
                .pick(&[
                    "127.0.0.1:47474",
                    "evil.example",
                    "localhost",
                    "[::1]:1",
                    "",
                ])
                .to_owned(),
            "Authorization" => format!("Bearer {}", random.text(8)),
            "Content-Type" => random
                .pick(&[
                    "application/json",
                    "application/x-www-form-urlencoded",
                    "text/plain",
                    "application/octet-stream; x",
                ])
                .to_owned(),
            _ => random.text(6),
        };
        headers.push((name.to_owned(), value));
    }
    let body = match random.below(4) {
        0 => Vec::new(),
        1 => random.bytes(256),
        2 => format!(
            "{{\"key\":\"{}\",\"nonce\":\"{}\"}}",
            random.text(4),
            random.text(4)
        )
        .into_bytes(),
        _ => random.text(12).into_bytes(),
    };
    Request {
        peer: if random.below(2) == 0 {
            Some("127.0.0.1:50000".parse().unwrap())
        } else {
            Some("192.168.1.66:50000".parse().unwrap())
        },
        secure: random.below(2) == 0,
        method: random
            .pick(&["GET", "POST", "PUT", "DELETE", "", "GET\r\nX"])
            .to_owned(),
        path,
        query: random.text(6),
        headers,
        body,
    }
}

/// Whether `response` let a stranger in where only a device with a token, or nobody, may be.
fn let_in(request: &Request, response: &Response) -> bool {
    let Some(route) = request.path.strip_prefix(API_PATH) else {
        return false;
    };
    let open = ["/version", "/certificate", "/pair", "/token", "/enroll"];
    let open = open.contains(&route);
    response.status < 300 && !open
}

#[test]
fn strangers_are_answered_and_let_in_nowhere() {
    let (served, _accounts) = served();
    let answer = handler(served.clone());
    let limit = admit(served);
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    for _ in 0..20_000 {
        let request = stranger(&mut random);
        let _ = limit(&request);
        let shown = format!("{} {}", request.method, request.path);
        let response = answer(Request {
            peer: request.peer,
            secure: request.secure,
            method: request.method.clone(),
            path: request.path.clone(),
            query: request.query.clone(),
            headers: request.headers.clone(),
            body: request.body.clone(),
        });
        assert!(
            (200..600).contains(&response.status),
            "{shown}: {}",
            response.status
        );
        assert!(!let_in(&request, &response), "{shown} let a stranger in");
    }
}

#[test]
fn the_http_server_takes_garbage_and_answers_after() {
    let server = Server::start(
        "127.0.0.1:0",
        Arc::new(|request: Request| Response::bytes(200, "text/plain", request.path.into_bytes())),
    )
    .unwrap();
    let mut random = Random(0x2545_f491_4f6c_dd1d);
    let valid = b"POST /a HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\n\r\nbody".to_vec();
    for _ in 0..300 {
        // A valid request, cut, mangled, or with garbage put in.
        let mut sent = valid.clone();
        match random.below(5) {
            0 => sent.truncate(random.below(sent.len())),
            1 => {
                let at = random.below(sent.len());
                sent[at] = random.next() as u8;
            }
            2 => {
                let at = random.below(sent.len());
                let garbage = random.bytes(64);
                sent.splice(at..at, garbage);
            }
            3 => sent = random.bytes(512),
            _ => {
                let header = format!("Content-Length: {}\r\n", random.text(3));
                let at = valid.windows(2).position(|w| w == b"\r\n").unwrap() + 2;
                sent.splice(at..at, header.into_bytes());
            }
        }
        let Ok(mut stream) = TcpStream::connect(server.address()) else {
            continue;
        };
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let _ = stream.write_all(&sent);
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let mut answer = Vec::new();
        let _ = stream.read_to_end(&mut answer);
    }
    // Still there, and right.
    let mut stream = TcpStream::connect(server.address()).unwrap();
    stream
        .write_all(b"GET /still HTTP/1.1\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    assert!(
        answer.starts_with("HTTP/1.1 200") && answer.ends_with("/still"),
        "{answer}"
    );
}
