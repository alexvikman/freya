//! Who may use the server: its users, each with a password once they set one, and the devices
//! they connected, each with a token of its own that can be revoked. Kept in a file in the
//! server's folder that only the user can read; of the tokens only their SHA-256 is kept, and
//! of the passwords their PBKDF2.
//!
//! A client on the server's own computer connects with the pairing key: a secret the server
//! writes to a file of its own, readable only by the user it runs as, and makes anew once
//! used, so only a program of that user, on that computer, connects so.
//!
//! Other devices log in in the browser, as OAuth's authorization code with PKCE has it: the
//! login page gives a code for the password, which only the client that asked for it can trade
//! for tokens, having the secret its challenge was made from.
//!
//! A device calls with an access token, which lasts an hour: its id and when it ends, signed
//! with the server's own secret, so nothing is kept of it and it outlasts a restart. It renews
//! it with a refresh token, which changes each time. A refresh token already used, coming
//! again, was copied: the device's tokens are all taken back, so whoever has them logs in again.
//! A device connected before refresh tokens renews with the token it has. Passwords are set and changed in
//! the browser too, on a page a device's ticket opens.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use freya_protocol::{AccountInfo, DeviceInfo, Joined};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

/// PBKDF2 rounds a password is hashed with, as OWASP asks of PBKDF2-HMAC-SHA256.
const ROUNDS: u32 = 600_000;
/// How long a wrong password holds up the answer, so guessing one takes long.
const WRONG_PASSWORD_WAIT: Duration = Duration::from_secs(1);
/// The user a server starts with, who owns it.
const OWNER: u32 = 1;
/// How often when a device was last used is kept: no more than this, so calling the server
/// does not write the accounts every time.
const USED_EVERY: u64 = 60 * 60;
/// How long an access token lasts, in seconds.
pub const ACCESS_LASTS: u64 = 60 * 60;
/// How long after it was renewed a refresh token still renews: for a client whose answer was
/// lost on the way, asking again.
const RENEWED_GRACE: u64 = 2 * 60;
/// How many used refresh tokens of a device are kept, to know them if they come again.
const RETIRED_KEPT: usize = 32;
/// How long a login's code may wait to be traded for a token, and a password page be used.
const CODE_LASTS: Duration = Duration::from_secs(120);
const TICKET_LASTS: Duration = Duration::from_secs(15 * 60);
/// The fewest characters a password has.
pub const SHORTEST_PASSWORD: usize = 8;

#[derive(Default, Serialize, Deserialize)]
struct Kept {
    users: Vec<User>,
    devices: Vec<Device>,
    /// What access tokens are signed with, hex.
    #[serde(default)]
    secret: String,
}

#[derive(Serialize, Deserialize)]
struct User {
    id: u32,
    name: String,
    password: Option<Password>,
}

#[derive(Serialize, Deserialize)]
struct Password {
    /// Hex.
    salt: String,
    hash: String,
    rounds: u32,
}

#[derive(Clone, Serialize, Deserialize)]
struct Device {
    /// Hex, to name it when revoking it.
    id: String,
    user: u32,
    name: String,
    /// The SHA-256 of its refresh token, hex.
    token: String,
    /// The refresh token it renewed with last, and when: taken again for a while.
    #[serde(default)]
    previous: Option<Renewed>,
    /// The ones before.
    #[serde(default)]
    retired: Vec<String>,
    /// Seconds since 1970.
    created: u64,
    #[serde(default)]
    used: u64,
    #[serde(default)]
    joined: Option<Joined>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Renewed {
    token: String,
    at: u64,
}

/// What a device is given to call with: an access token for [`ACCESS_LASTS`] seconds, and a
/// refresh token to renew it with, once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Granted {
    pub access: String,
    pub refresh: String,
}

impl Granted {
    /// As OAuth's token endpoint answers.
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "access_token": self.access,
            "refresh_token": self.refresh,
            "token_type": "Bearer",
            "expires_in": ACCESS_LASTS,
        })
    }
}

/// A login's code, kept until traded for a token or out of date: who logged in, the challenge
/// the client sent, and where the code was sent.
struct Grant {
    /// The SHA-256 of the code, hex.
    code: String,
    user: u32,
    challenge: String,
    redirect: String,
    until: Instant,
}

