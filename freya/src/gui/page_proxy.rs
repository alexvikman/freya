//! The server's pages in the user's browser, at `http://127.0.0.1:47477`: Freya carries what the browser asks
//! to the server, the way it reaches the server itself (over TLS with the certificate pinned,
//! for a server elsewhere). So the browser never meets the server's own certificate, nothing
//! leaves this computer unencrypted, and a password manager keeps the login for one address on
//! every computer.
//!
//! The login page sends the browser back here, to `/callback`, with a code; Freya trades it
//! for a token before it says the user is logged in.
//!
//! The pages are the user's own: Freya opens them with a start that works once, and the
//! browser that comes with it is given a cookie, which alone opens the pages after, sends
//! their forms and brings the login's code back. Another program on this computer, of another
//! user, and a page elsewhere in the browser, are given nothing: not the login page, not the
//! page adopting or joining the server, and not the ticket that page carries, which is added
//! on the way, so it is in no URL or command line.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};

use freya_client::{Http, Login, Pages};
use freya_http::{Request, Response, Server};
use freya_protocol::PAGES_PATH;

/// Where the browser opens the server's pages.
pub const ADDRESS: &str = "127.0.0.1:47477";

/// What keeps a page safe, set by the proxy whatever the server sent: no script runs on it,
/// so a hostile server's page cannot read what a password manager fills in; no other site
/// frames it; and its forms go to its own origin only.
const SAFE_HEADERS: [(&str, &str); 5] = [
    (
        "Content-Security-Policy",
        "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
    ),
    ("X-Frame-Options", "DENY"),
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "same-origin"),
    ("Cache-Control", "no-store"),
];

/// What happened in the browser.
pub enum Heard {
    /// Logged in: to the server pinned, the tokens; or why not.
    LoggedIn(Result<(Option<freya_client::Pin>, freya_client::Granted), String>),
    PasswordChanged,
}

/// The server whose pages are shown, and what is shown: a login waiting for its code, with the
/// ticket that lets it adopt or join the server instead; or the password page, with the
/// server's ticket for it. Each is opened by Freya with a start that works once, which gives
/// the browser that came with it the cookie that alone opens the pages after.
#[derive(Default)]
struct Showing {
    pages: Option<Pages>,
    login: Option<(Http, Login)>,
    claim: Option<String>,
    /// The server's ticket for the password page, while it is shown.
    password: Option<String>,
    /// The page the start sends the browser on to, with its query.
    page: String,
    /// The start Freya opened the pages with, until it was used; and the cookie the browser
    /// that came with it was given.
    start: Option<String>,
    cookie: Option<String>,
}

