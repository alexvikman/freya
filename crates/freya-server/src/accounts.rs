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
//! A device connected before refresh tokens renews with the token it has.
//!
//! A new device first knows the server by a connection code, shown on the server's computer:
//! the server proves it knows the code with an HMAC, keyed by the code stretched with PBKDF2
//! and salted with the server's certificate, over the device's nonce and that certificate. A
//! code lasts ten minutes; finding it from a proof would take thousands of years, so asking
//! for proofs again neither helps nor uses it up. Passwords are set and changed in
//! the browser too, on a page a device's ticket opens.

use std::collections::HashMap;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use freya_protocol::{AccountInfo, DeviceInfo, Joined, Role};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

/// PBKDF2 rounds a password is hashed with, as OWASP asks of PBKDF2-HMAC-SHA256.
const ROUNDS: u32 = 600_000;
/// How long a wrong password holds up the answer, so guessing one takes long.
const WRONG_PASSWORD_WAIT: Duration = Duration::from_secs(1);
/// Wrong passwords taken for a user's name from an address, or from an address, before the
/// next has to wait: first this long, twice as long after each, up to the longest, so a
/// thousand guesses take days. For a name from anywhere, more, so a guesser elsewhere does not
/// hold the user up as soon. Forgotten a day after the last.
const FREE_TRIES: u32 = 5;
const FREE_TRIES_FOR_A_NAME: u32 = 20;
const FIRST_WAIT: Duration = Duration::from_secs(30);
const LONGEST_WAIT: Duration = Duration::from_secs(15 * 60);
const TRIES_FORGOTTEN: Duration = Duration::from_secs(24 * 60 * 60);
/// The user a server starts with, who owns it.
const OWNER: u32 = 1;
/// How often when a device was last used is kept: no more than this, so calling the server
/// does not write the accounts every time.
const USED_EVERY: u64 = 60 * 60;
/// How long a connection code lasts, and how many proofs an address may ask before waiting.
const CONNECTION_CODE_LASTS: Duration = Duration::from_secs(10 * 60);
const PROOFS_FREE: u32 = 10;
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
    /// Admin for a user from before roles: the owner, the only one there was.
    #[serde(default)]
    role: Role,
}

#[derive(Clone, Serialize, Deserialize)]
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
    pub role: Role,
}

struct Connection {
    code: String,
    until: Instant,
    key: Option<ring::hmac::Key>,
}

/// Why a password was not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    Wrong,
    /// Too many wrong ones: none is tried for this long.
    Wait(Duration),
}

impl Refused {
    /// What a page says of it.
    pub fn said(&self) -> String {
        match self {
            Self::Wrong => "Wrong user or password.".into(),
            Self::Wait(wait) => {
                let minutes = wait.as_secs().div_ceil(60);
                let unit = if minutes == 1 { "minute" } else { "minutes" };
                format!("Too many wrong passwords. Try again in {minutes} {unit}.")
            }
        }
    }
}

/// Tries of passwords, by what they were tried for: a user's name from an address, a name, or
/// an address; each with the wrong ones it takes before waiting.
#[derive(Default)]
struct Tries(HashMap<String, Tried>);

struct Tried {
    wrong: u32,
    until: Instant,
    last: Instant,
}

type TriedFor = (String, u32);

impl Tries {
    /// How long `keys` wait before another try, at most.
    fn wait(&self, keys: &[TriedFor]) -> Option<Duration> {
        let now = Instant::now();
        keys.iter()
            .filter_map(|(key, _)| self.0.get(key))
            .filter(|tried| tried.until > now)
            .map(|tried| tried.until - now)
            .max()
    }

    /// Counts a try as wrong until it is known to be right: so tries made at once are all
    /// counted before any password is checked.
    fn count(&mut self, keys: &[TriedFor]) {
        let now = Instant::now();
        self.0.retain(|_, tried| now < tried.last + TRIES_FORGOTTEN);
        for (key, free) in keys {
            let tried = self.0.entry(key.clone()).or_insert(Tried {
                wrong: 0,
                until: now,
                last: now,
            });
            tried.wrong += 1;
            tried.last = now;
            if let Some(over) = tried.wrong.checked_sub(*free) {
                let wait = FIRST_WAIT.saturating_mul(1 << over.min(16));
                tried.until = now + wait.min(LONGEST_WAIT);
            }
        }
    }

