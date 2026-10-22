//! The server the library is on, as the user chooses it: this computer's, or one on another
//! computer that they find on the network or type in, prove with the connection code its
//! computer shows, and log in to in the browser, its certificate pinned; and the account this computer is connected to, its password changed in
//! the browser too. Asking the network happens on threads, so the window never waits for it.

use std::net::SocketAddr;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, SystemTime};

use freya_client::{Found, Http, Login, Pin};
use std::path::{Path, PathBuf};

use freya_protocol::{AccountInfo, FolderListing, Remote, ServerControl, ServerFound};
use slint_interpreter::{Struct, Value};

use super::page_proxy::{ADDRESS, Heard, PageProxy};
use super::skin_state::{ModelRows, string};

/// How long Freya listens for servers answering on the network.
const FINDING: Duration = Duration::from_millis(1500);

/// A server the user is logging in to, its certificate taken to pin.
struct Pending {
    pin: Pin,
    name: String,
}

/// What a thread came back with.
enum Done {
    Found(Vec<Found>),
    Certificate(Result<(Pending, String), String>),
    /// Whether the server knew the connection code typed.
    Verified(Result<(), String>),
}

/// A server the user logged in to, for the app to show the library of.
pub struct Chosen {
    pub pin: Pin,
    pub name: String,
    pub tokens: freya_client::Granted,
}

pub struct ServerPanel {
    finding: bool,
    found: Vec<Found>,
    /// While logging in: the server, its certificate's fingerprint, whether Freya is asking it
    /// now, and what went wrong.
    login: Option<Pending>,
    login_fingerprint: String,
    login_busy: bool,
    login_error: String,
    /// Whether the server chosen proved it knows the connection code, or is the one this
    /// computer knew already, by its certificate.
    login_verified: bool,
    known_certificate: Option<Vec<u8>>,
    /// The connection code shown on this computer, for a new device, and until when.
    connection_code: Option<(String, SystemTime)>,
    /// While the login page or the password page is open in the browser.
    login_waiting: bool,
    password_waiting: bool,
    /// The account and the server, as last read, and whether they want reading again.
    account: Option<AccountInfo>,
    info: Option<ServerFound>,
    refresh_wanted: bool,
    password_error: String,
    /// The server's pages in the browser, once opened, and what happens there.
    proxy: Option<PageProxy>,
    heard: (Sender<Heard>, Receiver<Heard>),
    /// While choosing a music folder among the server's: the folder shown and those in it.
    browse: Option<FolderListing>,
    done: (Sender<Done>, Receiver<Done>),
    pub device_rows: ModelRows,
    pub found_rows: ModelRows,
    pub browse_rows: ModelRows,
}

impl ServerPanel {
    pub fn new() -> Self {
        Self {
            finding: false,
            found: Vec::new(),
            login: None,
            login_fingerprint: String::new(),
            login_busy: false,
            login_error: String::new(),
            login_verified: false,
            known_certificate: None,
            connection_code: None,
            login_waiting: false,
            password_waiting: false,
            account: None,
            info: None,
            refresh_wanted: false,
            password_error: String::new(),
            proxy: None,
            heard: channel(),
            browse: None,
            done: channel(),
            device_rows: ModelRows::new(),
            found_rows: ModelRows::new(),
            browse_rows: ModelRows::new(),
        }
    }

    /// Runs `work` on a thread, its answer taken in by [`Self::poll`].
    fn spawn(&self, work: impl FnOnce() -> Done + Send + 'static) {
        let tell = self.done.0.clone();
        let started = std::thread::Builder::new()
            .name("freya-server-panel".into())
            .spawn(move || {
                let _ = tell.send(work());
            });
        if let Err(e) = started {
            log::warn!("could not ask the network: {e}");
        }
    }

    /// Looks for servers on the network.
    pub fn find(&mut self) {
        if self.finding {
            return;
        }
        self.finding = true;
        self.spawn(|| Done::Found(freya_client::find_servers(FINDING)));
    }

    /// Starts logging in to the `index`th server found.
    pub fn choose_found(&mut self, index: usize) {
        if let Some(found) = self.found.get(index) {
            let (address, name) = (found.address, found.server.name.clone());
            self.take_certificate(Ok(address), name);
        }
    }