/// The cookie the pages are opened with.
const COOKIE: &str = "freya-pages";

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

    /// Shows the pages of `server`, waiting for `login` to come back: logged in, or, with a
    /// claim's `ticket`, once the server is adopted or joined. The page to open, once: it
    /// sends the browser on to the login page, or the page adopting or joining the server.
    pub fn claim(&self, server: Http, login: Login, ticket: Option<String>) -> String {
        let page = if ticket.is_some() {
            login.join_page()
        } else {
            login.page()
        };
        let mut showing = self.showing();
        showing.pages = Some(server.pages());
        showing.login = Some((server, login));
        showing.claim = ticket;
        showing.password = None;
        self.open(&mut showing, page)
    }

    /// Shows `pages`, its password page opened with `ticket`, and no login waiting: the page
    /// to open, once.
    pub fn show_password(&self, pages: Pages, ticket: String) -> String {
        let mut showing = self.showing();
        showing.pages = Some(pages);
        showing.login = None;
        showing.claim = None;
        showing.password = Some(ticket);
        self.open(&mut showing, format!("{PAGES_PATH}/password"))
    }

    /// A new start for `page`, in place of any before, which the browser that comes with it
    /// alone is given the cookie for: the page Freya opens.
    fn open(&self, showing: &mut Showing, page: String) -> String {
        let start = random_hex();
        showing.page = page;
        showing.start = Some(start.clone());
        showing.cookie = None;
        format!("{}{PAGES_PATH}/open?start={start}", self.origin())
    }

    /// Waits for no login any more: one that comes back is not taken, and the pages opened for
    /// it are given nothing more.
    pub fn cancel_login(&self) {
        let mut showing = self.showing();
        showing.login = None;
        showing.claim = None;
        showing.start = None;
        showing.cookie = None;
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
        log::info!(
            "the pages' proxy refused {}: asked of {:?}",
            request.path,
            request.header("host")
        );
        return text(403, "Open this page from Freya.");
    }
    let origin = format!("http://{address}");
    // A form is taken only from the pages themselves, as a browser says it sent it.
    if request.method == "POST" && request.header("origin") != Some(origin.as_str()) {
        log::info!(
            "the pages' proxy refused a form for {}: sent from {:?}",
            request.path,
            request.header("origin")
        );
        return text(403, "Open this page from Freya.");
    }
    let open = format!("{PAGES_PATH}/open");
    if (request.method.as_str(), request.path.as_str()) == ("GET", open.as_str()) {
        // Opened by Freya: the browser that came is given the cookie, once.
        let mut showing = showing();
        let started = request
            .query_value("start")
            .zip(showing.start.as_deref())
            .is_some_and(|(given, start)| same(given, start));
        if !started || showing.cookie.is_some() {
            return text(
                403,
                "This page was opened already. Open it again from Freya.",
            );
        }
        let cookie = random_hex();
        showing.cookie = Some(cookie.clone());
        showing.start = None;
        let mut sent = text(303, "");
        sent.headers.extend([
            ("Location", showing.page.clone()),
            (
                "Set-Cookie",
                // Lax, not Strict: a browser started by another program, Freya, may take the
                // page it opens as from another site, and keep a Strict cookie from it. A
                // form from another site is still refused, by its origin, above.
                format!("{COOKIE}={cookie}; HttpOnly; SameSite=Lax; Path=/"),
            ),
        ]);
        return sent;
    }
    // Everything else only for the browser that came with the start.
    let came = {
        let showing = showing();
        showing
            .cookie
            .as_deref()
            .zip(sent_cookie(request).as_deref())
            .is_some_and(|(cookie, sent)| same(cookie, sent))
    };
    if !came {
        let sent = sent_cookie(request).is_some();
        log::info!(
            "the pages' proxy refused {}: {}",
            request.path,
            if sent {
                "the browser sent another page's cookie"
            } else {
                "the browser sent no cookie"
            }
        );
        return text(
            403,
            "This page opens only in the browser Freya opened it in, as it came. Start again from Freya's settings, and go on in the window that opens.",
        );
    }
    if (request.method.as_str(), request.path.as_str()) == ("GET", "/callback") {
        // Taken only by the login's own state, so another page cannot end it.
        let waiting = {
            let mut showing = showing();
            match &showing.login {
                None => None,
                Some((_, login))
                    if !request
                        .query_value("state")
                        .is_some_and(|state| same(state, &login.state)) =>
                {
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
                tell(Heard::LoggedIn(Ok((server.pin().cloned(), tokens))));
                let mut sent = text(303, "");
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
        let Some(ticket) = showing().password.clone() else {
            return text(404, "Open this page from Freya's settings.");
        };
        // Its ticket, added here.
        if request.method == "GET" {
            format!("{}?ticket={ticket}", request.path)
        } else {
            request.path.clone()
        }
    } else if request.method == "GET" && request.path == format!("{PAGES_PATH}/join") {
        // Its ticket, added here.
        let Some(ticket) = showing().claim.clone() else {
            return text(404, "Start again from Freya's settings, with the code.");
        };
        format!("{}?{}&ticket={ticket}", request.path, request.query)
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
        let mut showing = showing();
        showing.password = None;
        showing.cookie = None;
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
    response.headers.extend(safe_headers());
    // Where the server sends the browser on: to its own pages, or back to this proxy with a
    // login's code; nowhere else.
    let location = page
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("location"))
        .map(|(_, to)| to.clone())
        .filter(|to| {
            to.starts_with(&format!("{PAGES_PATH}/"))
                || to.starts_with(&format!("{origin}/callback?"))
        });
    if let Some(to) = location {
        response.headers.push(("Location", to));
    }
    response
}

/// The pages' cookie, as the browser sent it.
fn sent_cookie(request: &Request) -> Option<String> {
    request
        .header("cookie")?
        .split(';')
        .find_map(|pair| pair.trim().strip_prefix(&format!("{COOKIE}=")))
        .map(str::to_owned)
}

/// Whether `given` is `secret`, compared in the same time whatever they are.
fn same(given: &str, secret: &str) -> bool {
    freya_protocol::same_secret(given.as_bytes(), secret.as_bytes())
}

fn random_hex() -> String {
    let mut bytes = [0; 16];
    // The system's randomness failing leaves nothing safe to do.
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .expect("the system's random numbers");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn safe_headers() -> impl Iterator<Item = (&'static str, String)> {
    SAFE_HEADERS
        .iter()
        .map(|(name, value)| (*name, (*value).to_owned()))
}

/// A page of plain text, kept as the server's pages are.
fn text(status: u16, said: &str) -> Response {
    let mut response = Response::bytes(status, "text/plain; charset=utf-8", said.into());
    response.headers.extend(safe_headers());
    response
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

    /// The cookie `answer` set, as the browser sends it back.
    fn cookie_of(answer: &ureq::http::Response<ureq::Body>) -> String {
        let set = header(answer, "set-cookie");
        assert!(
            set.contains("HttpOnly") && set.contains("SameSite=Lax"),
            "{set}"
        );
        set.split(';').next().unwrap().to_owned()
    }

    /// A claim ticket of `accounts`, for the server with `certificate`, proving its adoption
    /// code as a device typing it does.
    fn adoption_ticket(accounts: &Accounts, certificate: &[u8]) -> String {
        let (code, _) = accounts.adoption_code().unwrap();
        let digest = ring::digest::digest(&ring::digest::SHA256, certificate);
        let key = freya_protocol::connection_key(&code, digest.as_ref());
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key);
        let nonce = [3; 32];
        let proof = ring::hmac::sign(
            &key,
            &freya_protocol::claim_message(&nonce, digest.as_ref()),
        );
        let (ticket, claim) = accounts.claim(&nonce, proof.as_ref(), certificate).unwrap();
        assert_eq!(claim, freya_protocol::Claim::Adopt);
        ticket
    }

    #[test]
    fn logs_in_and_changes_the_password_in_the_browser() {
        let dir = std::env::temp_dir().join(format!("freya-page-proxy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let here = free.local_addr().unwrap().to_string();
        drop(free);
        let accounts = Arc::new(Accounts::open(None));
        accounts
            .add_user("Ada", freya_protocol::Role::Admin, "battery staple")
            .unwrap();
        let certificate = b"the server's certificate".to_vec();
        let served = Served {
            library: Arc::new(freya_server::LocalLibrary::new(
                Library::open_in_memory().unwrap(),
                None,
            )),
            server: None,
            accounts: Some(Arc::clone(&accounts)),
            certificate: Some(certificate.clone()),
            callback_port: here.rsplit_once(':').unwrap().1.parse().unwrap(),
            updates: None,
        };
        let server = Server::start("127.0.0.1:0", freya_server::handler(served)).unwrap();
        let address = format!("http://{}", server.address());
        let (redirect, verifier) = (
            "http://127.0.0.1:1/callback",
            "a secret long enough to be one",
        );
        let code = accounts
            .grant(
                "Ada",
                "battery staple",
                &freya_server::accounts::challenge(verifier),
                redirect,
                None,
            )
            .unwrap();
        let mac = Http::new(&address).with_token(Some(
            accounts
                .redeem(&code, verifier, redirect, "Mac")
                .unwrap()
                .access,
        ));
        assert!(mac.account().unwrap().unwrap().has_password);

        let (tell, heard) = channel();
        let proxy = PageProxy::start_at(&here, tell).unwrap();
        let origin = proxy.origin();
        let browser: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .into();
        let wait = || heard.recv_timeout(Duration::from_secs(10)).unwrap();

        // The password, changed on the page Freya opened.
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
        assert_eq!(header(&started, "location"), "/web/password");
        let cookie = cookie_of(&started);
        assert!(cookie.starts_with("freya-pages="));
        assert_eq!(browser.get(&page).call().unwrap().status(), 403, "once");
        let shown = browser
            .get(&format!("{origin}/web/password"))
            .header("Cookie", cookie.as_str())
            .call()
            .unwrap();
        assert_eq!(shown.status(), 200);
        let form = format!(
            "ticket={ticket}&current=battery+staple&password=correct+horse&again=correct+horse"
        );
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
        // Done with: the cookie opens nothing more.
        let again = browser
            .get(&format!("{origin}/web/password"))
            .header("Cookie", cookie.as_str())
            .call()
            .unwrap();
        assert_eq!(again.status(), 403);

        // Logging in: the page sends the browser back with a code, which Freya trades for a
        // token before it says so. Only the browser Freya opened sees the page or sends it.
        let login = Login::new(&proxy.callback());
        let page = login.page();
        let start = proxy.claim(Http::new(&address), login, None);
        assert!(
            !start.contains("code_challenge"),
            "the start is only a start"
        );
        let unopened = browser.get(&format!("{origin}{page}")).call().unwrap();
        assert_eq!(unopened.status(), 403, "not before the start");
        let started = browser.get(&start).call().unwrap();
        assert_eq!(started.status(), 303);
        assert_eq!(header(&started, "location"), page);
        let cookie = cookie_of(&started);
        let shown = browser
            .get(&format!("{origin}{page}"))
            .header("Cookie", cookie.as_str())
            .call()
            .unwrap();
        assert_eq!(shown.status(), 200);
        assert_eq!(header(&shown, "x-frame-options"), "DENY");
        let opened = page.split_once('?').unwrap().1;
        let typed = format!("{opened}&username=Ada&password=correct+horse");
        let without = browser
            .post(&format!("{origin}/web/login"))
            .header("Content-Type", FORM)
            .header("Origin", origin.as_str())
            .send(typed.clone())
            .unwrap();
        assert_eq!(without.status(), 403, "not without the cookie");
        let sent = browser
            .post(&format!("{origin}/web/login"))
            .header("Content-Type", FORM)
            .header("Origin", origin.as_str())
            .header("Cookie", cookie.as_str())
            .send(typed)
            .unwrap();
        assert_eq!(sent.status(), 303);
        let back = header(&sent, "location").to_owned();
        assert!(back.starts_with(&format!("{origin}/callback?code=")));
        // Another page sending the browser back with another state ends nothing, nor one
        // without the cookie.
        let forged = browser
            .get(&format!("{origin}/callback?code=x&state=forged"))
            .header("Cookie", cookie.as_str())
            .call()
            .unwrap();
        assert_eq!(forged.status(), 400);
        assert_eq!(browser.get(&back).call().unwrap().status(), 403);
        let done = browser
            .get(&back)
            .header("Cookie", cookie.as_str())
            .call()
            .unwrap();
        assert_eq!(done.status(), 303);
        assert_eq!(header(&done, "location"), "/web/logged-in");
        let Heard::LoggedIn(Ok((_, tokens))) = wait() else {
            panic!("not logged in");
        };
        let pc = Http::new(&address).with_token(Some(tokens.access));
        assert_eq!(pc.account().unwrap().unwrap().devices.len(), 2);
        let logged_in = browser
            .get(&format!("{origin}/web/logged-in"))
            .header("Cookie", cookie.as_str())
            .call()
            .unwrap();
        assert_eq!(logged_in.status(), 200);
        // Once.
        let once = browser
            .get(&back)
            .header("Cookie", cookie.as_str())
            .call()
            .unwrap();
        assert_eq!(once.status(), 404);

        // Adopting the server, with a claim's ticket: added to the page only for the browser
        // Freya opened, so another program on this computer cannot learn it and adopt the
        // server itself.
        assert!(!accounts.adopted());
        let ticket = adoption_ticket(&accounts, &certificate);
        let login = Login::new(&proxy.callback());
        let page = login.join_page();
        let start = proxy.claim(Http::new(&address), login, Some(ticket.clone()));
        let unopened = browser.get(&format!("{origin}{page}")).call().unwrap();
        assert_eq!(unopened.status(), 403);
        let old = browser
            .get(&format!("{origin}{page}"))
            .header("Cookie", cookie.as_str())
            .call()
            .unwrap();
        assert_eq!(old.status(), 403, "the login's cookie is not the claim's");
        let started = browser.get(&start).call().unwrap();
        let cookie = cookie_of(&started);
        let mut shown = browser
            .get(&format!("{origin}{page}"))
            .header("Cookie", cookie.as_str())
            .call()
            .unwrap();
        assert_eq!(shown.status(), 200);
        let body = shown.body_mut().read_to_string().unwrap();
        assert!(body.contains(&ticket), "the page carries the ticket");
        assert!(body.contains("Adopt"), "{body}");
        let opened = page.split_once('?').unwrap().1;
        let typed = format!(
            "{opened}&ticket={ticket}&username=Ada+Owner&password=correct+horse&again=correct+horse"
        );
        let sent = browser
            .post(&format!("{origin}/web/join"))
            .header("Content-Type", FORM)
            .header("Origin", origin.as_str())
            .header("Cookie", cookie.as_str())
            .send(typed)
            .unwrap();
        assert_eq!(sent.status(), 303);
        let back = header(&sent, "location").to_owned();
        let done = browser
            .get(&back)
            .header("Cookie", cookie.as_str())
            .call()
            .unwrap();
        assert_eq!(done.status(), 303);
        let Heard::LoggedIn(Ok((_, tokens))) = wait() else {
            panic!("not adopted");
        };
        assert!(accounts.adopted());
        let owner = Http::new(&address).with_token(Some(tokens.access));
        assert!(owner.account().unwrap().unwrap().owner);

        // Cancelled, the pages are given nothing more.
        let login = Login::new(&proxy.callback());
        let start = proxy.claim(Http::new(&address), login, None);
        let cookie = cookie_of(&browser.get(&start).call().unwrap());
        proxy.cancel_login();
        let cancelled = browser
            .get(&format!("{origin}/web/login"))
            .header("Cookie", cookie.as_str())
            .call()
            .unwrap();
        assert_eq!(cancelled.status(), 403);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
