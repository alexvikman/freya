//! The server the library is on, as the user chooses it: this computer's, or one on another
//! computer that they find on the network or type in, prove with the connection code its
//! computer shows, and log in to in the browser, its certificate pinned; and the account this computer is connected to, its password changed in
//! the browser too. Asking the network happens on threads, so the window never waits for it.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, SystemTime};

use freya_client::{Found, Http, Login, Pin};
use std::path::{Path, PathBuf};

use freya_protocol::{
    AccountInfo, Claim, FolderListing, Remote, Role, ServerControl, ServerFound, UserInfo,
};
use slint_interpreter::{Struct, Value};

use super::page_proxy::{ADDRESS, Heard, PageProxy};
use super::skin_state::{ModelRows, string};

/// The most servers found that are shown: a device on the network answering for many cannot
/// fill the list.
const MOST_FOUND: usize = 32;

/// How long Freya listens for servers answering on the network.
const FINDING: Duration = Duration::from_millis(1500);

/// A server the user is logging in to, its certificate taken to pin.
struct Pending {
    pin: Pin,
    name: String,
}

/// Freya Server on this computer, to log in to: the server, its certificate's fingerprint,
/// and, while it is not adopted, the ticket of its claim.
type ThisComputer = (Pending, String, Option<(String, Claim)>);