    /// Starts logging in to the server at `typed`: "mini.local", "192.168.1.20:47475".
    pub fn choose_address(&mut self, typed: &str) {
        let typed = typed.trim().to_owned();
        self.login_busy = true;
        self.login_error.clear();
        self.spawn(move || {
            let address = freya_client::resolve(&typed);
            Done::Certificate(certificate(address, typed))
        });
    }

    fn take_certificate(&mut self, address: Result<SocketAddr, String>, name: String) {
        self.login_busy = true;
        self.login_error.clear();
        self.spawn(move || Done::Certificate(certificate(address, name)));
    }

    /// The certificate of the server this computer uses, once it knew it: one that gives the
    /// same is known without a code, at whatever address.
    pub fn set_known(&mut self, certificate: Option<Vec<u8>>) {
        self.known_certificate = certificate;
    }

    /// Asks the server chosen to prove it knows the connection code `typed`.
    pub fn verify_code(&mut self, typed: &str) {
        let Some(pin) = self.login.as_ref().map(|pending| pending.pin.clone()) else {
            return;
        };
        if self.login_busy {
            return;
        }
        let typed = typed.to_owned();
        self.login_busy = true;
        self.login_error.clear();
        self.spawn(move || Done::Verified(freya_client::verify_server(&pin, &typed)));
    }

    /// Shows a new connection code for a device to type, made by the server on this computer.
    pub fn show_connection_code(&mut self, remote: Option<&Remote<Http>>) {
        let Some(remote) = remote else {
            return;
        };
        match remote.0.connection_code() {
            Ok((code, lasts)) => {
                self.connection_code =
                    Some((freya_protocol::show_code(&code), SystemTime::now() + lasts));
            }
            Err(e) => log::warn!("could not make a connection code: {e}"),
        }
    }

    /// The server's pages in the browser, opened the first time.
    fn proxy(&mut self) -> Result<&PageProxy, String> {
        if self.proxy.is_none() {
            let proxy = PageProxy::start(self.heard.0.clone()).map_err(|e| {
                format!("Freya could not open the server's pages at {ADDRESS}: {e}")
            })?;
            self.proxy = Some(proxy);
        }
        self.proxy
            .as_ref()
            .ok_or_else(|| "Freya could not open the server's pages.".to_owned())
    }

    /// Opens the login page of the server chosen in the browser; Freya is logged in once the
    /// user is there.
    pub fn log_in(&mut self) {
        let Some(pin) = self.login.as_ref().map(|pending| pending.pin.clone()) else {
            return;
        };
        if !self.login_verified {
            return;
        }
        self.login_error.clear();
        let page = match self.proxy() {
            Ok(proxy) => {
                let login = Login::new(&proxy.callback());
                let page = format!("{}{}", proxy.origin(), login.page());
                proxy.log_in(Http::pinned(pin), login);
                page
            }
            Err(e) => {
                self.login_error = e;
                return;
            }
        };
        crate::update::open_page(&page);
        self.login_waiting = true;
    }

    pub fn cancel_login(&mut self) {
        self.login = None;
        self.login_verified = false;
        self.login_busy = false;
        self.login_waiting = false;
        self.login_error.clear();
    }

    /// Takes in what the threads came back with; a server the user logged in to.
    pub fn poll(&mut self) -> Option<Chosen> {
        while let Ok(done) = self.done.1.try_recv() {
            match done {
                Done::Found(found) => {
                    self.finding = false;
                    self.found = found;
                }
                Done::Certificate(Ok((pending, fingerprint))) => {
                    self.login_busy = false;
                    self.login_fingerprint = fingerprint;
                    self.login_verified =
                        self.known_certificate.as_ref() == Some(&pending.pin.certificate);
                    self.login = Some(pending);
                }
                Done::Verified(verified) => {
                    self.login_busy = false;
                    match verified {
                        Ok(()) => self.login_verified = true,
                        Err(e) => self.login_error = e,
                    }
                }
                Done::Certificate(Err(e)) => {
                    self.login_busy = false;
                    self.login_error = e;
                }
            }
        }
        while let Ok(heard) = self.heard.1.try_recv() {
            match heard {
                Heard::LoggedIn(Ok(tokens)) => {
                    let Some(pending) = self.login.take() else {
                        continue;
                    };
                    self.cancel_login();
                    return Some(Chosen {
                        pin: pending.pin,
                        name: pending.name,
                        tokens,
                    });
                }
                Heard::LoggedIn(Err(e)) => {
                    self.login_waiting = false;
                    self.login_error = e;
                }
                Heard::PasswordChanged => {
                    self.password_waiting = false;
                    self.refresh_wanted = true;
                }
            }
        }
        None
    }

