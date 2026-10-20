//! The server the library is on, as the user chooses it: this computer's, or one on another
//! computer that they find on the network or type in, and log in to, its certificate pinned;
//! and the account this computer is connected to. Asking the network happens on threads, so
//! the window never waits for it.

use std::net::SocketAddr;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use freya_client::{Found, Http, Pin};
use freya_protocol::{AccountInfo, Remote, ServerFound};
use slint_interpreter::{Struct, Value};

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
    LoggedIn(Result<(Pin, String, String), String>),
}

/// A server the user logged in to, for the app to show the library of.
pub struct Chosen {
    pub pin: Pin,
    pub name: String,
    pub token: String,
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
    /// The account and the server, as last read.
    account: Option<AccountInfo>,
    info: Option<ServerFound>,
    password_error: String,
    done: (Sender<Done>, Receiver<Done>),
    pub device_rows: ModelRows,
    pub found_rows: ModelRows,
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
            account: None,
            info: None,
            password_error: String::new(),
            done: channel(),
            device_rows: ModelRows::new(),
            found_rows: ModelRows::new(),
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

    /// Logs in to the server chosen, as `user` with `password`.
    pub fn log_in(&mut self, user: &str, password: &str) {
        let Some(pending) = &self.login else {
            return;
        };
        if self.login_busy {
            return;
        }
        let (pin, name) = (pending.pin.clone(), pending.name.clone());
        let (user, password) = (user.trim().to_owned(), password.to_owned());
        self.login_busy = true;
        self.login_error.clear();
        self.spawn(move || {
            let device = crate::settings::device_name();
            let logged_in = Http::pinned(pin.clone())
                .login(&user, &password, &device)
                .map(|token| (pin, name, token))
                .map_err(|e| e.to_string());
            Done::LoggedIn(logged_in)
        });
    }

    pub fn cancel_login(&mut self) {
        self.login = None;
        self.login_busy = false;
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
                    self.login = Some(pending);
                }
                Done::Certificate(Err(e)) => {
                    self.login_busy = false;
                    self.login_error = e;
                }
                Done::LoggedIn(Ok((pin, name, token))) => {
                    self.cancel_login();
                    return Some(Chosen { pin, name, token });
                }
                Done::LoggedIn(Err(e)) => {
                    self.login_busy = false;
                    self.login_error = e;
                }
            }
        }
        None
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

    pub fn set_password(&mut self, remote: Option<&Remote<Http>>, password: &str) {
        let Some(remote) = remote else {
            return;
        };
        match remote.0.set_password(password) {
            Ok(()) => self.password_error.clear(),
            Err(e) => self.password_error = e.to_string(),
        }
        self.refresh(Some(remote));
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
        let info = self.info.as_ref();
        vec![
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
    let connected = jiff::Timestamp::try_from(device.created)
        .map(|at| at.to_zoned(jiff::tz::TimeZone::system()).date().to_string())
        .unwrap_or_default();
    Value::Struct(Struct::from_iter([
        ("id".to_owned(), string(&device.id)),
        ("name".to_owned(), string(&device.name)),
        ("connected".to_owned(), string(connected)),
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