/// What opens a password page: a device's user, and whether that device is on the server's
/// own computer, where the user need not know the password to change it.
struct Ticket {
    /// The SHA-256 of the ticket, hex.
    ticket: String,
    user: u32,
    local: bool,
    until: Instant,
}

/// Whose password page a ticket opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasswordPage {
    pub user: String,
    /// Whether the password now has to be given to change it.
    pub asks_current: bool,
}

/// A device a token was given to, as a request names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    pub user: u32,
    pub device: String,
}

pub struct Accounts {
    /// Where they are kept; nowhere in tests.
    file: Option<PathBuf>,
    kept: Mutex<Kept>,
    /// The SHA-256 of the pairing key now, hex, and where it is written.
    pairing: Mutex<Option<String>>,
    pairing_file: Option<PathBuf>,
    grants: Mutex<Vec<Grant>>,
    tickets: Mutex<Vec<Ticket>>,
}

impl Accounts {
    /// The accounts kept in `file`, with the pairing key written to `pairing_file`; a server
    /// without any starts with its owner, who has no password yet.
    pub fn open(file: Option<PathBuf>, pairing_file: Option<PathBuf>) -> Self {
        let mut kept: Kept = file
            .as_deref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|text| match serde_json::from_str(&text) {
                Ok(kept) => Some(kept),
                Err(e) => {
                    log::warn!("could not read the accounts: {e}");
                    None
                }
            })
            .unwrap_or_default();
        if kept.users.is_empty() {
            kept.users.push(User {
                id: OWNER,
                name: owner_name(),
                password: None,
            });
        }
        if unhex(&kept.secret).is_none_or(|secret| secret.len() < 32) {
            kept.secret = random_hex(32);
        }
        let accounts = Self {
            file,
            kept: Mutex::new(kept),
            pairing: Mutex::new(None),
            pairing_file,
            grants: Mutex::new(Vec::new()),
            tickets: Mutex::new(Vec::new()),
        };
        accounts.save(&accounts.kept());
        accounts.new_pairing_key();
        accounts
    }

    fn kept(&self) -> MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Who calls with the access token `token`, if anyone; noted as when the device was last
    /// used.
    pub fn caller(&self, token: &str) -> Option<Caller> {
        let mut parts = token.split('.');
        let (Some(id), Some(until), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        let mut kept = self.kept();
        let key = signing_key(&kept)?;
        ring::hmac::verify(&key, format!("{id}.{until}").as_bytes(), &unhex(signature)?).ok()?;
        if until.parse::<u64>().ok()? <= now() {
            return None;
        }
        let device = kept.devices.iter_mut().find(|d| d.id == id)?;
        let caller = Caller {
            user: device.user,
            device: device.id.clone(),
        };
        let now = now();
        if now >= device.used + USED_EVERY {
            device.used = now;
            self.save(&kept);
        }
        Some(caller)
    }

    /// A token for a device named `name` of the owner, given the pairing key; none for
    /// another key. The key is made anew either way. A device of that name paired before is
    /// this computer's too, pairing again: it goes, so the devices do not pile up.
    pub fn pair(&self, key: &str, name: &str) -> Option<Granted> {
        let right = {
            let pairing = self.pairing.lock().unwrap_or_else(|e| e.into_inner());
            pairing.as_deref() == Some(sha256(key.as_bytes()).as_str())
        };
        self.new_pairing_key();
        if !right {
            return None;
        }
        let name = device_name(name);
        self.kept()
            .devices
            .retain(|d| !(d.joined == Some(Joined::Paired) && d.name == name));
        self.add_device(OWNER, &name, Joined::Paired)
    }

    /// The user `name` is, given their password; a wrong one holds the answer up.
    fn check(&self, name: &str, password: &str) -> Option<u32> {
        let user = {
            let kept = self.kept();
            kept.users
                .iter()
                .find(|u| u.name.eq_ignore_ascii_case(name))
                .and_then(|u| Some((u.id, u.password.as_ref()?.check(password))))
        };
        match user {
            Some((id, true)) => Some(id),
            _ => {
                std::thread::sleep(WRONG_PASSWORD_WAIT);
                None
            }
        }
    }

    /// A code for the user `name`, given their password, to send to `redirect`: the client
    /// that sent `challenge` trades it for a token with [`Self::redeem`].
    pub fn grant(
        &self,
        name: &str,
        password: &str,
        challenge: &str,
        redirect: &str,
    ) -> Option<String> {
        let user = self.check(name, password)?;
        let code = random_hex(32);
        let mut grants = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        grants.retain(|g| g.until > Instant::now());
        grants.push(Grant {
            code: sha256(code.as_bytes()),
            user,
            challenge: challenge.to_owned(),
            redirect: redirect.to_owned(),
            until: Instant::now() + CODE_LASTS,
        });
        Some(code)
    }

    /// A token for a device named `device`, given a code from [`Self::grant`] sent to
    /// `redirect`, and the secret its challenge was made from. A code is taken once, right or
    /// wrong.
    pub fn redeem(
        &self,
        code: &str,
        verifier: &str,
        redirect: &str,
        device: &str,
    ) -> Option<Granted> {
        let grant = {
            let mut grants = self.grants.lock().unwrap_or_else(|e| e.into_inner());
            let hash = sha256(code.as_bytes());
            let at = grants.iter().position(|g| g.code == hash)?;
            grants.swap_remove(at)
        };
        let fits = grant.until > Instant::now()
            && grant.redirect == redirect
            && challenge(verifier) == grant.challenge;
        if fits {
            self.add_device(grant.user, device, Joined::LoggedIn)
        } else {
            None
        }
    }

    /// New tokens for the device whose refresh token `token` is, which is used up. One used
    /// before takes the device's tokens back.
    pub fn renew(&self, token: &str) -> Option<Granted> {
        let hash = sha256(token.as_bytes());
        let now = now();
        let mut kept = self.kept();
        if let Some(at) = kept.devices.iter().position(|d| d.token == hash) {
            let device = &mut kept.devices[at];
            if let Some(previous) = device.previous.take() {
                device.retired.push(previous.token);
            }
            device.previous = Some(Renewed {
                token: hash,
                at: now,
            });
            let granted = issue(&mut kept, at);
            self.save(&kept);
            return granted;
        }
        let renewed = |d: &Device| {
            d.previous
                .as_ref()
                .filter(|p| p.token == hash)
                .map(|p| p.at)
        };
        // Its answer was lost: renewed again, the tokens it never had used up.
        if let Some(at) = kept
            .devices
            .iter()
            .position(|d| renewed(d).is_some_and(|at| now <= at + RENEWED_GRACE))
        {
            let device = &mut kept.devices[at];
            let lost = std::mem::take(&mut device.token);
            device.retired.push(lost);
            // Once: asked again, it was copied.
            if let Some(previous) = device.previous.take() {
                device.retired.push(previous.token);
            }
            let granted = issue(&mut kept, at);
            self.save(&kept);
            return granted;
        }
        if let Some(at) = kept
            .devices
            .iter()
            .position(|d| d.retired.contains(&hash) || renewed(d).is_some())
        {
            let gone = kept.devices.remove(at);
            log::warn!(
                "a used refresh token of {} came again, so it was copied: the device is logged out",
                gone.name
            );
            self.save(&kept);
        }
        None
    }

    /// A ticket that opens the password page of `caller`'s user; `local` when they call from
    /// the server's own computer.
    pub fn ticket(&self, caller: &Caller, local: bool) -> String {
        let ticket = random_hex(32);
        let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        tickets.retain(|t| t.until > Instant::now());
        tickets.push(Ticket {
            ticket: sha256(ticket.as_bytes()),
            user: caller.user,
            local,
            until: Instant::now() + TICKET_LASTS,
        });
        ticket
    }

    /// Whose password page `ticket` opens, while it does.
    pub fn password_page(&self, ticket: &str) -> Option<PasswordPage> {
        let (user, local) = {
            let tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
            let hash = sha256(ticket.as_bytes());
            let t = tickets
                .iter()
                .find(|t| t.ticket == hash && t.until > Instant::now())?;
            (t.user, t.local)
        };
        let kept = self.kept();
        let user = kept.users.iter().find(|u| u.id == user)?;
        Some(PasswordPage {
            user: user.name.clone(),
            asks_current: !local && user.password.is_some(),
        })
    }

    /// Changes the password of the user `ticket` is for to `new`, given the one now when the
    /// page asks for it; the ticket is used up once it did.
    pub fn change_password(&self, ticket: &str, current: &str, new: &str) -> Result<(), String> {
        let page = self
            .password_page(ticket)
            .ok_or("This page is out of date. Open it again from Freya's settings.")?;
        if new.chars().count() < SHORTEST_PASSWORD {
            return Err(format!(
                "A password needs at least {SHORTEST_PASSWORD} characters."
            ));
        }
        if page.asks_current && self.check(&page.user, current).is_none() {
            return Err("That is not the password now.".into());
        }
        let user = {
            let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
            let hash = sha256(ticket.as_bytes());
            let at = tickets
                .iter()
                .position(|t| t.ticket == hash)
                .ok_or("This page is out of date. Open it again from Freya's settings.")?;
            tickets.swap_remove(at).user
        };
        self.set_password(user, new)
    }

    /// Sets the password `user` logs in with on other devices.
    fn set_password(&self, user: u32, password: &str) -> Result<(), String> {
        let mut kept = self.kept();
        let user = kept
            .users
            .iter_mut()
            .find(|u| u.id == user)
            .ok_or("no such user")?;
        user.password = Some(Password::new(password));
        self.save(&kept);
        Ok(())
    }

    /// What `caller` sees of their account.
    pub fn info(&self, caller: &Caller) -> AccountInfo {
        let kept = self.kept();
        let user = kept.users.iter().find(|u| u.id == caller.user);
        AccountInfo {
            user: user.map(|u| u.name.clone()).unwrap_or_default(),
            has_password: user.is_some_and(|u| u.password.is_some()),
            devices: kept
                .devices
                .iter()
                .filter(|d| d.user == caller.user)
                .map(|d| DeviceInfo {
                    id: d.id.clone(),
                    name: d.name.clone(),
                    created: SystemTime::UNIX_EPOCH + Duration::from_secs(d.created),
                    used: (d.used > 0)
                        .then(|| SystemTime::UNIX_EPOCH + Duration::from_secs(d.used)),
                    joined: d.joined,
                    this: d.id == caller.device,
                })
                .collect(),
        }
    }

    /// Takes the token of `caller`'s device `device` back; whether there was one.
    pub fn revoke(&self, caller: &Caller, device: &str) -> bool {
        let mut kept = self.kept();
        let before = kept.devices.len();
        kept.devices
            .retain(|d| !(d.user == caller.user && d.id == device));
        let revoked = kept.devices.len() < before;
        if revoked {
            self.save(&kept);
        }
        revoked
    }

    fn add_device(&self, user: u32, name: &str, joined: Joined) -> Option<Granted> {
        let device = Device {
            id: random_hex(8),
            user,
            name: device_name(name),
            token: String::new(),
            previous: None,
            retired: Vec::new(),
            created: now(),
            used: now(),
            joined: Some(joined),
        };
        log::info!("a device was connected: {}", device.name);
        let mut kept = self.kept();
        kept.devices.push(device);
        let at = kept.devices.len() - 1;
        let granted = issue(&mut kept, at);
        self.save(&kept);
        granted
    }

    /// Makes a new pairing key and writes it where a client on this computer reads it.
    fn new_pairing_key(&self) {
        let key = random_hex(32);
        *self.pairing.lock().unwrap_or_else(|e| e.into_inner()) = Some(sha256(key.as_bytes()));
        if let Some(file) = &self.pairing_file
            && let Err(e) = write_private(file, key.as_bytes())
        {
            log::warn!("could not write the pairing key: {e}");
        }
    }

    fn save(&self, kept: &Kept) {
        let Some(file) = &self.file else {
            return;
        };
        let written = serde_json::to_vec_pretty(kept)
            .map_err(std::io::Error::other)
            .and_then(|json| write_private(file, &json));
        if let Err(e) = written {
            log::warn!("could not keep the accounts: {e}");
        }
    }
}

