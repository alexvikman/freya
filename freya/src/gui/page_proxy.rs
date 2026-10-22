//! The server's pages in the user's browser, at `http://127.0.0.1:47477`: Freya carries what the browser asks
//! to the server, the way it reaches the server itself (over TLS with the certificate pinned,
//! for a server elsewhere). So the browser never meets the server's own certificate, nothing
//! leaves this computer unencrypted, and a password manager keeps the login for one address on
//! every computer.
//!
//! The login page sends the browser back here, to `/callback`, with a code; Freya trades it
//! for a token before it says the user is logged in.
//!
//! The password page is the user's own: Freya opens it with a start that works once, and the
//! browser that comes with it is given a cookie, which alone opens the page after. The ticket
//! the server gave for it is added on the way, so it is in no URL or command line, and another
//! program on this computer asking the proxy is given nothing.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};

use freya_client::{Http, Login, Pages};
use freya_protocol::PAGES_PATH;
use freya_server::http::{Request, Response, Server};

/// Where the browser opens the server's pages.
pub const ADDRESS: &str = "127.0.0.1:47477";

/// The headers of a page passed on to the browser: where it sends the browser, and what keeps
/// it safe.
const KEPT_HEADERS: [&str; 6] = [
    "Location",
    "Content-Security-Policy",
    "X-Frame-Options",
    "X-Content-Type-Options",
    "Referrer-Policy",
    "Cache-Control",
];

/// What happened in the browser.
pub enum Heard {
    /// Logged in: the token, or why not.
    LoggedIn(Result<freya_client::Granted, String>),
    PasswordChanged,
}

/// The server whose pages are shown, and the login waiting for its code.
#[derive(Default)]
struct Showing {
    pages: Option<Pages>,
    login: Option<(Http, Login)>,
    password: Option<PasswordPage>,
}

/// The password page shown: the start Freya opens it with, once; the cookie the browser that
/// came with it was given; and the server's ticket for it.
struct PasswordPage {
    start: String,
    cookie: Option<String>,
    ticket: String,
}

/// The cookie the password page is opened with.
const COOKIE: &str = "freya-password";

pub struct PageProxy {
    showing: Arc<Mutex<Showing>>,
    address: String,
    _server: Server,
}

impl PageProxy {
    /// Listens at [`ADDRESS`], telling `tell` what happens.
    pub fn start(tell: Sender<Heard>) -> std::io::Result<Self> {
        Self::start_at(ADDRESS, tell)
    }

    fn start_at(address: &str, tell: Sender<Heard>) -> std::io::Result<Self> {
        let showing = Arc::new(Mutex::new(Showing::default()));
        let shown = Arc::clone(&showing);
        let tell = Mutex::new(tell);
        let here = address.to_owned();
        let server = Server::start(
            address,
            Arc::new(move |request| answer(&here, &shown, &tell, &request)),
        )?;
        Ok(Self {
            showing,
            address: address.to_owned(),
            _server: server,
        })
    }

    /// Where the browser opens the pages: `http://127.0.0.1:47477`.
    pub fn origin(&self) -> String {
        format!("http://{}", self.address)
    }

    fn showing(&self) -> MutexGuard<'_, Showing> {
        self.showing.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Shows the pages of `server`, waiting for `login` to come back.
    pub fn log_in(&self, server: Http, login: Login) {
        let mut showing = self.showing();
        showing.pages = Some(server.pages());
        showing.login = Some((server, login));
        showing.password = None;
    }

    /// Shows `pages`, its password page opened with `ticket`, and no login waiting: the page
    /// to open, once.
    pub fn show_password(&self, pages: Pages, ticket: String) -> String {
        let start = random_hex();
        let mut showing = self.showing();
        showing.pages = Some(pages);
        showing.login = None;
        showing.password = Some(PasswordPage {
            start: start.clone(),
            cookie: None,
            ticket,
        });
        format!("{}{PAGES_PATH}/password?start={start}", self.origin())
    }

    /// Where the login page sends the browser back to.
    pub fn callback(&self) -> String {
        format!("{}/callback", self.origin())
    }
}