    /// Whether the account wants reading again, as after its password changed.
    pub fn take_refresh_wanted(&mut self) -> bool {
        std::mem::take(&mut self.refresh_wanted)
    }

    /// Reads the account and the server's own information again.
    pub fn refresh(&mut self, remote: Option<&Remote<Http>>) {
        let Some(remote) = remote else {
            self.account = None;
            self.info = None;
            return;
        };
        self.account = remote.0.account().ok().flatten();
        self.info = remote.0.info().ok();
    }

    /// Opens the page the account's password is set or changed on in the browser.
    pub fn change_password(&mut self, remote: Option<&Remote<Http>>) {
        let Some(remote) = remote else {
            return;
        };
        self.password_error.clear();
        let page = match remote.0.password_page() {
            Ok(page) => page,
            Err(e) => {
                self.password_error = e.to_string();
                return;
            }
        };
        let page = match self.proxy() {
            Ok(proxy) => {
                proxy.show(remote.0.pages());
                format!("{}{page}", proxy.origin())
            }
            Err(e) => {
                self.password_error = e;
                return;
            }
        };
        crate::update::open_page(&page);
        self.password_waiting = true;
    }

    /// Takes this computer's token on `remote` back.
    pub fn log_out(&mut self, remote: Option<&Remote<Http>>) {
        if let Some(remote) = remote
            && let Err(e) = remote.0.log_out()
        {
            log::warn!("could not take this computer's token back: {e}");
        }
        self.account = None;
        self.info = None;
    }

    pub fn revoke(&mut self, remote: Option<&Remote<Http>>, device: &str) {
        let Some(remote) = remote else {
            return;
        };
        if let Err(e) = remote.0.revoke(device) {
            log::warn!("could not take the device's token back: {e}");
        }
        self.refresh(Some(remote));
    }

    /// Shows the folders in `folder` on the server, or where to start when none.
    pub fn browse(&mut self, remote: Option<&Remote<Http>>, folder: Option<&Path>) {
        let Some(remote) = remote else {
            return;
        };
        match remote.server_folders(folder) {
            Ok(listing) => self.browse = Some(listing),
            Err(e) => log::warn!("could not list the server's folders: {e}"),
        }
    }

    /// The folder shown, the `index`th in it, or the one it is in.
    pub fn browsed(&self) -> Option<PathBuf> {
        self.browse.as_ref()?.folder.clone()
    }

    pub fn browsed_into(&self, index: usize) -> Option<PathBuf> {
        self.browse.as_ref()?.folders.get(index).cloned()
    }

    /// The folder the one shown is in; none at the start, where it goes back to.
    pub fn browsed_up(&self) -> Option<PathBuf> {
        self.browse.as_ref()?.parent.clone()
    }

    pub fn stop_browsing(&mut self) {
        self.browse = None;
    }

