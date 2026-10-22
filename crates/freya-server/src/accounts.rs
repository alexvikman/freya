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
//! for a token, having the secret its challenge was made from. Passwords are set and changed in
//! the browser too, on a page a device's ticket opens.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use freya_protocol::{AccountInfo, DeviceInfo};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

/// PBKDF2 rounds a password is hashed with, as OWASP asks of PBKDF2-HMAC-SHA256.
const ROUNDS: u32 = 600_000;
/// How long a wrong password holds up the answer, so guessing one takes long.
const WRONG_PASSWORD_WAIT: Duration = Duration::from_secs(1);
/// The user a server starts with, who owns it.
const OWNER: u32 = 1;
/// How long a login's code may wait to be traded for a token, and a password page be used.
const CODE_LASTS: Duration = Duration::from_secs(120);
const TICKET_LASTS: Duration = Duration::from_secs(15 * 60);
/// The fewest characters a password has.
pub const SHORTEST_PASSWORD: usize = 8;

#[derive(Default, Serialize, Deserialize)]
struct Kept {
    users: Vec<User>,
    devices: Vec<Device>,
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
    /// The SHA-256 of its token, hex.
    token: String,
    /// Seconds since 1970.
    created: u64,
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

    /// Who calls with `token`, if anyone.
    pub fn caller(&self, token: &str) -> Option<Caller> {
        let hash = sha256(token.as_bytes());
        self.kept()
            .devices
            .iter()
            .find(|d| d.token == hash)
            .map(|d| Caller {
                user: d.user,
                device: d.id.clone(),
            })
    }

    /// A token for a device named `name` of the owner, given the pairing key; none for
    /// another key. The key is made anew either way.
    pub fn pair(&self, key: &str, name: &str) -> Option<String> {
        let right = {
            let pairing = self.pairing.lock().unwrap_or_else(|e| e.into_inner());
            pairing.as_deref() == Some(sha256(key.as_bytes()).as_str())
        };
        self.new_pairing_key();
        right.then(|| self.add_device(OWNER, name))
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
    ) -> Option<String> {
        let grant = {
            let mut grants = self.grants.lock().unwrap_or_else(|e| e.into_inner());
            let hash = sha256(code.as_bytes());
            let at = grants.iter().position(|g| g.code == hash)?;
            grants.swap_remove(at)
        };
        let fits = grant.until > Instant::now()
            && grant.redirect == redirect
            && challenge(verifier) == grant.challenge;
        fits.then(|| self.add_device(grant.user, device))
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

    fn add_device(&self, user: u32, name: &str) -> String {
        let token = random_hex(32);
        let device = Device {
            id: random_hex(8),
            user,
            name: name.chars().take(100).collect(),
            token: sha256(token.as_bytes()),
            created: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        };
        log::info!("a device was connected: {}", device.name);
        let mut kept = self.kept();
        kept.devices.push(device);
        self.save(&kept);
        token
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
        let token = accounts.pair(&key, "Mac").unwrap();
        let mac = accounts.caller(&token).unwrap();
        assert_eq!(mac.user, OWNER);
        assert_eq!(accounts.caller("not a token"), None);
        assert!(!std::fs::read_to_string(&file).unwrap().contains(&token));

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
        let pc = accounts.redeem(&code, verifier, redirect, "PC").unwrap();
        let pc = accounts.caller(&pc).unwrap();

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
        assert!(again.revoke(&mac, &pc.device));
        assert_eq!(again.caller(&token).as_ref(), Some(&mac));
        assert_eq!(again.info(&mac).devices.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