    /// A try counted was right: the name's are forgotten; the address's not, as someone
    /// else's wrong ones from it still count.
    fn right(&mut self, keys: &[TriedFor]) {
        for (key, _) in keys {
            if key.starts_with("user:") {
                self.0.remove(key);
            } else if let Some(tried) = self.0.get_mut(key) {
                tried.wrong = tried.wrong.saturating_sub(1);
            }
        }
    }
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
    tries: Mutex<Tries>,
    /// The connection code shown now, until when, and its key once derived.
    connection: Mutex<Option<Connection>>,
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
                role: Role::Admin,
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
            tries: Mutex::new(Tries::default()),
            connection: Mutex::new(None),
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
        let user = device.user;
        let caller = Caller {
            user,
            device: device.id.clone(),
            role: Role::Admin,
        };
        let now = now();
        if now >= device.used + USED_EVERY {
            device.used = now;
            self.save(&kept);
        }
        let role = kept.users.iter().find(|u| u.id == user)?.role;
        Some(Caller { role, ..caller })
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

    /// The user `name` is, given their password, tried from `from`. A wrong one holds the
    /// answer up; after a few, for that name or from that address, none is tried for a while.
    fn check(&self, name: &str, password: &str, from: Option<IpAddr>) -> Result<u32, Refused> {
        let (id, hashed) = {
            let kept = self.kept();
            let user = kept
                .users
                .iter()
                .find(|u| u.name.eq_ignore_ascii_case(name.trim()));
            (user.map(|u| u.id), user.and_then(|u| u.password.clone()))
        };
        // A name only counts when there is such a user, so made-up ones fill nothing.
        let mut keys: Vec<TriedFor> = Vec::new();
        if let Some(id) = id {
            keys.push((format!("user:{id}"), FREE_TRIES_FOR_A_NAME));
        }
        if let Some(ip) = from {
            keys.push((format!("from:{ip}"), FREE_TRIES));
            if let Some(id) = id {
                keys.push((format!("user:{id}@{ip}"), FREE_TRIES));
            }
        }
        {
            let mut tries = self.tries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(wait) = tries.wait(&keys) {
                return Err(Refused::Wait(wait));
            }
            tries.count(&keys);
        }
        match (id, hashed.is_some_and(|hashed| hashed.check(password))) {
            (Some(id), true) => {
                self.tries
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .right(&keys);
                Ok(id)
            }
            _ => {
                std::thread::sleep(WRONG_PASSWORD_WAIT);
                Err(Refused::Wrong)
            }
        }
    }

    /// A user named `name` with the role `role`, and the password they start with: their id.
    pub fn add_user(&self, name: &str, role: Role, password: &str) -> Result<u32, String> {
        let name = name.trim();
        if password.chars().count() < SHORTEST_PASSWORD {
            return Err(format!(
                "A password needs at least {SHORTEST_PASSWORD} characters."
            ));
        }
        let mut kept = self.kept();
        if name.is_empty() || kept.users.iter().any(|u| u.name.eq_ignore_ascii_case(name)) {
            return Err(format!("There is a user named {name} already."));
        }
        let id = kept.users.iter().map(|u| u.id).max().unwrap_or(0) + 1;
        kept.users.push(User {
            id,
            name: name.to_owned(),
            password: Some(Password::new(password)),
            role,
        });
        self.save(&kept);
        Ok(id)
    }

    /// A code for the user `name`, given their password, to send to `redirect`: the client
    /// that sent `challenge` trades it for a token with [`Self::redeem`].
    pub fn grant(
        &self,
        name: &str,
        password: &str,
        challenge: &str,
        redirect: &str,
        from: Option<IpAddr>,
    ) -> Result<String, Refused> {
        let user = self.check(name, password, from)?;
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
        Ok(code)
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

    /// A new connection code, in place of any shown before; how long it lasts.
    pub fn connection_code(&self) -> (String, Duration) {
        let code: String = random_bytes(freya_protocol::CODE_LENGTH)
            .iter()
            .map(|b| freya_protocol::CODE_ALPHABET[usize::from(b & 31)] as char)
            .collect();
        *self.connection.lock().unwrap_or_else(|e| e.into_inner()) = Some(Connection {
            code: code.clone(),
            until: Instant::now() + CONNECTION_CODE_LASTS,
            key: None,
        });
        (code, CONNECTION_CODE_LASTS)
    }

    /// The proof that this server knows the connection code shown, for the device that sent
    /// `nonce` and was given `certificate`; none without a code that lasts still. The code's
    /// key is derived once, so asking again costs the server little.
    pub fn prove(&self, nonce: &[u8], certificate: &[u8]) -> Option<Vec<u8>> {
        let digest = ring::digest::digest(&ring::digest::SHA256, certificate);
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let shown = connection.as_mut().filter(|c| c.until > Instant::now())?;
        let key = shown.key.get_or_insert_with(|| {
            let derived = freya_protocol::connection_key(&shown.code, digest.as_ref());
            ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &derived)
        });
        let message = freya_protocol::connection_message(nonce, digest.as_ref());
        Some(ring::hmac::sign(key, &message).as_ref().to_vec())
    }