/// What the proxy at `address` answers `request` with.
fn answer(
    address: &str,
    showing: &Mutex<Showing>,
    tell: &Mutex<Sender<Heard>>,
    request: &Request,
) -> Response {
    let showing = || showing.lock().unwrap_or_else(|e| e.into_inner());
    let tell = |heard| {
        let _ = tell.lock().unwrap_or_else(|e| e.into_inner()).send(heard);
    };
    // A page elsewhere that names this computer by DNS, or a form sent from one, is not
    // answered.
    if request.header("host") != Some(address) {
        return text(403, "Open this page from Freya.");
    }
    let origin = format!("http://{address}");
    // A form is taken only from the pages themselves, as a browser says it sent it.
    if request.method == "POST" && request.header("origin") != Some(origin.as_str()) {
        return text(403, "Open this page from Freya.");
    }
    if (request.method.as_str(), request.path.as_str()) == ("GET", "/callback") {
        // Taken only by the login's own state, so another page cannot end it.
        let waiting = {
            let mut showing = showing();
            match &showing.login {
                None => None,
                Some((_, login)) if request.query_value("state") != Some(login.state.as_str()) => {
                    return text(
                        400,
                        "This is not the login Freya asked for. Start again from its settings.",
                    );
                }
                Some(_) => showing.login.take(),
            }
        };
        let Some((server, login)) = waiting else {
            return text(
                404,
                "Freya is not logging in now. Start again from its settings.",
            );
        };
        let code = request.query_value("code").unwrap_or_default();
        let device = crate::settings::device_name();
        return match server.redeem(&login, code, &device) {
            Ok(tokens) => {
                tell(Heard::LoggedIn(Ok(tokens)));
                let mut sent = Response::bytes(303, "text/plain; charset=utf-8", Vec::new());
                sent.headers
                    .push(("Location", format!("{PAGES_PATH}/logged-in")));
                sent
            }
            Err(e) => {
                tell(Heard::LoggedIn(Err(e.to_string())));
                text(502, &format!("Freya could not log in: {e}"))
            }
        };
    }
    if !request.path.starts_with(&format!("{PAGES_PATH}/")) {
        return text(404, "No such page.");
    }
    let password_page = request.path == format!("{PAGES_PATH}/password");
    let path = if password_page {
        let mut showing = showing();
        let Some(page) = showing.password.as_mut() else {
            return text(404, "Open this page from Freya's settings.");
        };
        // Opened by Freya: the browser that came is given the cookie, once.
        if let Some(start) = request.query_value("start") {
            if page.cookie.is_some() || start != page.start {
                return text(
                    403,
                    "This page was opened already. Open it again from Freya's settings.",
                );
            }
            let cookie = random_hex();
            page.cookie = Some(cookie.clone());
            let mut sent = Response::bytes(303, "text/plain; charset=utf-8", Vec::new());
            sent.headers.extend([
                ("Location", format!("{PAGES_PATH}/password")),
                (
                    "Set-Cookie",
                    format!(
                        "{COOKIE}={cookie}; HttpOnly; SameSite=Strict; Path={PAGES_PATH}/password"
                    ),
                ),
                ("Cache-Control", "no-store".into()),
            ]);
            return sent;
        }
        let came = page
            .cookie
            .as_ref()
            .is_some_and(|cookie| sent_cookie(request).as_deref() == Some(cookie.as_str()));
        if !came {
            return text(403, "Open this page from Freya's settings.");
        }
        // Its ticket, added here.
        if request.method == "GET" {
            format!("{}?ticket={}", request.path, page.ticket)
        } else {
            request.path.clone()
        }
    } else if request.query.is_empty() {
        request.path.clone()
    } else {
        format!("{}?{}", request.path, request.query)
    };
    let Some(pages) = showing().pages.clone() else {
        return text(404, "Open this page from Freya.");
    };
    let page = match pages.ask(
        &request.method,
        &path,
        request.header("content-type"),
        request.body.clone(),
    ) {
        Ok(page) => page,
        Err(e) => return text(502, &format!("Freya could not reach the server: {e}")),
    };
    if request.method == "POST" && password_page && page.status == 200 {
        // Done with: the page is not opened again with its cookie.
        showing().password = None;
        tell(Heard::PasswordChanged);
    }
    let html = page.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("content-type") && value.starts_with("text/html")
    });
    let kind = if html {
        "text/html; charset=utf-8"
    } else {
        "text/plain; charset=utf-8"
    };
    let mut response = Response::bytes(page.status, kind, page.body);
    for (name, value) in page.headers {
        if let Some(kept) = KEPT_HEADERS
            .iter()
            .find(|kept| kept.eq_ignore_ascii_case(&name))
        {
            response.headers.push((kept, value));
        }
    }
    response
}

/// The password page's cookie, as the browser sent it.
fn sent_cookie(request: &Request) -> Option<String> {
    request
        .header("cookie")?
        .split(';')
        .find_map(|pair| pair.trim().strip_prefix(&format!("{COOKIE}=")))
        .map(str::to_owned)
}