    /// `Server`'s properties but where the library is and how it goes.
    pub fn values(&mut self) -> Vec<(&'static str, Value)> {
        let account = self.account.as_ref();
        self.device_rows.sync(
            account
                .map(|a| a.devices.iter().map(device_row).collect())
                .unwrap_or_default(),
        );
        self.found_rows
            .sync(self.found.iter().map(found_row).collect());
        let browse = self.browse.as_ref();
        self.browse_rows.sync(
            browse
                .map(|b| {
                    b.folders
                        .iter()
                        .map(|f| {
                            // Where to start, whole paths; inside a folder, names.
                            let shown = match &b.folder {
                                Some(_) => f.file_name().map(|n| n.to_string_lossy().into_owned()),
                                None => None,
                            };
                            string(shown.unwrap_or_else(|| f.to_string_lossy().into_owned()))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        );
        let info = self.info.as_ref();
        // Shown only while it lasts.
        if self
            .connection_code
            .as_ref()
            .is_some_and(|(_, until)| *until <= SystemTime::now())
        {
            self.connection_code = None;
        }
        let code = self.connection_code.clone();
        let code_until = code
            .as_ref()
            .and_then(|(_, until)| jiff::Timestamp::try_from(*until).ok())
            .map(|at| {
                at.to_zoned(jiff::tz::TimeZone::system())
                    .strftime("%H:%M")
                    .to_string()
            })
            .unwrap_or_default();
        vec![
            ("browsing", Value::Bool(browse.is_some())),
            (
                "browse-folder",
                string(
                    browse
                        .and_then(|b| b.folder.as_ref())
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ),
            ),
            (
                "fingerprint",
                string(info.map(|i| i.fingerprint.clone()).unwrap_or_default()),
            ),
            (
                "server-name",
                string(info.map(|i| i.name.clone()).unwrap_or_default()),
            ),
            (
                "user",
                string(account.map(|a| a.user.clone()).unwrap_or_default()),
            ),
            (
                "has-password",
                Value::Bool(account.is_some_and(|a| a.has_password)),
            ),
            ("password-error", string(&self.password_error)),
            ("password-waiting", Value::Bool(self.password_waiting)),
            ("finding", Value::Bool(self.finding)),
            ("logging-in", Value::Bool(self.login.is_some())),
            (
                "login-address",
                string(
                    self.login
                        .as_ref()
                        .map(|l| l.pin.address.to_string())
                        .unwrap_or_default(),
                ),
            ),
            ("login-fingerprint", string(&self.login_fingerprint)),
            ("login-busy", Value::Bool(self.login_busy)),
            ("login-waiting", Value::Bool(self.login_waiting)),
            ("login-verified", Value::Bool(self.login_verified)),
            (
                "connection-code",
                string(
                    code.as_ref()
                        .map(|(code, _)| code.clone())
                        .unwrap_or_default(),
                ),
            ),
            ("connection-code-until", string(code_until)),
            ("login-error", string(&self.login_error)),
        ]
    }
}

/// The certificate of the server at `address`, to pin, and its fingerprint.
fn certificate(
    address: Result<SocketAddr, String>,
    name: String,
) -> Result<(Pending, String), String> {
    let address = address?;
    let certificate = freya_client::certificate_of(address)
        .map_err(|e| format!("Could not reach the server at {address}: {e}"))?;
    let fingerprint = freya_server::tls::fingerprint(&certificate);
    let pin = Pin {
        address,
        certificate,
    };
    Ok((Pending { pin, name }, fingerprint))
}

fn device_row(device: &freya_protocol::DeviceInfo) -> Value {
    let date = |at: std::time::SystemTime| {
        jiff::Timestamp::try_from(at)
            .map(|at| at.to_zoned(jiff::tz::TimeZone::system()).date().to_string())
            .unwrap_or_default()
    };
    let joined = match device.joined {
        Some(freya_protocol::Joined::Paired) => "paired",
        Some(freya_protocol::Joined::LoggedIn) => "logged-in",
        None => "",
    };
    Value::Struct(Struct::from_iter([
        ("id".to_owned(), string(&device.id)),
        ("name".to_owned(), string(&device.name)),
        ("connected".to_owned(), string(date(device.created))),
        (
            "used".to_owned(),
            string(device.used.map(date).unwrap_or_default()),
        ),
        ("joined".to_owned(), string(joined)),
        ("this".to_owned(), Value::Bool(device.this)),
    ]))
}

fn found_row(found: &Found) -> Value {
    Value::Struct(Struct::from_iter([
        ("name".to_owned(), string(&found.server.name)),
        ("address".to_owned(), string(found.address.to_string())),
        ("fingerprint".to_owned(), string(&found.server.fingerprint)),
    ]))
}