impl Password {
    fn new(password: &str) -> Self {
        let salt = random_bytes(16);
        let mut hash = [0; 32];
        ring::pbkdf2::derive(
            ring::pbkdf2::PBKDF2_HMAC_SHA256,
            NonZeroU32::new(ROUNDS).unwrap_or(NonZeroU32::MIN),
            &salt,
            password.as_bytes(),
            &mut hash,
        );
        Self {
            salt: hex(&salt),
            hash: hex(&hash),
            rounds: ROUNDS,
        }
    }

    fn check(&self, password: &str) -> bool {
        let (Some(salt), Some(hash), Some(rounds)) = (
            unhex(&self.salt),
            unhex(&self.hash),
            NonZeroU32::new(self.rounds),
        ) else {
            return false;
        };
        ring::pbkdf2::verify(
            ring::pbkdf2::PBKDF2_HMAC_SHA256,
            rounds,
            &salt,
            password.as_bytes(),
            &hash,
        )
        .is_ok()
    }
}

/// New tokens for the `at`th device: an access token, and a refresh token in place of the one
/// it had.
fn issue(kept: &mut Kept, at: usize) -> Option<Granted> {
    let key = signing_key(kept)?;
    let device = kept.devices.get_mut(at)?;
    let refresh = random_hex(32);
    device.token = sha256(refresh.as_bytes());
    let excess = device.retired.len().saturating_sub(RETIRED_KEPT);
    device.retired.drain(..excess);
    let signed = format!("{}.{}", device.id, now() + ACCESS_LASTS);
    let signature = hex(ring::hmac::sign(&key, signed.as_bytes()).as_ref());
    Some(Granted {
        access: format!("{signed}.{signature}"),
        refresh,
    })
}