fn random_hex() -> String {
    let mut bytes = [0; 16];
    // The system's randomness failing leaves nothing safe to do.
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .expect("the system's random numbers");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A page of plain text.
fn text(status: u16, said: &str) -> Response {
    Response::bytes(status, "text/plain; charset=utf-8", said.into())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;
    use std::time::Duration;

    use freya_library::Library;
    use freya_server::Served;
    use freya_server::accounts::Accounts;

    use super::*;

    const FORM: &str = "application/x-www-form-urlencoded";

    #[test]
    fn listens_where_servers_send_logins_back() {
        assert_eq!(
            ADDRESS,
            format!("127.0.0.1:{}", freya_protocol::CALLBACK_PORT)
        );
    }

    fn header<'a>(answer: &'a ureq::http::Response<ureq::Body>, name: &str) -> &'a str {
        answer
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
    }

    #[test]
    fn logs_in_and_changes_the_password_in_the_browser() {
        let dir = std::env::temp_dir().join(format!("freya-page-proxy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pairing = dir.join("pairing.key");
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let here = free.local_addr().unwrap().to_string();
        drop(free);
        let served = Served {
            library: Arc::new(freya_server::LocalLibrary::new(
                Library::open_in_memory().unwrap(),
                None,
            )),
            server: None,
            quit: None,
            accounts: Some(Arc::new(Accounts::open(None, Some(pairing.clone())))),
            certificate: None,
            callback_port: here.rsplit_once(':').unwrap().1.parse().unwrap(),
        };
        let server = Server::start("127.0.0.1:0", freya_server::handler(served)).unwrap();
        let address = format!("http://{}", server.address());
        let key = std::fs::read_to_string(&pairing).unwrap();
        let mac = Http::new(&address)
            .with_token(Some(Http::new(&address).pair(&key, "Mac").unwrap().access));
        let user = mac.account().unwrap().unwrap().user;

        let (tell, heard) = channel();
        let proxy = PageProxy::start_at(&here, tell).unwrap();
        let origin = proxy.origin();
        let browser: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .into();
        let wait = || heard.recv_timeout(Duration::from_secs(10)).unwrap();

        // The password, set on the page Freya opened, as from the server's own computer.
        let ticket = mac.password_ticket().unwrap();
        let page = proxy.show_password(mac.pages(), ticket.clone());
        assert!(!page.contains(&ticket), "the ticket is in no URL");
        // Another program on this computer is given nothing.
        let other = browser
            .get(&format!("{origin}/web/password"))
            .call()
            .unwrap();
        assert_eq!(other.status(), 403);
        let started = browser.get(&page).call().unwrap();
        assert_eq!(started.status(), 303);
        let cookie = header(&started, "set-cookie")
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        assert!(cookie.starts_with("freya-password="));
        assert!(header(&started, "set-cookie").contains("HttpOnly"));
        assert_eq!(browser.get(&page).call().unwrap().status(), 403, "once");
        let shown = browser
            .get(&format!("{origin}/web/password"))
            .header("Cookie", cookie.as_str())
            .call()
            .unwrap();
        assert_eq!(shown.status(), 200);
        let form = format!("ticket={ticket}&password=correct+horse&again=correct+horse");
        let unsent = browser
            .post(&format!("{origin}/web/password"))
            .header("Content-Type", FORM)
            .send(form.clone())
            .unwrap();
        assert_eq!(unsent.status(), 403, "a form says where it was sent from");
        let elsewhere = browser
            .post(&format!("{origin}/web/password"))
            .header("Content-Type", FORM)
            .header("Origin", "https://evil.example")
            .send(form.clone())
            .unwrap();
        assert_eq!(elsewhere.status(), 403);
        let without = browser
            .post(&format!("{origin}/web/password"))
            .header("Content-Type", FORM)
            .header("Origin", origin.as_str())
            .send(form.clone())
            .unwrap();
        assert_eq!(without.status(), 403, "not without the cookie");
        let changed = browser
            .post(&format!("{origin}/web/password"))
            .header("Content-Type", FORM)
            .header("Origin", origin.as_str())
            .header("Cookie", cookie.as_str())
            .send(form)
            .unwrap();
        assert_eq!(changed.status(), 200);
        assert!(matches!(wait(), Heard::PasswordChanged));

        // Logging in: the page sends the browser back with a code, which Freya trades for a
        // token before it says so.
        let login = Login::new(&proxy.callback());
        let page = login.page();
        proxy.log_in(Http::new(&address), login);
        let shown = browser.get(&format!("{origin}{page}")).call().unwrap();
        assert_eq!(shown.status(), 200);
        assert_eq!(header(&shown, "x-frame-options"), "DENY");
        let opened = page.split_once('?').unwrap().1;
        let sent = browser
            .post(&format!("{origin}/web/login"))
            .header("Content-Type", FORM)
            .header("Origin", origin.as_str())
            .send(format!("{opened}&username={user}&password=correct+horse"))
            .unwrap();
        assert_eq!(sent.status(), 303);
        let back = header(&sent, "location").to_owned();
        assert!(back.starts_with(&format!("{origin}/callback?code=")));
        // Another page sending the browser back with another state ends nothing.
        let forged = browser
            .get(&format!("{origin}/callback?code=x&state=forged"))
            .call()
            .unwrap();
        assert_eq!(forged.status(), 400);
        let done = browser.get(&back).call().unwrap();
        assert_eq!(done.status(), 303);
        assert_eq!(header(&done, "location"), "/web/logged-in");
        let Heard::LoggedIn(Ok(tokens)) = wait() else {
            panic!("not logged in");
        };
        let pc = Http::new(&address).with_token(Some(tokens.access));
        assert_eq!(pc.account().unwrap().unwrap().devices.len(), 2);
        let logged_in = browser
            .get(&format!("{origin}/web/logged-in"))
            .call()
            .unwrap();
        assert_eq!(logged_in.status(), 200);
        // Once.
        assert_eq!(browser.get(&back).call().unwrap().status(), 404);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