    /// Whether a device at `from` may ask for another proof now: a few a minute.
    pub fn may_ask_proof(&self, from: Option<IpAddr>) -> bool {
        let Some(from) = from else {
            return true;
        };
        let key = [(format!("proof:{from}"), PROOFS_FREE)];
        let mut tries = self.tries.lock().unwrap_or_else(|e| e.into_inner());
        if tries.wait(&key).is_some() {
            return false;
        }
        tries.count(&key);
        true
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
    pub fn change_password(
        &self,
        ticket: &str,
        current: &str,
        new: &str,
        from: Option<IpAddr>,
    ) -> Result<(), String> {
        let page = self
            .password_page(ticket)
            .ok_or("This page is out of date. Open it again from Freya's settings.")?;
        if new.chars().count() < SHORTEST_PASSWORD {
            return Err(format!(
                "A password needs at least {SHORTEST_PASSWORD} characters."
            ));
        }
        if page.asks_current {
            match self.check(&page.user, current, from) {
                Ok(_) => {}
                Err(Refused::Wrong) => return Err("That is not the password now.".into()),
                Err(waiting) => return Err(waiting.said()),
            }
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
            role: user.map(|u| u.role).unwrap_or_default(),
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
        assert_eq!(accounts.grant(&owner, "", &sent, redirect, None).ok(), None);

        // The Mac, on the server's computer, sets one without giving one now.
        let ticket = accounts.ticket(&mac, true);
        assert!(!accounts.password_page(&ticket).unwrap().asks_current);
        assert!(
            accounts
                .change_password(&ticket, "", "short", None)
                .is_err()
        );
        accounts
            .change_password(&ticket, "", "correct horse", None)
            .unwrap();
        assert!(
            accounts.password_page(&ticket).is_none(),
            "a ticket is used once"
        );

        // Logging in: a code for the password, traded once, by the client that asked.
        assert_eq!(
            accounts
                .grant(&owner, "wrong horse", &sent, redirect, None)
                .ok(),
            None
        );
        let code = accounts
            .grant(&owner, "correct horse", &sent, redirect, None)
            .ok()
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
            .grant(&owner, "correct horse", &sent, redirect, None)
            .ok()
            .unwrap();
        assert_eq!(
            accounts.redeem(&code, verifier, "http://127.0.0.1:1/callback", "PC"),
            None
        );
        let code = accounts
            .grant(&owner, "correct horse", &sent, redirect, None)
            .ok()
            .unwrap();
        let pc_tokens = accounts.redeem(&code, verifier, redirect, "PC").unwrap();
        let pc = accounts.caller(&pc_tokens.access).unwrap();

        // Elsewhere, the password now is asked for to change it.
        let ticket = accounts.ticket(&pc, false);
        assert!(accounts.password_page(&ticket).unwrap().asks_current);
        assert!(
            accounts
                .change_password(&ticket, "wrong horse", "battery staple", None)
                .is_err()
        );
        accounts
            .change_password(&ticket, "correct horse", "battery staple", None)
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
    fn wrong_passwords_wait_longer_and_longer() {
        let accounts = Accounts::open(None, None);
        let kim = accounts
            .add_user("Kim", Role::Listener, "correct horse")
            .unwrap();
        assert!(
            accounts
                .add_user("kim", Role::Listener, "battery staple")
                .is_err()
        );
        let here = Some(IpAddr::from([192, 168, 1, 20]));
        let elsewhere = Some(IpAddr::from([192, 168, 1, 21]));
        let at = |ip: &str| {
            [
                (format!("user:{kim}"), FREE_TRIES_FOR_A_NAME),
                (format!("from:{ip}"), FREE_TRIES),
                (format!("user:{kim}@{ip}"), FREE_TRIES),
            ]
        };
        let mut tries = accounts.tries.lock().unwrap();
        let keys = at("192.168.1.20");
        for _ in 0..FREE_TRIES - 1 {
            tries.count(&keys);
        }
        assert_eq!(tries.wait(&keys), None, "a few are free");
        tries.count(&keys);
        let first = tries.wait(&keys).unwrap();
        assert!(first > FIRST_WAIT - Duration::from_secs(1) && first <= FIRST_WAIT);
        tries.count(&keys);
        assert!(tries.wait(&keys).unwrap() > FIRST_WAIT, "longer each time");
        for _ in 0..20 {
            tries.count(&keys);
        }
        assert!(tries.wait(&keys).unwrap() <= LONGEST_WAIT);
        drop(tries);

        // Waiting, even the right password is not tried from there.
        let refused = accounts.check("Kim", "correct horse", here);
        assert!(matches!(refused, Err(Refused::Wait(_))));
        assert!(
            refused
                .unwrap_err()
                .said()
                .starts_with("Too many wrong passwords")
        );
        // Too many for the name from anywhere: elsewhere waits too.
        assert!(matches!(
            accounts.check("Kim", "correct horse", elsewhere),
            Err(Refused::Wait(_))
        ));
        // Made-up names count only for the address.
        let mut tries = accounts.tries.lock().unwrap();
        tries.0.clear();
        drop(tries);
        assert_eq!(
            accounts.check("nobody", "x", elsewhere),
            Err(Refused::Wrong)
        );
        assert_eq!(accounts.tries.lock().unwrap().0.len(), 1);

        // A few from one address hold the name up there, not elsewhere.
        accounts.tries.lock().unwrap().0.clear();
        {
            let mut tries = accounts.tries.lock().unwrap();
            for _ in 0..FREE_TRIES {
                tries.count(&at("192.168.1.20"));
            }
        }
        assert!(matches!(
            accounts.check("Kim", "correct horse", here),
            Err(Refused::Wait(_))
        ));
        assert_eq!(accounts.check("Kim", "correct horse", elsewhere), Ok(kim));
        // The right one forgets the name's tries from there.
        assert!(
            !accounts
                .tries
                .lock()
                .unwrap()
                .0
                .contains_key(&format!("user:{kim}@192.168.1.21"))
        );
    }

    #[test]
    fn a_connection_code_is_proved_while_it_lasts() {
        let accounts = Accounts::open(None, None);
        let (nonce, certificate) = ([7; 32], b"the server's certificate".as_slice());
        assert_eq!(accounts.prove(&nonce, certificate), None, "no code shown");
        let (code, lasts) = accounts.connection_code();
        assert_eq!(lasts, CONNECTION_CODE_LASTS);
        assert_eq!(code.len(), freya_protocol::CODE_LENGTH);
        assert_eq!(
            freya_protocol::normalize_code(&code).as_deref(),
            Some(code.as_str())
        );
        let proof = accounts.prove(&nonce, certificate).unwrap();
        let digest = ring::digest::digest(&ring::digest::SHA256, certificate);
        let derived = freya_protocol::connection_key(&code, digest.as_ref());
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &derived);
        let message = freya_protocol::connection_message(&nonce, digest.as_ref());
        assert!(ring::hmac::verify(&key, &message, &proof).is_ok());
        // Asking again is no use to anyone, and does not use the code up.
        assert!(accounts.prove(&[8; 32], certificate).is_some());
        // A new one takes the place of the one before.
        let (first, _) = accounts.connection_code();
        let (second, _) = accounts.connection_code();
        assert_ne!(first, second);
        // An address asks a few a minute.
        let from = Some(IpAddr::from([192, 168, 1, 30]));
        for _ in 0..PROOFS_FREE {
            assert!(accounts.may_ask_proof(from));
        }
        assert!(!accounts.may_ask_proof(from));
    }

    #[test]
    fn a_listener_logs_in_as_a_listener() {
        let accounts = Accounts::open(None, None);
        accounts
            .add_user("Kim", Role::Listener, "correct horse")
            .unwrap();
        let redirect = "http://127.0.0.1:47477/callback";
        let verifier = "a secret only the client knows, long enough";
        let code = accounts
            .grant("Kim", "correct horse", &challenge(verifier), redirect, None)
            .unwrap();
        let tokens = accounts.redeem(&code, verifier, redirect, "Phone").unwrap();
        let kim = accounts.caller(&tokens.access).unwrap();
        assert_eq!(kim.role, Role::Listener);
        assert_eq!(accounts.info(&kim).role, Role::Listener);
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