/// What a thread came back with.
enum Done {
    Found(Vec<Found>),
    Certificate(Result<(Pending, String), String>),
    /// Whether the server with this certificate knew the code typed, and what this device may
    /// do with it: adopt or join the server, with the ticket for that page.
    Verified(Vec<u8>, Result<Option<(String, Claim)>, String>),
    /// The server's users, for an admin.
    Users(Result<Vec<UserInfo>, String>),
    /// A user invited, and the code they join with, until when; or why not.
    Invited(Result<(String, String, SystemTime), String>),
    /// A user taken away, or why not.
    Removed(Result<(), String>),
    /// How updating the server goes, for an admin.
    Update(Result<freya_protocol::ServerUpdate, String>),
    /// Freya Server on this computer, as its folder makes it known: proved, and what this
    /// device may do with it.
    ThisComputer(Result<ThisComputer, String>),
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
    /// What the code typed lets this device do instead of logging in: adopt the server, or
    /// join it as the user invited, with the ticket for the page.
    claim: Option<(String, Claim)>,
    /// The server's users, for an admin; the code a user was just invited with, whom for, and
    /// until when; and why inviting or taking one away failed.
    users: Vec<UserInfo>,
    invite: Option<(String, String, SystemTime)>,
    invite_error: String,
    /// How updating the server goes, for an admin, and when that was read.
    update: Option<freya_protocol::ServerUpdate>,
    update_read: Option<std::time::Instant>,
    /// The connection code shown on this computer, for a new device, and until when; why
    /// none could be made.
    connection_code: Option<(String, SystemTime)>,
    connection_code_error: String,
    /// While the login page or the password page is open in the browser.
    login_waiting: bool,
    password_waiting: bool,
    /// The account and the server, as last read, and whether they want reading again.
    account: Option<AccountInfo>,
    info: Option<ServerFound>,
    fingerprint: String,
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
    pub user_rows: ModelRows,
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
            claim: None,
            users: Vec::new(),
            invite: None,
            invite_error: String::new(),
            update: None,
            update_read: None,
            connection_code: None,
            connection_code_error: String::new(),
            login_waiting: false,
            password_waiting: false,
            account: None,
            info: None,
            fingerprint: String::new(),
            refresh_wanted: false,
            password_error: String::new(),
            proxy: None,
            heard: channel(),
            browse: None,
            done: channel(),
            device_rows: ModelRows::new(),
            user_rows: ModelRows::new(),
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
            let claimed = found.server.fingerprint.clone();
            self.take_certificate(Ok(address), name, Some(claimed));
        }
    }

    /// Starts logging in to the server at `typed`: "mini.local", "192.168.1.20:47475".
    pub fn choose_address(&mut self, typed: &str) {
        if self.login_busy {
            return;
        }
        let typed = typed.trim().to_owned();
        self.login_busy = true;
        self.login_error.clear();
        self.spawn(move || {
            let address = freya_client::resolve(&typed);
            Done::Certificate(certificate(address, typed, None))
        });
    }

    /// Takes the certificate of the server at `address`, which said it has the one of
    /// `claimed` when it was found.
    fn take_certificate(
        &mut self,
        address: Result<SocketAddr, String>,
        name: String,
        claimed: Option<String>,
    ) {
        if self.login_busy {
            return;
        }
        self.login_busy = true;
        self.login_error.clear();
        self.spawn(move || Done::Certificate(certificate(address, name, claimed)));
    }

    /// Starts logging in to Freya Server on this computer, as to any other, without a code
    /// typed: its certificate, and while it is not adopted the code it is adopted with, are read
    /// from its folder, which only its computer's user can read.
    pub fn choose_this_computer(&mut self) {
        if self.login_busy {
            return;
        }
        self.login_busy = true;
        self.login_error.clear();
        self.spawn(|| Done::ThisComputer(this_computer()));
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
        self.spawn(move || {
            let verified = freya_client::verify_server(&pin, &typed);
            Done::Verified(pin.certificate, verified)
        });
    }

    /// Shows a new connection code for a device to type, made by the server on this computer.
    pub fn show_connection_code(&mut self, remote: Option<&Remote<Http>>) {
        let Some(remote) = remote else {
            self.connection_code_error = "Freya is not connected to its server.".into();
            return;
        };
        match remote.0.connection_code() {
            Ok((code, lasts)) => {
                self.connection_code =
                    Some((freya_protocol::show_code(&code), SystemTime::now() + lasts));
                self.connection_code_error.clear();
            }
            Err(e) => {
                log::warn!("could not make a connection code: {e}");
                self.connection_code_error = format!("No code could be made: {e}");
            }
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
        let claim = self.claim.clone();
        let page = match self.proxy() {
            Ok(proxy) => {
                let login = Login::new(&proxy.callback());
                let (page, ticket) = match claim {
                    // Adopting or joining the server: its page logs in too.
                    Some((ticket, _)) => (login.join_page(), Some(ticket)),
                    None => (login.page(), None),
                };
                let page = format!("{}{page}", proxy.origin());
                proxy.claim(Http::pinned(pin), login, ticket);
                page
            }
            Err(e) => {
                self.login_error = e;
                return;
            }
        };
        crate::update::open_page_privately(&page);
        self.login_waiting = true;
    }

    pub fn cancel_login(&mut self) {
        if let Some(proxy) = &self.proxy {
            proxy.cancel_login();
        }
        self.login = None;
        self.login_verified = false;
        self.claim = None;
        self.login_busy = false;
        self.login_waiting = false;
        self.login_error.clear();
    }

    /// Takes in what the threads came back with; a server the user logged in to.
    pub fn poll(&mut self) -> Option<Chosen> {
        while let Ok(done) = self.done.1.try_recv() {
            match done {
                Done::Found(mut found) => {
                    self.finding = false;
                    found.truncate(MOST_FOUND);
                    self.found = found;
                }
                Done::Certificate(Ok((pending, fingerprint))) => {
                    self.login_busy = false;
                    self.login_fingerprint = fingerprint;
                    self.login_verified =
                        self.known_certificate.as_ref() == Some(&pending.pin.certificate);
                    self.login = Some(pending);
                }
                Done::Verified(certificate, verified) => {
                    self.login_busy = false;
                    // Only for the server chosen now, not one chosen before it.
                    let chosen = self.login.as_ref().map(|pending| &pending.pin.certificate);
                    match verified {
                        Ok(claim) if chosen == Some(&certificate) => {
                            self.login_verified = true;
                            self.claim = claim;
                        }
                        Ok(_) => {}
                        Err(e) => self.login_error = e,
                    }
                }
                Done::Certificate(Err(e)) => {
                    self.login_busy = false;
                    self.login_error = e;
                }
                Done::Users(Ok(users)) => self.users = users,
                Done::Users(Err(e)) => log::warn!("could not read the server's users: {e}"),
                Done::Invited(Ok(invite)) => {
                    self.invite = Some(invite);
                    self.invite_error.clear();
                    self.refresh_wanted = true;
                }
                Done::Invited(Err(e)) | Done::Removed(Err(e)) => self.invite_error = e,
                Done::Removed(Ok(())) => {
                    self.invite_error.clear();
                    self.refresh_wanted = true;
                }
                Done::ThisComputer(Ok((pending, fingerprint, claim))) => {
                    self.login_busy = false;
                    self.login_fingerprint = fingerprint;
                    self.login_verified = true;
                    self.claim = claim;
                    self.login = Some(pending);
                }
                Done::ThisComputer(Err(e)) => {
                    self.login_busy = false;
                    self.login_error = e;
                }
                Done::Update(Ok(update)) => self.update = Some(update),
                Done::Update(Err(e)) => {
                    log::debug!("could not read how updating the server goes: {e}");
                    self.update = None;
                }
            }
        }
        while let Ok(heard) = self.heard.1.try_recv() {
            match heard {
                Heard::LoggedIn(Ok((pin, tokens))) => {
                    let Some(pending) = self.login.take() else {
                        continue;
                    };
                    self.cancel_login();
                    // The tokens are the server's that gave them, and go to no other chosen since.
                    if pin.as_ref() != Some(&pending.pin) {
                        log::warn!("a login came back for another server than the one chosen");
                        continue;
                    }
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

    /// Reads the server's users again, on a thread, when the account is an admin's.
    pub fn load_users(&mut self, remote: Option<Arc<Remote<Http>>>) {
        let admin = self.account.as_ref().is_some_and(|a| a.role == Role::Admin);
        let Some(remote) = remote.filter(|_| admin) else {
            self.users.clear();
            return;
        };
        self.spawn(move || Done::Users(remote.0.users().map_err(|e| e.to_string())));
    }

    /// Reads how updating the server goes, on a thread, when the account is an admin's: once a
    /// minute, and every two seconds while the server looks or downloads.
    pub fn load_update(&mut self, remote: Option<Arc<Remote<Http>>>) {
        let admin = self.account.as_ref().is_some_and(|a| a.role == Role::Admin);
        let Some(remote) = remote.filter(|_| admin) else {
            self.update = None;
            return;
        };
        let busy = self
            .update
            .as_ref()
            .is_some_and(|u| matches!(u.state.as_str(), "checking" | "downloading" | "installed"));
        let every = std::time::Duration::from_secs(if busy { 2 } else { 60 });
        if self.update_read.is_some_and(|at| at.elapsed() < every) {
            return;
        }
        self.update_read = Some(std::time::Instant::now());
        self.spawn(move || Done::Update(remote.0.server_update().map_err(|e| e.to_string())));
    }

    /// Has the server look for an update, or install the one available; where it cannot
    /// install itself, its page opens here instead.
    pub fn check_update(&mut self, remote: Option<Arc<Remote<Http>>>) {
        let Some(remote) = remote else {
            return;
        };
        self.update_read = None;
        self.spawn(move || {
            let asked = remote.0.check_server_update();
            Done::Update(
                asked
                    .and_then(|()| remote.0.server_update())
                    .map_err(|e| e.to_string()),
            )
        });
    }

    pub fn install_update(&mut self, remote: Option<Arc<Remote<Http>>>) {
        let Some(update) = &self.update else {
            return;
        };
        if !update.can_install {
            crate::update::open_page(&update.page);
            return;
        }
        let Some(remote) = remote else {
            return;
        };
        self.update_read = None;
        self.spawn(move || {
            let asked = remote.0.install_server_update();
            Done::Update(
                asked
                    .and_then(|()| remote.0.server_update())
                    .map_err(|e| e.to_string()),
            )
        });
    }

    /// Invites a user named `name`, an admin or a listener, showing the code they join with.
    pub fn invite_user(&mut self, remote: Option<Arc<Remote<Http>>>, name: &str, admin: bool) {
        let Some(remote) = remote else {
            return;
        };
        let (name, role) = (
            name.trim().to_owned(),
            if admin { Role::Admin } else { Role::Listener },
        );
        self.invite = None;
        self.invite_error.clear();
        self.spawn(move || {
            let invited = remote.0.invite_user(&name, role);
            Done::Invited(
                invited
                    .map(|(code, lasts)| (name, code, SystemTime::now() + lasts))
                    .map_err(|e| e.to_string()),
            )
        });
    }

    /// Invites the user `name` again, with a new code.
    pub fn invite_again(&mut self, remote: Option<Arc<Remote<Http>>>, name: &str) {
        let Some(remote) = remote else {
            return;
        };
        let name = name.to_owned();
        self.invite = None;
        self.invite_error.clear();
        self.spawn(move || {
            let invited = remote.0.invite_again(&name);
            Done::Invited(
                invited
                    .map(|(code, lasts)| (name, code, SystemTime::now() + lasts))
                    .map_err(|e| e.to_string()),
            )
        });
    }

    /// Takes the user `name` away, with their devices.
    pub fn remove_user(&mut self, remote: Option<Arc<Remote<Http>>>, name: &str) {
        let Some(remote) = remote else {
            return;
        };
        let name = name.to_owned();
        self.invite_error.clear();
        if self
            .invite
            .as_ref()
            .is_some_and(|(invited, ..)| *invited == name)
        {
            self.invite = None;
        }
        self.spawn(move || Done::Removed(remote.0.remove_user(&name).map_err(|e| e.to_string())));
    }

    /// Stops showing the code a user was invited with.
    pub fn hide_invite_code(&mut self) {
        self.invite = None;
    }

    /// Reads the account and the server's own information again.
    pub fn refresh(&mut self, remote: Option<&Remote<Http>>) {
        let account = remote.and_then(|remote| remote.0.account().ok().flatten());
        let info = remote.and_then(|remote| remote.0.info().ok());
        self.connected(remote, account, info);
    }

    /// The server connected to through `remote`, its account and itself as they say.
    pub fn connected(
        &mut self,
        remote: Option<&Remote<Http>>,
        account: Option<freya_protocol::AccountInfo>,
        info: Option<freya_protocol::ServerFound>,
    ) {
        let Some(remote) = remote else {
            self.account = None;
            self.info = None;
            return;
        };
        self.account = account;
        self.info = info;
        // The fingerprint shown is the certificate's this computer pinned, not what the server
        // says of itself.
        self.fingerprint = remote
            .0
            .certificate()
            .map(freya_protocol::fingerprint)
            .unwrap_or_default();
    }

    /// Opens the page the account's password is set or changed on in the browser.
    pub fn change_password(&mut self, remote: Option<&Remote<Http>>) {
        let Some(remote) = remote else {
            return;
        };
        self.password_error.clear();
        let ticket = match remote.0.password_ticket() {
            Ok(ticket) => ticket,
            Err(e) => {
                self.password_error = e.to_string();
                return;
            }
        };
        let page = match self.proxy() {
            Ok(proxy) => proxy.show_password(remote.0.pages(), ticket),
            Err(e) => {
                self.password_error = e;
                return;
            }
        };
        crate::update::open_page_privately(&page);
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
        self.user_rows
            .sync(self.users.iter().map(user_row).collect());
        // Shown only while it lasts.
        if self
            .invite
            .as_ref()
            .is_some_and(|(.., until)| *until <= SystemTime::now())
        {
            self.invite = None;
        }
        let invite = self.invite.as_ref();
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
            ("fingerprint", string(&self.fingerprint)),
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
            (
                "login-name",
                string(
                    self.login
                        .as_ref()
                        .map(|l| l.name.clone())
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
            ("connection-code-error", string(&self.connection_code_error)),
            ("login-error", string(&self.login_error)),
            (
                "login-kind",
                string(match &self.claim {
                    None => "log-in",
                    Some((_, Claim::Adopt)) => "adopt",
                    Some((_, Claim::Join { .. })) => "join",
                }),
            ),
            (
                "login-user",
                string(match &self.claim {
                    Some((_, Claim::Join { user })) => user.as_str(),
                    _ => "",
                }),
            ),
            (
                "admin",
                Value::Bool(account.is_some_and(|a| a.role == Role::Admin)),
            ),
            (
                "update-state",
                string(self.update.as_ref().map_or("", |u| u.state.as_str())),
            ),
            (
                "update-version",
                string(self.update.as_ref().map_or("", |u| u.version.as_str())),
            ),
            (
                "update-current",
                string(self.update.as_ref().map_or("", |u| u.current.as_str())),
            ),
            (
                "update-progress",
                Value::Number(self.update.as_ref().map_or(0.0, |u| f64::from(u.progress))),
            ),
            (
                "update-error",
                string(self.update.as_ref().map_or("", |u| u.error.as_str())),
            ),
            (
                "update-can-install",
                Value::Bool(self.update.as_ref().is_some_and(|u| u.can_install)),
            ),
            (
                "invite-code",
                string(
                    invite
                        .map(|(_, code, _)| freya_protocol::show_code(code))
                        .unwrap_or_default(),
                ),
            ),
            (
                "invite-for",
                string(invite.map(|(name, ..)| name.clone()).unwrap_or_default()),
            ),
            (
                "invite-until",
                string(invite.map(|(.., until)| day(*until)).unwrap_or_default()),
            ),
            ("invite-error", string(&self.invite_error)),
        ]
    }
}

/// The day `at` is, here: "2026-10-30".
fn day(at: SystemTime) -> String {
    jiff::Timestamp::try_from(at)
        .map(|at| {
            at.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default()
}

/// A row of `Server.users`.
fn user_row(user: &UserInfo) -> Value {
    let state = if user.owner {
        "owner"
    } else if user.joined {
        "joined"
    } else if user.invited_until.is_some() {
        "invited"
    } else {
        "not-joined"
    };
    Value::Struct(Struct::from_iter([
        ("name".to_owned(), string(&user.name)),
        (
            "role".to_owned(),
            string(match user.role {
                Role::Admin => "admin",
                Role::Listener => "listener",
            }),
        ),
        ("state".to_owned(), string(state)),
        (
            "invited-until".to_owned(),
            string(user.invited_until.map(day).unwrap_or_default()),
        ),
        ("devices".to_owned(), Value::Number(f64::from(user.devices))),
    ]))
}

/// The certificate of the server at `address`, to pin, and its fingerprint.
fn certificate(
    address: Result<SocketAddr, String>,
    name: String,
    claimed: Option<String>,
) -> Result<(Pending, String), String> {
    let address = address?;
    let certificate = freya_client::certificate_of(address)
        .map_err(|e| format!("Could not reach the server at {address}: {e}"))?;
    let fingerprint = freya_protocol::fingerprint(&certificate);
    if claimed.is_some_and(|claimed| claimed != fingerprint) {
        return Err(format!(
            "The computer at {address} gave another certificate than it said when it was found: \
             another computer may be answering in the server's place."
        ));
    }
    let pin = Pin {
        address,
        certificate,
    };
    Ok((Pending { pin, name }, fingerprint))
}

/// Freya Server on this computer, proved by the certificate in its folder, named as the
/// computer is; with what this device may do while it is not adopted, by the code there.
fn this_computer() -> Result<ThisComputer, String> {
    let address: SocketAddr = freya_protocol::LOCAL_ADDRESS
        .parse()
        .map_err(|_| "Freya Server's address on this computer is not one.".to_owned())?;
    let certificate = crate::settings::server_certificate_path()
        .and_then(|file| std::fs::read(file).ok())
        .ok_or("Freya Server on this computer is not this user's: Freya here cannot use it.")?;
    let pin = Pin {
        address,
        certificate,
    };
    // With that certificate: else another program answers in its place.
    Http::pinned(pin.clone())
        .version()
        .map_err(|_| "Freya Server on this computer did not answer as itself.".to_owned())?;
    let fingerprint = freya_protocol::fingerprint(&pin.certificate);
    let claim = match adoption_code() {
        Some(code) => freya_client::verify_server(&pin, &code)?,
        None => None,
    };
    let name = crate::settings::device_name();
    Ok((Pending { pin, name }, fingerprint, claim))
}

/// The code Freya Server on this computer is adopted with, while it is not adopted and the code
/// lasts.
fn adoption_code() -> Option<String> {
    let kept = std::fs::read_to_string(crate::settings::adoption_code_path()?).ok()?;
    let mut lines = kept.lines();
    let code = lines.next()?.trim().to_owned();
    let until: u64 = lines.next()?.trim().parse().ok()?;
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    (until > now && !code.is_empty()).then_some(code)
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
