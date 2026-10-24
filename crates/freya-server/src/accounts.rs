//! Who may use the server: its users, each with a password once they set one, and the devices
//! they connected, each with a token of its own that can be revoked. Kept in a file in the
//! server's folder that only the user can read; of the tokens only their SHA-256 is kept, and
//! of the passwords their PBKDF2.
//!
//! Every device logs in with a password, on the server's own computer as elsewhere: a server
//! is adopted first, by whoever proves they know the code it shows, who chooses its owner's
//! name and password. A password, once set, is changed only by giving it: on any device. One
//! forgotten is forgotten on the server's computer, with the server stopped: `freya-server
//! --forget-password`; the server is then adopted again.
//!
//! Devices log in in the browser, as OAuth's authorization code with PKCE has it: the
//! login page gives a code for the password, which only the client that asked for it can trade
//! for tokens, having the secret its challenge was made from.
//!
//! A device calls with an access token, which lasts an hour: its id and when it ends, signed
//! with the server's own secret, so nothing is kept of it and it outlasts a restart. It renews
//! it with a refresh token, which changes each time. A refresh token already used, coming
//! again, was copied: the device's tokens are all taken back, so whoever has them logs in again.
//! The one renewed last is taken once more, within a day, for a client whose answer was lost;
//! its successor, which that client never had, is used up then. A device connected before
//! refresh tokens renews with the token it has.
//!
//! A new device first knows the server by a connection code, shown on the server's computer:
//! the server proves it knows the code with an HMAC, keyed by the code stretched with PBKDF2
//! and salted with the server's certificate, over the device's nonce and that certificate. A
//! code lasts ten minutes; finding it from a proof would take thousands of years, so asking
//! for proofs again neither helps nor uses it up. Passwords are set and changed in
//! the browser too, on a page a device's ticket opens.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use freya_protocol::{AccountInfo, Claim, DeviceInfo, Joined, Role, UserInfo};
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
/// The most tries remembered, and addresses users logged in from: past it, the oldest go, so
/// made-up names and addresses cannot fill the memory.
const MAX_TRIED: usize = 100_000;
const MAX_KNOWN: usize = 1_000;
/// How much of a name typed counts.
const MAX_NAME: usize = 128;
/// The user a server starts with, who owns it.
const OWNER: u32 = 1;
/// How often when a device was last used is kept: no more than this, so calling the server
/// does not write the accounts every time.
const USED_EVERY: u64 = 60 * 60;
/// How long a connection code lasts, and how many proofs an address may ask before waiting.
const CONNECTION_CODE_LASTS: Duration = Duration::from_secs(10 * 60);
const PROOFS_FREE: u32 = 10;
/// How long after it was renewed a refresh token renews once more, for a client whose answer
/// was lost: a computer that slept the night through still does, one asking a day later not.
const RENEWED_GRACE: u64 = 24 * 60 * 60;
/// How long an access token lasts, in seconds.
pub const ACCESS_LASTS: u64 = 60 * 60;
/// How many used refresh tokens of a device are kept, to know them if they come again.
const RETIRED_KEPT: usize = 32;
/// How long a login's code may wait to be traded for a token, and a password page be used.
const CODE_LASTS: Duration = Duration::from_secs(120);
const TICKET_LASTS: Duration = Duration::from_secs(15 * 60);
/// How long the code a user is invited with lasts.
const INVITE_LASTS: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The longest name a user is given.
const LONGEST_USER_NAME: usize = 64;
/// The fewest characters a password has.
pub const SHORTEST_PASSWORD: usize = 8;

#[derive(Default, Serialize, Deserialize)]
struct Kept {
    users: Vec<User>,
    devices: Vec<Device>,
    /// What access tokens are signed with, hex.
    #[serde(default)]
    secret: String,
    /// The codes users were invited with, while they last.
    #[serde(default)]
    invites: Vec<Invite>,
    /// The id the next user is given: ids are never given twice, so nothing kept of a user
    /// taken away is taken for another's.
    #[serde(default)]
    next_user: u32,
}

impl Kept {
    /// An id never given before.
    fn new_user_id(&mut self) -> u32 {
        let id = self
            .next_user
            .max(self.users.iter().map(|u| u.id).max().unwrap_or(0) + 1);
        self.next_user = id + 1;
        id
    }
}

/// A user's invitation: the key of the code they were given, as a connection code's is made
/// (PBKDF2 of the code, salted with the certificate's SHA-256), never the code itself.
#[derive(Clone, Serialize, Deserialize)]
struct Invite {
    user: u32,
    /// Hex.
    key: String,
    /// Seconds since 1970.
    until: u64,
}

/// What a claim ticket lets the browser that comes with it do: adopt the server, or join it as
/// the user invited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Claimed {
    Adopt,
    Join(u32),
}

/// A claim ticket and what it was given for: the invitation's key, for joining, so it lasts no
/// longer than the invitation it was proved with.
struct ClaimFor {
    claimed: Claimed,
    invite: Option<String>,
}

/// A ticket a device was given for proving it knows a code, which opens the page claiming
/// the server, for a while.
struct ClaimTicket {
    /// The SHA-256 of the ticket, hex.
    ticket: String,
    claimed: Claimed,
    /// The key of the invitation it was proved with, hex, for joining.
    invite: Option<String>,
    until: Instant,
}

#[derive(Serialize, Deserialize)]
struct User {
    id: u32,
    name: String,
    password: Option<Password>,
    /// Admin for a user from before roles: the owner, the only one there was.
    #[serde(default)]
    role: Role,
    /// How often the password was set while the server ran: a login's code or a password
    /// page given before holds no longer once it was.
    #[serde(skip)]
    changed: u64,
}

impl User {
    fn set_password(&mut self, password: &str) {
        self.password = Some(Password::new(password));
        self.changed += 1;
    }
}

/// Whether the password of `user` is still the one it was when it had been set `changed`
/// times.
fn unchanged(kept: &Kept, user: u32, changed: u64) -> bool {
    kept.users
        .iter()
        .any(|u| u.id == user && u.changed == changed)
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
    /// How often the password had been set when the code was given.
    changed: u64,
    until: Instant,
}

