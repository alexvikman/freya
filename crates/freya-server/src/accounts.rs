//! Who may use the server: its users, each with a password once they set one, and the devices
//! they connected, each with a token of its own that can be revoked. Kept in a file in the
//! server's folder that only the user can read; of the tokens only their SHA-256 is kept, and
//! of the passwords their PBKDF2.
//!
//! A client on the server's own computer connects with the pairing key: a secret the server
//! writes to a file of its own, readable only by the user it runs as, and makes anew once
//! used, so only a program of that user, on that computer, connects so.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use freya_protocol::{AccountInfo, DeviceInfo};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

/// PBKDF2 rounds a password is hashed with, as OWASP asks of PBKDF2-HMAC-SHA256.
const ROUNDS: u32 = 600_000;
/// How long a wrong password holds up the answer, so guessing one takes long.
const WRONG_PASSWORD_WAIT: Duration = Duration::from_secs(1);
/// The user a server starts with, who owns it.
const OWNER: u32 = 1;

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

    /// A token for a device named `device` of the user `name`, given their password.
    pub fn login(&self, name: &str, password: &str, device: &str) -> Option<String> {
        let user = {
            let kept = self.kept();
            kept.users
                .iter()
                .find(|u| u.name.eq_ignore_ascii_case(name))
                .and_then(|u| Some((u.id, u.password.as_ref()?.check(password))))
        };
        match user {
            Some((id, true)) => Some(self.add_device(id, device)),
            _ => {
                std::thread::sleep(WRONG_PASSWORD_WAIT);
                None
            }
        }
    }

    /// Sets the password `user` logs in with on other devices.
    pub fn set_password(&self, user: u32, password: &str) -> Result<(), String> {
        if password.chars().count() < 8 {
            return Err("A password needs at least 8 characters".into());
        }
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
        assert_eq!(accounts.login(&owner, "", "PC"), None);
        assert!(accounts.set_password(OWNER, "short").is_err());
        accounts.set_password(OWNER, "correct horse").unwrap();
        assert_eq!(accounts.login(&owner, "wrong horse", "PC"), None);
        let pc = accounts.login(&owner, "correct horse", "PC").unwrap();
        let pc = accounts.caller(&pc).unwrap();

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