fn signing_key(kept: &Kept) -> Option<ring::hmac::Key> {
    Some(ring::hmac::Key::new(
        ring::hmac::HMAC_SHA256,
        &unhex(&kept.secret)?,
    ))
}

/// Seconds since 1970.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A device's name as kept: its first 100 characters.
fn device_name(name: &str) -> String {
    name.chars().take(100).collect()
}

/// The name of the user the server runs as, for its owner.
fn owner_name() -> String {
    ["USER", "USERNAME"]
        .iter()
        .find_map(|var| std::env::var(var).ok())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "owner".into())
}

fn random_bytes(count: usize) -> Vec<u8> {
    let mut bytes = vec![0; count];
    // The system's randomness failing leaves nothing safe to do.
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("the system's random numbers");
    bytes
}

fn random_hex(count: usize) -> String {
    hex(&random_bytes(count))
}

/// The challenge a client sends for `verifier`, as PKCE's S256 makes it: its SHA-256, in
/// base64url without padding.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ))
}

fn sha256(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
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

/// Writes `bytes` to `file` through a new file renamed over it, readable and writable by the
/// user only.
pub fn write_private(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = file.with_extension("partial");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut out = options.open(&partial)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        out.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    out.write_all(bytes)?;
    out.sync_all()?;
    std::fs::rename(&partial, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_pair_log_in_and_are_revoked() {
        let dir = std::env::temp_dir().join(format!("freya-accounts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (file, pairing) = (dir.join("accounts.json"), dir.join("pairing.key"));
        let accounts = Accounts::open(Some(file.clone()), Some(pairing.clone()));
        let key = std::fs::read_to_string(&pairing).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |f: &Path| std::fs::metadata(f).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&pairing), 0o600);
            assert_eq!(mode(&file), 0o600);
        }

        assert_eq!(accounts.pair("wrong", "Mac"), None);
        assert_eq!(accounts.pair(&key, "Mac"), None, "a key is used once");
        let key = std::fs::read_to_string(&pairing).unwrap();
        let old = accounts.pair(&key, "Mac").unwrap();
        // Pairing again takes the place of the device paired before.
        let key = std::fs::read_to_string(&pairing).unwrap();
        let paired = accounts.pair(&key, "Mac").unwrap();
        assert_eq!(accounts.caller(&old.access), None);
        let mac = accounts.caller(&paired.access).unwrap();
        assert_eq!(mac.user, OWNER);
        assert_eq!(accounts.caller("not a token"), None);
        assert_eq!(
            accounts.caller(&paired.refresh),
            None,
            "a refresh token is not called with"
        );
        let saved = std::fs::read_to_string(&file).unwrap();
        assert!(!saved.contains(&paired.refresh) && !saved.contains(&paired.access));

        // No password yet: nobody logs in.
        let owner = accounts.info(&mac).user;
        let redirect = "http://127.0.0.1:47477/callback";
        let verifier = "a secret only the client knows, long enough";
        let sent = challenge(verifier);
        assert_eq!(accounts.grant(&owner, "", &sent, redirect), None);

        // The Mac, on the server's computer, sets one without giving one now.
        let ticket = accounts.ticket(&mac, true);
        assert!(!accounts.password_page(&ticket).unwrap().asks_current);
        assert!(accounts.change_password(&ticket, "", "short").is_err());
        accounts
            .change_password(&ticket, "", "correct horse")
            .unwrap();
        assert!(
            accounts.password_page(&ticket).is_none(),
            "a ticket is used once"
        );

        // Logging in: a code for the password, traded once, by the client that asked.
        assert_eq!(accounts.grant(&owner, "wrong horse", &sent, redirect), None);
        let code = accounts
            .grant(&owner, "correct horse", &sent, redirect)
            .unwrap();
        assert_eq!(
            accounts.redeem(&code, "another secret", redirect, "PC"),
            None
        );
        assert_eq!(
            accounts.redeem(&code, verifier, redirect, "PC"),
            None,
            "a code is taken once, right or wrong"
        );
        let code = accounts
            .grant(&owner, "correct horse", &sent, redirect)
            .unwrap();
        assert_eq!(
            accounts.redeem(&code, verifier, "http://127.0.0.1:1/callback", "PC"),
            None
        );
        let code = accounts
            .grant(&owner, "correct horse", &sent, redirect)
            .unwrap();
        let pc_tokens = accounts.redeem(&code, verifier, redirect, "PC").unwrap();
        let pc = accounts.caller(&pc_tokens.access).unwrap();

        // Elsewhere, the password now is asked for to change it.
        let ticket = accounts.ticket(&pc, false);
        assert!(accounts.password_page(&ticket).unwrap().asks_current);
        assert!(
            accounts
                .change_password(&ticket, "wrong horse", "battery staple")
                .is_err()
        );
        accounts
            .change_password(&ticket, "correct horse", "battery staple")
            .unwrap();

        // Kept across starts.
        let again = Accounts::open(Some(file.clone()), Some(pairing.clone()));
        let info = again.info(&mac);
        assert!(info.has_password);
        let names: Vec<&str> = info.devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Mac", "PC"]);
        assert!(info.devices[0].this);
        // Access tokens outlast a restart.
        assert_eq!(again.caller(&paired.access).as_ref(), Some(&mac));
        assert!(again.revoke(&mac, &pc.device));
        assert_eq!(again.caller(&pc_tokens.access), None);
        assert_eq!(again.info(&mac).devices.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_tokens_change_and_a_copied_one_logs_the_device_out() {
        let dir = std::env::temp_dir().join(format!("freya-renewing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pairing = dir.join("pairing.key");
        let accounts = Accounts::open(None, Some(pairing.clone()));
        let key = std::fs::read_to_string(&pairing).unwrap();
        let first = accounts.pair(&key, "Mac").unwrap();
        let mac = accounts.caller(&first.access).unwrap();

        let second = accounts.renew(&first.refresh).unwrap();
        assert_ne!(second.refresh, first.refresh);
        assert_eq!(accounts.caller(&second.access).as_ref(), Some(&mac));
        // The answer was lost: asked again at once, it renews again.
        let third = accounts.renew(&first.refresh).unwrap();
        assert!(
            accounts.caller(&third.access).is_some(),
            "not taken for a copy"
        );
        let fourth = accounts.renew(&third.refresh).unwrap();
        assert!(accounts.caller(&fourth.access).is_some());

        // A refresh token used before, coming again, or the one whose answer was lost: copied.
        // The device is logged out.
        assert_eq!(accounts.renew(&second.refresh), None);
        assert_eq!(accounts.caller(&fourth.access), None);
        assert_eq!(accounts.renew(&fourth.refresh), None);

        // A lost answer is asked again once: twice, the token was copied.
        let key = std::fs::read_to_string(&pairing).unwrap();
        let paired = accounts.pair(&key, "Tablet").unwrap();
        let renewed = accounts.renew(&paired.refresh).unwrap();
        assert!(accounts.renew(&paired.refresh).is_some());
        assert_eq!(accounts.renew(&paired.refresh), None);
        assert_eq!(accounts.caller(&renewed.access), None);

        // Forged or ended access tokens are not taken.
        let key = std::fs::read_to_string(&pairing).unwrap();
        let fresh = accounts.pair(&key, "Mac").unwrap();
        let (signed, _) = fresh.access.rsplit_once('.').unwrap();
        assert_eq!(
            accounts.caller(&format!("{signed}.{}", "00".repeat(32))),
            None
        );
        let id = signed.split('.').next().unwrap();
        assert_eq!(
            accounts.caller(&format!("{id}.1.{}", "00".repeat(32))),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