/// What opens a password page: a device's user, for a while.
struct Ticket {
    /// The SHA-256 of the ticket, hex.
    ticket: String,
    user: u32,
    /// The device it was asked from, which stays logged in when the others are logged out.
    device: String,
    /// How often the password had been set when the ticket was given.
    changed: u64,
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
struct Tries(HashMap<String, Tried>, Option<Instant>);

/// How often the tries forgotten are swept out of the table.
const SWEEP_EVERY: Duration = Duration::from_secs(60);

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
    /// counted before any password is checked. Whether every key could be: those there already
    /// always are, and new ones while there is room, made among tries not waiting; waits still
    /// running are never dropped to make it.
    fn count(&mut self, keys: &[TriedFor]) -> bool {
        let now = Instant::now();
        // Those forgotten go now and then, however full the table is.
        if self
            .1
            .is_none_or(|swept| now.duration_since(swept) >= SWEEP_EVERY)
        {
            self.0.retain(|_, tried| now < tried.last + TRIES_FORGOTTEN);
            self.1 = Some(now);
        }
        if self.0.len() >= MAX_TRIED {
            let mut idle: Vec<(Instant, String)> = self
                .0
                .iter()
                .filter(|(_, tried)| tried.until <= now)
                .map(|(key, tried)| (tried.last, key.clone()))
                .collect();
            idle.sort();
            let excess = self.0.len() - MAX_TRIED / 2;
            for (_, key) in idle.into_iter().take(excess) {
                self.0.remove(&key);
            }
        }
        let mut all = true;
        for (key, free) in keys {
            if !self.0.contains_key(key) && self.0.len() >= MAX_TRIED {
                all = false;
                continue;
            }
            let tried = self.0.entry(key.clone()).or_insert(Tried {
                wrong: 0,
                until: now,
                last: now,
            });
            // Wrong ones a day ago are forgotten, swept or not.
            if now >= tried.last + TRIES_FORGOTTEN {
                tried.wrong = 0;
            }
            tried.wrong += 1;
            tried.last = now;
            if let Some(over) = tried.wrong.checked_sub(*free) {
                let wait = FIRST_WAIT.saturating_mul(1 << over.min(16));
                tried.until = now + wait.min(LONGEST_WAIT);
            }
        }
        all
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
    grants: Mutex<Vec<Grant>>,
    tickets: Mutex<Vec<Ticket>>,
    claims: Mutex<Vec<ClaimTicket>>,
    tries: Mutex<Tries>,
    home: Mutex<Tries>,
    proofs: Mutex<Tries>,
    /// The users who logged in from an address, by its network: tries for their name from
    /// elsewhere do not hold them up there.
    known: Mutex<HashSet<(u32, String)>>,
    /// The connection code shown now, until when, and its key once derived.
    connection: Mutex<Option<Connection>>,
}

impl Accounts {
    /// The accounts kept in `file`; a server without any starts with its owner, who has no
    /// password yet: it is not adopted.
    pub fn open(file: Option<PathBuf>) -> Self {
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
                changed: 0,
            });
        }
        if unhex(&kept.secret).is_none_or(|secret| secret.len() < 32) {
            kept.secret = random_hex(32);
        }
        // Only a device that logged in with a password, of a user who has one, stays: one paired
        // on the server's computer, as until 0.3, or from before how it joined was kept, logged in
        // without one.
        let with_password: HashSet<u32> = kept
            .users
            .iter()
            .filter(|u| u.password.is_some())
            .map(|u| u.id)
            .collect();
        kept.devices
            .retain(|d| d.joined == Some(Joined::LoggedIn) && with_password.contains(&d.user));
        let accounts = Self {
            file,
            kept: Mutex::new(kept),
            grants: Mutex::new(Vec::new()),
            tickets: Mutex::new(Vec::new()),
            claims: Mutex::new(Vec::new()),
            tries: Mutex::new(Tries::default()),
            home: Mutex::new(Tries::default()),
            proofs: Mutex::new(Tries::default()),
            known: Mutex::new(HashSet::new()),
            connection: Mutex::new(None),
        };
        accounts.save(&accounts.kept());
        accounts
    }

    /// Whether the accounts in `file` can be read, when there are any: a server does not
    /// start without them, rather than start anew and lose its users, devices and secret.
    pub fn readable(file: &Path) -> Result<(), String> {
        match std::fs::read_to_string(file) {
            Ok(text) => serde_json::from_str::<Kept>(&text).map(drop).map_err(|e| {
                format!(
                    "the accounts in {} cannot be read ({e}); the server does not start without them",
                    file.display()
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("the accounts in {} cannot be read: {e}", file.display())),
        }
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

    /// The user `name` is, given their password, tried from `from`. A wrong one holds the
    /// answer up; after a few, for that name or from that address, none is tried for a while.
    ///
    /// Whether the name is a user's does not show: a made-up one is counted and waited for as
    /// a user's, and a password is checked against a hash of nothing for it, as long.
    /// With the user, how often their password had been set when it was checked.
    fn check(
        &self,
        name: &str,
        password: &str,
        from: Option<IpAddr>,
    ) -> Result<(u32, u64), Refused> {
        // A name is counted by its first characters, so a long one fills nothing.
        let typed: String = name.trim().to_lowercase().chars().take(MAX_NAME).collect();
        let (id, hashed, changed) = {
            let kept = self.kept();
            let user = kept.users.iter().find(|u| u.name.to_lowercase() == typed);
            (
                user.map(|u| u.id),
                user.and_then(|u| u.password.clone()),
                user.map_or(0, |u| u.changed),
            )
        };
        let network = from.map(network);
        let known = |id: u32, network: &String| {
            self.known
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&(id, network.clone()))
        };
        let named = format!("user:{typed}");
        let at_home = id
            .zip(network.as_ref())
            .is_some_and(|(id, network)| known(id, network));
        let keys: Vec<TriedFor> = if at_home {
            // Where the user logged in before, only the tries for their name there count: from
            // this very address, and from its network, more of them, so a guesser elsewhere
            // never holds them up, one beside them only after many, and one moving from
            // address to address within the network gains nothing.
            let mut keys: Vec<TriedFor> = from
                .iter()
                .map(|ip| (format!("{named}@{ip}"), FREE_TRIES))
                .collect();
            if let Some(network) = &network {
                keys.push((format!("{named}@{network}"), FREE_TRIES_FOR_A_NAME));
            }
            keys
        } else {
            let mut keys = vec![(named.clone(), FREE_TRIES_FOR_A_NAME)];
            if let Some(network) = &network {
                keys.push((format!("from:{network}"), FREE_TRIES));
                keys.push((format!("{named}@{network}"), FREE_TRIES));
            }
            keys
        };
        {
            let mut tries = self.tries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(wait) = tries.wait(&keys) {
                return Err(Refused::Wait(wait));
            }
            if !tries.count(&keys) {
                // With the table full of waits, a user is let try only where they logged in
                // before, counted in a table of their own: a guesser filling it locks nobody out
                // of home, and gains no tries there either.
                if !at_home {
                    return Err(Refused::Wait(FIRST_WAIT));
                }
                let mut home = self.home.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(wait) = home.wait(&keys) {
                    return Err(Refused::Wait(wait));
                }
                if !home.count(&keys) {
                    return Err(Refused::Wait(FIRST_WAIT));
                }
            }
        }
        let right = match &hashed {
            Some(hashed) => hashed.check(password),
            None => {
                no_password().check(password);
                false
            }
        };
        match (id, right) {
            (Some(id), true) => {
                self.tries
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .right(&keys);
                self.home
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .right(&keys);
                if let Some(network) = network {
                    let mut known = self.known.lock().unwrap_or_else(|e| e.into_inner());
                    if known.len() >= MAX_KNOWN
                        && let Some(any) = known.iter().next().cloned()
                    {
                        known.remove(&any);
                    }
                    known.insert((id, network));
                }
                Ok((id, changed))
            }
            _ => {
                std::thread::sleep(WRONG_PASSWORD_WAIT);
                Err(Refused::Wrong)
            }
        }
    }

    /// Forgets the password of the user the server started with, in the accounts kept in
    /// `file`, while no server holds them: their name. The server is adopted again, by whoever
    /// proves they know the code it shows. The devices that logged in with it are logged out:
    /// one of them may be whoever learned it.
    pub fn forget_owner_password(file: &Path) -> Result<String, String> {
        let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
        let mut kept: Kept = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let owner = kept
            .users
            .iter_mut()
            .find(|u| u.id == OWNER)
            .ok_or("no owner in the accounts")?;
        owner.password = None;
        let name = owner.name.clone();
        kept.devices.retain(|d| d.user != OWNER);
        let json = serde_json::to_vec_pretty(&kept).map_err(|e| e.to_string())?;
        write_private(file, &json).map_err(|e| e.to_string())?;
        Ok(name)
    }

    /// A user named `name` with the role `role`, and the password they start with: their id.
    pub fn add_user(&self, name: &str, role: Role, password: &str) -> Result<u32, String> {
        let name: String = name.trim().chars().filter(|c| !c.is_control()).collect();
        let name = name.as_str();
        if password.chars().count() < SHORTEST_PASSWORD {
            return Err(format!(
                "A password needs at least {SHORTEST_PASSWORD} characters."
            ));
        }
        let mut kept = self.kept();
        if name.is_empty() || kept.users.iter().any(|u| u.name.eq_ignore_ascii_case(name)) {
            return Err(format!("There is a user named {name} already."));
        }
        let id = kept.new_user_id();
        kept.users.push(User {
            id,
            name: name.to_owned(),
            password: Some(Password::new(password)),
            role,
            changed: 0,
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
        let (user, changed) = self.check(name, password, from)?;
        let code = random_hex(32);
        let mut grants = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        grants.retain(|g| g.until > Instant::now());
        grants.push(Grant {
            code: sha256(code.as_bytes()),
            user,
            challenge: challenge.to_owned(),
            redirect: redirect.to_owned(),
            changed,
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
            self.add_device(grant.user, device, Joined::LoggedIn, Some(grant.changed))
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
        let renewed = |d: &Device| d.previous.as_ref().is_some_and(|p| p.token == hash);
        // Its answer was lost, by a computer that slept or a network that dropped it: renewed
        // again, once, within a day, as the tokens it never had were not used either. Later,
        // it is taken for a copy.
        let lost = |d: &Device| {
            renewed(d)
                && d.previous
                    .as_ref()
                    .is_some_and(|p| now <= p.at + RENEWED_GRACE)
        };
        if let Some(at) = kept.devices.iter().position(lost) {
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
            .position(|d| d.retired.contains(&hash) || renewed(d))
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

    /// The proofs that this server knows the codes a device may type, for the device that
    /// sent `nonce` and was given `certificate`: the connection code shown, while it lasts, and
    /// each code a user was invited with. A code's key is derived once, so asking again costs
    /// the server little.
    pub fn prove(&self, nonce: &[u8], certificate: &[u8]) -> Vec<Vec<u8>> {
        let digest = ring::digest::digest(&ring::digest::SHA256, certificate);
        let message = freya_protocol::connection_message(nonce, digest.as_ref());
        self.code_keys(certificate)
            .iter()
            .map(|(key, _)| ring::hmac::sign(key, &message).as_ref().to_vec())
            .collect()
    }

    /// The keys of the codes a device may type now, each with what knowing it lets the device
    /// do: the connection code's, to adopt the server while it is not adopted, and each
    /// invitation's.
    fn code_keys(&self, certificate: &[u8]) -> Vec<(ring::hmac::Key, Option<ClaimFor>)> {
        let digest = ring::digest::digest(&ring::digest::SHA256, certificate);
        let mut keys = Vec::new();
        {
            let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(shown) = connection.as_mut().filter(|c| c.until > Instant::now()) {
                let key = shown.key.get_or_insert_with(|| {
                    let derived = freya_protocol::connection_key(&shown.code, digest.as_ref());
                    ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &derived)
                });
                let adopt = (!self.adopted()).then_some(ClaimFor {
                    claimed: Claimed::Adopt,
                    invite: None,
                });
                keys.push((key.clone(), adopt));
            }
        }
        let now = now();
        for invite in self.kept().invites.iter().filter(|i| i.until > now) {
            if let Some(key) = unhex(&invite.key) {
                let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key);
                let join = ClaimFor {
                    claimed: Claimed::Join(invite.user),
                    invite: Some(invite.key.clone()),
                };
                keys.push((key, Some(join)));
            }
        }
        keys
    }

    /// Whether the server was adopted: its owner chose a password.
    pub fn adopted(&self) -> bool {
        self.kept()
            .users
            .iter()
            .any(|u| u.id == OWNER && u.password.is_some())
    }

    /// The connection code a server not adopted yet keeps, for a device to adopt it with: the
    /// one shown now while it lasts, else a new one; how long it lasts. None once adopted.
    pub fn adoption_code(&self) -> Option<(String, Duration)> {
        if self.adopted() {
            return None;
        }
        {
            let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(shown) = connection.as_ref()
                && let Some(left) = shown.until.checked_duration_since(Instant::now())
                && !left.is_zero()
            {
                return Some((shown.code.clone(), left));
            }
        }
        Some(self.connection_code())
    }

    /// A ticket for the device that sent `nonce`, was given `certificate`, and proves with
    /// `proof` that it knows a code that lets it claim the server: adopting it with its
    /// connection code while it is not adopted, or joining it with an invitation's. None for
    /// any other proof.
    pub fn claim(&self, nonce: &[u8], proof: &[u8], certificate: &[u8]) -> Option<(String, Claim)> {
        let digest = ring::digest::digest(&ring::digest::SHA256, certificate);
        let message = freya_protocol::claim_message(nonce, digest.as_ref());
        let claimed = self
            .code_keys(certificate)
            .into_iter()
            .find(|(key, _)| ring::hmac::verify(key, &message, proof).is_ok())?
            .1?;
        let ClaimFor { claimed, invite } = claimed;
        let claim = match claimed {
            Claimed::Adopt => Claim::Adopt,
            Claimed::Join(user) => Claim::Join {
                user: self
                    .kept()
                    .users
                    .iter()
                    .find(|u| u.id == user)?
                    .name
                    .clone(),
            },
        };
        let ticket = random_hex(32);
        let mut claims = self.claims.lock().unwrap_or_else(|e| e.into_inner());
        claims.retain(|c| c.until > Instant::now());
        claims.push(ClaimTicket {
            ticket: sha256(ticket.as_bytes()),
            claimed,
            invite,
            until: Instant::now() + TICKET_LASTS,
        });
        Some((ticket, claim))
    }

    /// What the claim `ticket` lets its page do, while it does: adopt the server, with the
    /// owner's name now to start from, or join it as the user named.
    pub fn claim_page(&self, ticket: &str) -> Option<(Claim, String)> {
        let (claimed, invite) = {
            let claims = self.claims.lock().unwrap_or_else(|e| e.into_inner());
            let hash = sha256(ticket.as_bytes());
            let found = claims
                .iter()
                .find(|c| c.ticket == hash && c.until > Instant::now())?;
            (found.claimed, found.invite.clone())
        };
        let kept = self.kept();
        // An invitation replaced, used or taken back since takes its tickets with it.
        if let Some(invite) = &invite
            && !lasts(&kept, invite)
        {
            return None;
        }
        match claimed {
            Claimed::Adopt => {
                let owner = kept.users.iter().find(|u| u.id == OWNER)?;
                // Adopted meanwhile, from another device.
                if owner.password.is_some() {
                    return None;
                }
                Some((Claim::Adopt, owner.name.clone()))
            }
            Claimed::Join(user) => {
                let user = kept.users.iter().find(|u| u.id == user)?;
                Some((
                    Claim::Join {
                        user: user.name.clone(),
                    },
                    user.name.clone(),
                ))
            }
        }
    }

    /// Claims the server with the `ticket`: adopting it, its owner named `name`, or joining it
    /// as the user invited, with `password`. Then a code for the device's login, as
    /// [`Self::grant`] gives one for a password; the ticket and the invitation are used up.
    pub fn complete_claim(
        &self,
        ticket: &str,
        name: &str,
        password: &str,
        challenge: &str,
        redirect: &str,
    ) -> Result<String, String> {
        let out_of_date = "This page is out of date. Start again from Freya.";
        let (claim, _) = self.claim_page(ticket).ok_or(out_of_date)?;
        if password.chars().count() < SHORTEST_PASSWORD {
            return Err(format!(
                "A password needs at least {SHORTEST_PASSWORD} characters."
            ));
        }
        let (claimed, invite) = {
            let mut claims = self.claims.lock().unwrap_or_else(|e| e.into_inner());
            let hash = sha256(ticket.as_bytes());
            let at = claims
                .iter()
                .position(|c| c.ticket == hash)
                .ok_or(out_of_date)?;
            let used = claims.swap_remove(at);
            (used.claimed, used.invite)
        };
        let mut kept = self.kept();
        // Checked again under the lock it is written under: adopted meanwhile, or the
        // invitation replaced or taken back, the claim is out of date.
        let adopted = kept
            .users
            .iter()
            .any(|u| u.id == OWNER && u.password.is_some());
        if (claimed == Claimed::Adopt && adopted)
            || invite.as_ref().is_some_and(|invite| !lasts(&kept, invite))
        {
            return Err(out_of_date.into());
        }
        // Whether a password was replaced: what was given for the old one goes too.
        let mut reset = false;
        let (user, changed) = match (claimed, claim) {
            (Claimed::Adopt, Claim::Adopt) => {
                let name = user_name(name)?;
                let taken = kept
                    .users
                    .iter()
                    .any(|u| u.id != OWNER && u.name.eq_ignore_ascii_case(&name));
                if taken {
                    return Err(format!("There is a user named {name} already."));
                }
                let owner = kept
                    .users
                    .iter_mut()
                    .find(|u| u.id == OWNER)
                    .ok_or(out_of_date)?;
                owner.name = name;
                owner.set_password(password);
                log::info!("the server was adopted by {}", owner.name);
                (OWNER, owner.changed)
            }
            (Claimed::Join(user), _) => {
                let joined = kept
                    .users
                    .iter_mut()
                    .find(|u| u.id == user)
                    .ok_or(out_of_date)?;
                // A new password for one who had one: their devices log in again with it.
                let had_one = joined.password.is_some();
                joined.set_password(password);
                log::info!("{} joined the server", joined.name);
                let changed = joined.changed;
                kept.invites.retain(|i| i.user != user);
                if had_one {
                    kept.devices.retain(|d| d.user != user);
                    reset = true;
                }
                (user, changed)
            }
            _ => return Err(out_of_date.into()),
        };
        self.save(&kept);
        drop(kept);
        // The other tickets for the same claim are out of date now; and for a password
        // replaced, the codes and password pages given while the old one held (those given
        // as it is replaced do not hold either, as their password changed).
        self.claims
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|c| c.claimed != claimed);
        if reset {
            self.forget_given(user);
        }
        let code = random_hex(32);
        let mut grants = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        grants.retain(|g| g.until > Instant::now());
        grants.push(Grant {
            code: sha256(code.as_bytes()),
            user,
            challenge: challenge.to_owned(),
            redirect: redirect.to_owned(),
            changed,
            until: Instant::now() + CODE_LASTS,
        });
        Ok(code)
    }

    /// Invites a user named `name` with `role`: the code they join with, from their own
    /// device, for a server with `certificate`; how long it lasts. They choose their password
    /// when they join.
    pub fn invite_user(
        &self,
        name: &str,
        role: Role,
        certificate: &[u8],
    ) -> Result<(String, Duration), String> {
        let name = user_name(name)?;
        let id = {
            let mut kept = self.kept();
            if kept
                .users
                .iter()
                .any(|u| u.name.eq_ignore_ascii_case(&name))
            {
                return Err(format!("There is a user named {name} already."));
            }
            let id = kept.new_user_id();
            kept.users.push(User {
                id,
                name,
                password: None,
                role,
                changed: 0,
            });
            self.save(&kept);
            id
        };
        Ok(self.invite(id, certificate))
    }

    /// Invites the user `name` again, for `caller`, with a new code in place of any before: one
    /// who has not joined, or has forgotten their password, which the new one replaces when
    /// they join. Never the owner, whose password is reset on the server's computer; an admin
    /// only by the owner, so no admin takes another's account.
    pub fn invite_again(
        &self,
        caller: &Caller,
        name: &str,
        certificate: &[u8],
    ) -> Result<(String, Duration), String> {
        let id = self.other_user(caller, name)?;
        Ok(self.invite(id, certificate))
    }

    /// The user `name`, for `caller` to invite again or take away: not the owner, and an admin
    /// only for the owner.
    fn other_user(&self, caller: &Caller, name: &str) -> Result<u32, String> {
        let kept = self.kept();
        let user = kept
            .users
            .iter()
            .find(|u| u.name.eq_ignore_ascii_case(name.trim()))
            .ok_or("There is no such user.")?;
        if user.id == OWNER {
            return Err(
                "The server's owner stays, and changes their password on its computer.".into(),
            );
        }
        if user.role == Role::Admin && caller.user != OWNER {
            return Err("Only the server's owner may do that to an admin.".into());
        }
        Ok(user.id)
    }

    fn invite(&self, user: u32, certificate: &[u8]) -> (String, Duration) {
        let code: String = random_bytes(freya_protocol::CODE_LENGTH)
            .iter()
            .map(|b| freya_protocol::CODE_ALPHABET[usize::from(b & 31)] as char)
            .collect();
        let digest = ring::digest::digest(&ring::digest::SHA256, certificate);
        let key = freya_protocol::connection_key(&code, digest.as_ref());
        let mut kept = self.kept();
        let now = now();
        kept.invites.retain(|i| i.user != user && i.until > now);
        kept.invites.push(Invite {
            user,
            key: key.iter().map(|b| format!("{b:02x}")).collect(),
            until: now + INVITE_LASTS.as_secs(),
        });
        self.save(&kept);
        (code, INVITE_LASTS)
    }

    /// Takes the user `name` away, for `caller`, with their devices, invitation, and whatever
    /// was given them to log in with: not the owner, and an admin only for the owner.
    pub fn remove_user(&self, caller: &Caller, name: &str) -> Result<(), String> {
        let id = self.other_user(caller, name)?;
        let mut kept = self.kept();
        kept.users.retain(|u| u.id != id);
        kept.devices.retain(|d| d.user != id);
        kept.invites.retain(|i| i.user != id);
        self.save(&kept);
        drop(kept);
        self.claims
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|c| c.claimed != Claimed::Join(id));
        self.forget_given(id);
        Ok(())
    }

    /// Forgets the login codes and password pages given to `user`, not traded or used yet.
    fn forget_given(&self, user: u32) {
        self.tickets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|t| t.user != user);
        self.grants
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|g| g.user != user);
    }

    /// The server's users, the owner first.
    pub fn users(&self) -> Vec<UserInfo> {
        let kept = self.kept();
        let now = now();
        let mut users: Vec<UserInfo> = kept
            .users
            .iter()
            .map(|u| UserInfo {
                name: u.name.clone(),
                role: u.role,
                joined: u.password.is_some(),
                invited_until: kept
                    .invites
                    .iter()
                    .find(|i| i.user == u.id && i.until > now)
                    .map(|i| SystemTime::UNIX_EPOCH + Duration::from_secs(i.until)),
                devices: kept.devices.iter().filter(|d| d.user == u.id).count() as u32,
                owner: u.id == OWNER,
            })
            .collect();
        users.sort_by_key(|u| (!u.owner, u.name.to_lowercase()));
        users
    }

    /// Whether a device at `from` may ask for another proof now: a few a minute.
    pub fn may_ask_proof(&self, from: Option<IpAddr>) -> bool {
        let Some(from) = from else {
            return true;
        };
        // Counted apart from logins, by address, so asking for proofs fills nothing logins need,
        // nor holds up another on the same network.
        let key = [(format!("proof:{from}"), PROOFS_FREE)];
        let mut tries = self.proofs.lock().unwrap_or_else(|e| e.into_inner());
        if tries.wait(&key).is_some() {
            return false;
        }
        tries.count(&key)
    }

    /// A ticket that opens the password page of `caller`'s user; `local` when they call from
    /// the server's own computer.
    pub fn ticket(&self, caller: &Caller) -> String {
        let ticket = random_hex(32);
        let changed = self
            .kept()
            .users
            .iter()
            .find(|u| u.id == caller.user)
            .map_or(0, |u| u.changed);
        let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        tickets.retain(|t| t.until > Instant::now());
        tickets.push(Ticket {
            ticket: sha256(ticket.as_bytes()),
            user: caller.user,
            device: caller.device.clone(),
            changed,
            until: Instant::now() + TICKET_LASTS,
        });
        ticket
    }

    /// Whose password page `ticket` opens, while it does.
    pub fn password_page(&self, ticket: &str) -> Option<PasswordPage> {
        let (user, changed) = {
            let tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
            let hash = sha256(ticket.as_bytes());
            let ticket = tickets
                .iter()
                .find(|t| t.ticket == hash && t.until > Instant::now())?;
            (ticket.user, ticket.changed)
        };
        let kept = self.kept();
        let user = kept
            .users
            .iter()
            .find(|u| u.id == user && u.changed == changed)?;
        Some(PasswordPage {
            user: user.name.clone(),
            asks_current: user.password.is_some(),
        })
    }

    /// Changes the password of the user `ticket` is for to `new`, given the one now when the
    /// page asks for it; the ticket is used up once it did.
    /// With `log_out_others`, every other device of the user is logged out too, but the one the
    /// page was opened from.
    pub fn change_password(
        &self,
        ticket: &str,
        current: &str,
        new: &str,
        from: Option<IpAddr>,
        log_out_others: bool,
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
        let used = {
            let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
            let hash = sha256(ticket.as_bytes());
            let at = tickets
                .iter()
                .position(|t| t.ticket == hash)
                .ok_or("This page is out of date. Open it again from Freya's settings.")?;
            tickets.swap_remove(at)
        };
        self.set_password(used.user, new, used.changed)?;
        if log_out_others {
            let mut kept = self.kept();
            let before = kept.devices.len();
            kept.devices
                .retain(|d| d.user != used.user || d.id == used.device);
            log::info!(
                "the password changed, and {} other devices were logged out",
                before - kept.devices.len()
            );
            self.save(&kept);
        }
        Ok(())
    }

    /// Sets the password `user` logs in with on other devices.
    /// Sets the password of `user` while it is the one it was when it had been set `changed`
    /// times.
    fn set_password(&self, user: u32, password: &str, changed: u64) -> Result<(), String> {
        let mut kept = self.kept();
        let user = kept
            .users
            .iter_mut()
            .find(|u| u.id == user && u.changed == changed)
            .ok_or("This page is out of date. Open it again from Freya's settings.")?;
        user.set_password(password);
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

    /// With `changed`, only while the user's password is the one it was then.
    fn add_device(
        &self,
        user: u32,
        name: &str,
        joined: Joined,
        changed: Option<u64>,
    ) -> Option<Granted> {
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
        let mut kept = self.kept();
        if changed.is_some_and(|changed| !unchanged(&kept, user, changed)) {
            return None;
        }
        log::info!("a device was connected: {}", device.name);
        kept.devices.push(device);
        let at = kept.devices.len() - 1;
        let granted = issue(&mut kept, at);
        self.save(&kept);
        granted
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

/// The network an address is in, as tries are counted: an IPv6 address by its /64, which one
/// computer may change at will within.
fn network(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(ip) => ip.to_string(),
            // Every link-local address is in fe80::/64: those count one by one.
            None if ip.is_unicast_link_local() => ip.to_string(),
            None => {
                let s = ip.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
    }
}

/// A password of nothing, which a made-up name's is checked against, so the check takes as
/// long as a user's.
fn no_password() -> &'static Password {
    static NONE: std::sync::OnceLock<Password> = std::sync::OnceLock::new();
    NONE.get_or_init(|| Password::new(&random_hex(16)))
}

/// Whether the invitation whose key is `key` lasts still, in `kept`.
fn lasts(kept: &Kept, key: &str) -> bool {
    let now = now();
    kept.invites.iter().any(|i| i.key == key && i.until > now)
}

/// A user's name as given: trimmed, without control characters, at most
/// [`LONGEST_USER_NAME`] characters; an error for none.
fn user_name(name: &str) -> Result<String, String> {
    let name: String = name.trim().chars().filter(|c| !c.is_control()).collect();
    if name.is_empty() {
        return Err("A user needs a name.".into());
    }
    if name.chars().count() > LONGEST_USER_NAME {
        return Err(format!(
            "A name has at most {LONGEST_USER_NAME} characters."
        ));
    }
    Ok(name)
}

/// Seconds since 1970.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A device's name as kept: its first 100 characters, without control characters, which
/// would forge lines in the log.
fn device_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(100).collect()
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
    // A new file, never one already there, nor through a link put there in its place.
    match std::fs::remove_file(&partial) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
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

    /// Proves to `accounts` knowing `code`, for a server with `certificate`: what may be done
    /// with it.
    fn claim_with(accounts: &Accounts, code: &str, certificate: &[u8]) -> Option<(String, Claim)> {
        let digest = ring::digest::digest(&ring::digest::SHA256, certificate);
        let key = freya_protocol::connection_key(code, digest.as_ref());
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key);
        let nonce = [7; 32];
        let proof = ring::hmac::sign(
            &key,
            &freya_protocol::claim_message(&nonce, digest.as_ref()),
        );
        accounts.claim(&nonce, proof.as_ref(), certificate)
    }

    fn caller(user: u32, role: Role) -> Caller {
        Caller {
            user,
            device: String::new(),
            role,
        }
    }

    const REDIRECT: &str = "http://127.0.0.1:47477/callback";
    const VERIFIER: &str = "a secret only the client knows, long enough";

    /// Adopts the server, with the code it shows, as its owner `name` with `password`: the
    /// tokens of the first device, `device`, as its login in the browser gives them.
    fn adopt(accounts: &Accounts, name: &str, password: &str, device: &str) -> Granted {
        let (code, _) = accounts.adoption_code().unwrap();
        let (ticket, claim) = claim_with(accounts, &code, b"the server's certificate").unwrap();
        assert!(matches!(claim, Claim::Adopt));
        let code = accounts
            .complete_claim(&ticket, name, password, &challenge(VERIFIER), REDIRECT)
            .unwrap();
        accounts.redeem(&code, VERIFIER, REDIRECT, device).unwrap()
    }

    /// `name`'s tokens for `device`, logged in with `password`.
    fn log_in(accounts: &Accounts, name: &str, password: &str, device: &str) -> Granted {
        let code = accounts
            .grant(name, password, &challenge(VERIFIER), REDIRECT, None)
            .unwrap();
        accounts.redeem(&code, VERIFIER, REDIRECT, device).unwrap()
    }

    #[test]
    fn admins_take_neither_the_owners_account_nor_each_others() {
        let accounts = Accounts::open(None);
        let certificate = b"the server's certificate";
        let redirect = "http://127.0.0.1:47477/callback";
        let owner = caller(OWNER, Role::Admin);
        let (code, _) = accounts
            .invite_user("Bo", Role::Admin, certificate)
            .unwrap();
        let (ticket, _) = claim_with(&accounts, &code, certificate).unwrap();
        accounts
            .complete_claim(&ticket, "", "correct horse", "challenge", redirect)
            .unwrap();
        let bo_id = accounts
            .kept()
            .users
            .iter()
            .find(|u| u.name == "Bo")
            .unwrap()
            .id;
        let bo = caller(bo_id, Role::Admin);
        accounts
            .invite_user("Kim", Role::Listener, certificate)
            .unwrap();
        accounts
            .invite_user("Eva", Role::Admin, certificate)
            .unwrap();
        let owner_name = accounts.users()[0].name.clone();

        // Another admin re-invites no owner, and no admin; a listener, yes.
        assert!(
            accounts
                .invite_again(&bo, &owner_name, certificate)
                .is_err()
        );
        assert!(accounts.invite_again(&bo, "Eva", certificate).is_err());
        assert!(accounts.remove_user(&bo, "Eva").is_err());
        assert!(accounts.remove_user(&bo, &owner_name).is_err());
        assert!(accounts.invite_again(&bo, "Kim", certificate).is_ok());
        // The owner does, but stays.
        assert!(accounts.invite_again(&owner, "Eva", certificate).is_ok());
        assert!(accounts.remove_user(&owner, &owner_name).is_err());

        // A ticket lasts no longer than the invitation it was proved with.
        let (first, _) = accounts.invite_again(&owner, "Kim", certificate).unwrap();
        let (ticket, _) = claim_with(&accounts, &first, certificate).unwrap();
        accounts.invite_again(&owner, "Kim", certificate).unwrap();
        assert_eq!(accounts.claim_page(&ticket), None);
        assert!(
            accounts
                .complete_claim(&ticket, "", "correct horse", "challenge", redirect)
                .is_err()
        );

        // A joined user invited again chooses a new password, and their devices log in again.
        let (again, _) = accounts.invite_again(&owner, "Bo", certificate).unwrap();
        let code = accounts
            .grant(
                "Bo",
                "correct horse",
                &challenge("a verifier long enough to be one"),
                redirect,
                None,
            )
            .unwrap();
        accounts
            .redeem(
                &code,
                "a verifier long enough to be one",
                redirect,
                "Laptop",
            )
            .unwrap();
        assert_eq!(
            accounts
                .users()
                .iter()
                .find(|u| u.name == "Bo")
                .unwrap()
                .devices,
            1
        );
        let waiting = accounts
            .grant(
                "Bo",
                "correct horse",
                &challenge("a verifier long enough to be one"),
                redirect,
                None,
            )
            .unwrap();
        let page = accounts.ticket(&bo);
        let (ticket, _) = claim_with(&accounts, &again, certificate).unwrap();
        accounts
            .complete_claim(&ticket, "", "battery staple", "challenge", redirect)
            .unwrap();
        assert_eq!(
            accounts
                .users()
                .iter()
                .find(|u| u.name == "Bo")
                .unwrap()
                .devices,
            0
        );
        // Nor do a login or a password page given while the old password held.
        assert!(
            accounts
                .redeem(
                    &waiting,
                    "a verifier long enough to be one",
                    redirect,
                    "Pad"
                )
                .is_none()
        );
        assert!(accounts.password_page(&page).is_none());

        // A user taken away leaves their id to nobody.
        let before = accounts
            .kept()
            .users
            .iter()
            .find(|u| u.name == "Kim")
            .unwrap()
            .id;
        accounts.remove_user(&owner, "Kim").unwrap();
        accounts
            .invite_user("Kim", Role::Listener, certificate)
            .unwrap();
        let after = accounts
            .kept()
            .users
            .iter()
            .find(|u| u.name == "Kim")
            .unwrap()
            .id;
        assert!(after > before);
    }

    #[test]
    fn devices_log_in_and_are_revoked() {
        let dir = std::env::temp_dir().join(format!("freya-accounts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("accounts.json");
        let accounts = Accounts::open(Some(file.clone()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |f: &Path| std::fs::metadata(f).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&file), 0o600);
        }

        // Not adopted: nobody logs in without a password, the owner neither.
        let redirect = REDIRECT;
        let verifier = VERIFIER;
        let sent = challenge(verifier);
        let unadopted = accounts.users()[0].name.clone();
        assert_eq!(
            accounts.grant(&unadopted, "", &sent, redirect, None).ok(),
            None
        );
        let first = adopt(&accounts, "Alex", "correct horse", "Mac");
        assert!(accounts.adoption_code().is_none(), "adopted once");
        let mac = accounts.caller(&first.access).unwrap();
        assert_eq!(mac.user, OWNER);
        assert_eq!(accounts.caller("not a token"), None);
        assert_eq!(
            accounts.caller(&first.refresh),
            None,
            "a refresh token is not called with"
        );
        let saved = std::fs::read_to_string(&file).unwrap();
        assert!(!saved.contains(&first.refresh) && !saved.contains(&first.access));
        let owner = accounts.info(&mac).user;
        assert_eq!(owner, "Alex");

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

        // Once set, it is asked for to change it, from any device.
        let ticket = accounts.ticket(&mac);
        assert!(accounts.password_page(&ticket).unwrap().asks_current);
        let ticket = accounts.ticket(&pc);
        assert!(accounts.password_page(&ticket).unwrap().asks_current);
        assert!(
            accounts
                .change_password(&ticket, "wrong horse", "battery staple", None, false)
                .is_err()
        );
        accounts
            .change_password(&ticket, "correct horse", "battery staple", None, false)
            .unwrap();

        // Kept across starts.
        let again = Accounts::open(Some(file.clone()));
        let info = again.info(&mac);
        assert!(info.has_password);
        let names: Vec<&str> = info.devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Mac", "PC"]);
        assert!(info.devices[0].this);
        // Access tokens outlast a restart.
        assert_eq!(again.caller(&first.access).as_ref(), Some(&mac));
        assert!(again.revoke(&mac, &pc.device));
        assert_eq!(again.caller(&pc_tokens.access), None);
        assert_eq!(again.info(&mac).devices.len(), 1);
        drop(again);

        // A device paired on the server's computer, as until 0.3, or from before how it joined
        // was kept, logged in without a password: logged out as the server starts.
        let kept = std::fs::read_to_string(&file).unwrap();
        assert!(kept.contains("\"logged-in\""));
        for old in ["\"paired\"", "null"] {
            std::fs::write(&file, kept.replace("\"logged-in\"", old)).unwrap();
            let again = Accounts::open(Some(file.clone()));
            assert_eq!(again.caller(&first.access), None, "joined {old}");
        }
        // So is one of a user without a password, as an owner's whose was forgotten in the file.
        let mut without: serde_json::Value = serde_json::from_str(&kept).unwrap();
        without["users"][0]["password"] = serde_json::Value::Null;
        std::fs::write(&file, without.to_string()).unwrap();
        let again = Accounts::open(Some(file.clone()));
        assert_eq!(again.caller(&first.access), None, "no password");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_passwords_wait_longer_and_longer() {
        let accounts = Accounts::open(None);
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
        let _ = kim;
        let at = |ip: &str| {
            [
                ("user:kim".to_owned(), FREE_TRIES_FOR_A_NAME),
                (format!("from:{ip}"), FREE_TRIES),
                (format!("user:kim@{ip}"), FREE_TRIES),
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
        // Made-up names count as users' do, so neither the answer nor the wait tells them
        // apart.
        let mut tries = accounts.tries.lock().unwrap();
        tries.0.clear();
        drop(tries);
        assert_eq!(
            accounts.check("nobody", "x", elsewhere),
            Err(Refused::Wrong)
        );
        assert!(accounts.tries.lock().unwrap().0.contains_key("user:nobody"));
        // IPv6 addresses count by their /64.
        assert_eq!(
            network("2001:db8:1:2:aaaa::1".parse().unwrap()),
            network("2001:db8:1:2:bbbb::9".parse().unwrap())
        );
        assert_ne!(
            network("2001:db8:1:2::1".parse().unwrap()),
            network("2001:db8:1:3::1".parse().unwrap())
        );

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
        let kim = accounts.check("Kim", "correct horse", elsewhere).unwrap();
        // The right one forgets the name's tries from there.
        assert!(
            !accounts
                .tries
                .lock()
                .unwrap()
                .0
                .contains_key("user:kim@192.168.1.21")
        );
        // Where the user logged in before, a guesser elsewhere does not hold them up.
        {
            let mut tries = accounts.tries.lock().unwrap();
            for _ in 0..FREE_TRIES_FOR_A_NAME + 1 {
                tries.count(&[("user:kim".to_owned(), FREE_TRIES_FOR_A_NAME)]);
            }
        }
        assert_eq!(accounts.check("Kim", "correct horse", elsewhere), Ok(kim));
        assert!(matches!(
            accounts.check("Kim", "correct horse", Some(IpAddr::from([10, 0, 0, 9]))),
            Err(Refused::Wait(_))
        ));
    }

    #[test]
    fn forgetting_the_password_logs_out_who_logged_in_with_it() {
        let dir = std::env::temp_dir().join(format!("freya-forget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("accounts.json");
        let accounts = Accounts::open(Some(file.clone()));
        let mac = adopt(&accounts, "Alex", "correct horse", "Mac");
        let pc = log_in(&accounts, "Alex", "correct horse", "PC");
        drop(accounts);
        Accounts::forget_owner_password(&file).unwrap();
        // Every device that logged in with it is logged out, and the server is adopted again.
        let again = Accounts::open(Some(file));
        assert_eq!(again.caller(&mac.access), None);
        assert_eq!(again.caller(&pc.access), None);
        assert!(again.adoption_code().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_full_table_keeps_its_waits() {
        let mut tries = Tries::default();
        let key = |i: usize| [(format!("user:{i}"), 0)];
        // Full of waits: none is dropped, and no new try is taken.
        for i in 0..MAX_TRIED {
            assert!(tries.count(&key(i)));
        }
        assert!(!tries.count(&key(MAX_TRIED)));
        assert!(tries.wait(&key(0)).is_some(), "the oldest wait is kept");
        // One there already is still counted.
        let before = tries.0[&format!("user:{}", MAX_TRIED - 1)].wrong;
        assert!(tries.count(&key(MAX_TRIED - 1)));
        assert_eq!(
            tries.0[&format!("user:{}", MAX_TRIED - 1)].wrong,
            before + 1
        );
        // Tries not waiting make room.
        let mut idle = Tries::default();
        for i in 0..MAX_TRIED {
            assert!(idle.count(&[(format!("user:{i}"), FREE_TRIES)]));
        }
        assert!(idle.count(&key(MAX_TRIED)));
        assert!(idle.0.len() <= MAX_TRIED / 2 + 1);
        // Wrong ones a day ago are forgotten, the table swept or not.
        let mut old = Tries::default();
        let keys = [("user:kim".to_owned(), FREE_TRIES)];
        for _ in 0..FREE_TRIES + 3 {
            old.count(&keys);
        }
        // A computer up for less than a day has no instant a day ago: nothing to try there.
        let last = old.0["user:kim"].last;
        if let Some(day_ago) = last.checked_sub(TRIES_FORGOTTEN) {
            old.0.get_mut("user:kim").unwrap().last = day_ago;
            old.count(&keys);
            assert_eq!(old.0["user:kim"].wrong, 1);
        }
    }

    #[test]
    fn changing_the_password_may_log_the_other_devices_out() {
        let accounts = Accounts::open(None);
        accounts
            .add_user("Kim", Role::Listener, "correct horse")
            .unwrap();
        let redirect = "http://127.0.0.1:47477/callback";
        let verifier = "a secret only the client knows, long enough";
        let device = |name: &str| {
            let code = accounts
                .grant("Kim", "correct horse", &challenge(verifier), redirect, None)
                .unwrap();
            let tokens = accounts.redeem(&code, verifier, redirect, name).unwrap();
            accounts.caller(&tokens.access).unwrap();
            tokens
        };
        let (phone, pad) = (device("Phone"), device("Pad"));
        // A login checked with the old password, and a page opened for it, as it is changed.
        let late = accounts
            .grant("Kim", "correct horse", &challenge(verifier), redirect, None)
            .unwrap();
        let other = accounts.ticket(&accounts.caller(&pad.access).unwrap());
        let ticket = accounts.ticket(&accounts.caller(&phone.access).unwrap());
        accounts
            .change_password(&ticket, "correct horse", "battery staple", None, true)
            .unwrap();
        assert!(
            accounts.caller(&phone.access).is_some(),
            "the one it was changed on"
        );
        assert_eq!(accounts.caller(&pad.access), None);
        assert_eq!(accounts.redeem(&late, verifier, redirect, "Laptop"), None);
        assert_eq!(accounts.password_page(&other), None);
        assert!(
            accounts
                .change_password(&other, "battery staple", "staple battery", None, false)
                .is_err()
        );
    }

    #[test]
    fn a_connection_code_is_proved_while_it_lasts() {
        let accounts = Accounts::open(None);
        let (nonce, certificate) = ([7; 32], b"the server's certificate".as_slice());
        assert!(
            accounts.prove(&nonce, certificate).is_empty(),
            "no code shown"
        );
        let (code, lasts) = accounts.connection_code();
        assert_eq!(lasts, CONNECTION_CODE_LASTS);
        assert_eq!(code.len(), freya_protocol::CODE_LENGTH);
        assert_eq!(
            freya_protocol::normalize_code(&code).as_deref(),
            Some(code.as_str())
        );
        let proof = accounts.prove(&nonce, certificate).remove(0);
        let digest = ring::digest::digest(&ring::digest::SHA256, certificate);
        let derived = freya_protocol::connection_key(&code, digest.as_ref());
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &derived);
        let message = freya_protocol::connection_message(&nonce, digest.as_ref());
        assert!(ring::hmac::verify(&key, &message, &proof).is_ok());
        // Asking again is no use to anyone, and does not use the code up.
        assert!(!accounts.prove(&[8; 32], certificate).is_empty());
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
    fn unreadable_accounts_are_not_started_anew() {
        let dir = std::env::temp_dir().join(format!("freya-accounts-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("accounts.json");
        assert!(Accounts::readable(&file).is_ok(), "none yet");
        std::fs::write(&file, b"{ damaged").unwrap();
        assert!(Accounts::readable(&file).is_err());
        // Written anew, never through a link put where it writes first.
        #[cfg(unix)]
        {
            let elsewhere = dir.join("elsewhere");
            std::fs::write(&elsewhere, b"untouched").unwrap();
            std::os::unix::fs::symlink(&elsewhere, dir.join("accounts.partial")).unwrap();
            write_private(&file, b"{}").unwrap();
            assert_eq!(std::fs::read(&elsewhere).unwrap(), b"untouched");
            assert_eq!(std::fs::read(&file).unwrap(), b"{}");
        }
        assert_eq!(device_name("PC\nforged line"), "PCforged line");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_listener_logs_in_as_a_listener() {
        let accounts = Accounts::open(None);
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
        let accounts = Accounts::open(None);
        let first = adopt(&accounts, "Alex", "correct horse", "Mac");
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
        let tablet = log_in(&accounts, "Alex", "correct horse", "Tablet");
        let renewed = accounts.renew(&tablet.refresh).unwrap();
        assert!(accounts.renew(&tablet.refresh).is_some());
        assert_eq!(accounts.renew(&tablet.refresh), None);
        assert_eq!(accounts.caller(&renewed.access), None);

        // Forged or ended access tokens are not taken.
        let fresh = log_in(&accounts, "Alex", "correct horse", "Mac");
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
    }
}
