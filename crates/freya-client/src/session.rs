//! Staying logged in to a server: the access token a device calls with lasts an hour, and is
//! renewed with the refresh token, which the server changes each time. One renewal at a time,
//! so the same refresh token is never sent twice, which the server would take for a copy.
//!
//! Refresh tokens are kept in the system's keychain where Freya uses one (Windows' Credential
//! Manager, the Secret Service on Linux), else in a file only the user can read. Freya talks to
//! both itself, through the Windows bindings and the zbus it has already.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use freya_protocol::{ApiError, ApiResult};

use crate::{error, failed, read};

/// How long before an access token ends it is renewed.
const RENEW_BEFORE: Duration = Duration::from_secs(60);

/// The tokens a server gave this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Granted {
    pub access: String,
    pub refresh: String,
    /// How long the access token lasts.
    pub lasts: Duration,
}

impl Granted {
    /// The tokens in the server's answer, `(status, body)`.
    pub(crate) fn from_answer((status, body): (u16, Vec<u8>)) -> ApiResult<Self> {
        if status != 200 {
            return Err(error(status, &body));
        }
        let answer: serde_json::Value = serde_json::from_slice(&body).map_err(failed)?;
        // A server from before refresh tokens gives one token, called with as it is.
        if let Some(token) = answer["token"].as_str() {
            return Ok(Self {
                access: token.to_owned(),
                refresh: String::new(),
                lasts: Duration::from_secs(60 * 60),
            });
        }
        let text = |name: &str| {
            answer[name]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| ApiError(format!("the server gave no {name}")))
        };
        Ok(Self {
            access: text("access_token")?,
            refresh: text("refresh_token")?,
            lasts: Duration::from_secs(answer["expires_in"].as_u64().unwrap_or(0)),
        })
    }
}

/// Where this computer keeps the refresh tokens servers gave it, by the server's address.
#[derive(Clone, Debug)]
pub struct TokenStore {
    file: PathBuf,
    /// Whether the system's keychain is asked first.
    keychain: bool,
}

impl TokenStore {
    /// Kept in `file`, or in the system's keychain when `keychain` and there is one; never on
    /// macOS, where an app signed as Freya is each version would be asked for the user's
    /// password after every update.
    pub fn new(file: PathBuf, keychain: bool) -> Self {
        Self {
            file,
            keychain: keychain && cfg!(any(windows, target_os = "linux")),
        }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        if self.keychain
            && let Some(token) = keychain::get(key)
        {
            return Some(token);
        }
        let token = Tokens::load(&self.file).get(key)?;
        // Kept in the file before the keychain was used: moved there.
        if self.keychain && keychain::set(key, &token) {
            self.forget_in_file(key);
        }
        Some(token)
    }

    pub fn set(&self, key: &str, token: &str) {
        if self.keychain && keychain::set(key, token) {
            self.forget_in_file(key);
            return;
        }
        let mut tokens = Tokens::load(&self.file);
        if tokens.get(key).as_deref() != Some(token) {
            tokens.0.insert(key.to_owned(), token.to_owned());
            tokens.save(&self.file);
        }
    }

    pub fn remove(&self, key: &str) {
        if self.keychain {
            keychain::remove(key);
        }
        self.forget_in_file(key);
    }

    fn forget_in_file(&self, key: &str) {
        let mut tokens = Tokens::load(&self.file);
        if tokens.0.remove(key).is_some() {
            tokens.save(&self.file);
        }
    }
}

/// Windows' Credential Manager: an entry "Freya: <server>" of the user, on this computer.
#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "reads and writes Freya's own entries in Windows' Credential Manager"
)]
mod keychain {
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
        CredReadW, CredWriteW,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn target(key: &str) -> Vec<u16> {
        wide(&format!("Freya: {key}"))
    }

    pub fn get(key: &str) -> Option<String> {
        let target = target(key);
        let mut found: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: a NUL-terminated name; the entry Windows gives back is copied, then freed.
        unsafe {
            if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut found) == 0 || found.is_null()
            {
                return None;
            }
            let credential = &*found;
            let bytes = if credential.CredentialBlob.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(
                    credential.CredentialBlob,
                    credential.CredentialBlobSize as usize,
                )
                .to_vec()
            };
            CredFree(found.cast());
            String::from_utf8(bytes).ok()
        }
    }

    /// Whether it was kept there.
    pub fn set(key: &str, token: &str) -> bool {
        let (mut target, mut user) = (target(key), wide("Freya"));
        let mut blob = token.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: user.as_mut_ptr(),
            ..Default::default()
        };
        // SAFETY: the entry points at buffers alive for the call, the blob with its length.
        let written = unsafe { CredWriteW(&credential, 0) } != 0;
        if !written {
            log::warn!(
                "could not keep the server's token in the Credential Manager: {}",
                std::io::Error::last_os_error()
            );
        }
        written
    }

    pub fn remove(key: &str) {
        let target = target(key);
        // SAFETY: a NUL-terminated name.
        unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
    }
}

/// The Secret Service (GNOME Keyring, KWallet): an item with Freya's attributes, in the
/// default collection, over the zbus that Freya has already.
#[cfg(target_os = "linux")]
mod keychain {
    use std::collections::HashMap;

    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    const SERVICE: &str = "org.freedesktop.secrets";
    const PATH: &str = "/org/freedesktop/secrets";
    const DEFAULT_COLLECTION: &str = "/org/freedesktop/secrets/aliases/default";

    /// A secret as the Secret Service passes it: its session, parameters, value and type.
    type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

    struct Secrets {
        connection: Connection,
        session: OwnedObjectPath,
    }

    impl Secrets {
        fn open() -> zbus::Result<Self> {
            let connection = Connection::session()?;
            let service = Proxy::new(&connection, SERVICE, PATH, "org.freedesktop.Secret.Service")?;
            // Plain, as the specification allows: the secret goes over the user's own session
            // bus.
            let (_, session): (OwnedValue, OwnedObjectPath) =
                service.call("OpenSession", &("plain", Value::from("")))?;
            Ok(Self {
                connection,
                session,
            })
        }

        fn proxy<'a>(&'a self, path: &'a str, interface: &'a str) -> zbus::Result<Proxy<'a>> {
            Proxy::new(&self.connection, SERVICE, path, interface)
        }

        /// Freya's unlocked items for `key`.
        fn items(&self, key: &str) -> zbus::Result<Vec<OwnedObjectPath>> {
            let service = self.proxy(PATH, "org.freedesktop.Secret.Service")?;
            let (unlocked, _locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) =
                service.call("SearchItems", &(attributes(key),))?;
            Ok(unlocked)
        }
    }

    fn attributes(key: &str) -> HashMap<&str, &str> {
        HashMap::from([("application", "freya"), ("server", key)])
    }

    pub fn get(key: &str) -> Option<String> {
        let found = (|| -> zbus::Result<Option<String>> {
            let secrets = Secrets::open()?;
            let Some(item) = secrets.items(key)?.into_iter().next() else {
                return Ok(None);
            };
            let item = secrets.proxy(item.as_str(), "org.freedesktop.Secret.Item")?;
            let (_, _, value, _): Secret = item.call("GetSecret", &(&secrets.session,))?;
            Ok(String::from_utf8(value).ok())
        })();
        found
            .map_err(|e| log::debug!("no Secret Service: {e}"))
            .ok()
            .flatten()
    }

    /// Whether it was kept there: not when the keychain is locked, which asks the user.
    pub fn set(key: &str, token: &str) -> bool {
        let kept = (|| -> zbus::Result<bool> {
            let secrets = Secrets::open()?;
            let collection =
                secrets.proxy(DEFAULT_COLLECTION, "org.freedesktop.Secret.Collection")?;
            let properties = HashMap::from([
                (
                    "org.freedesktop.Secret.Item.Label",
                    Value::from(format!("Freya: {key}")),
                ),
                (
                    "org.freedesktop.Secret.Item.Attributes",
                    Value::from(attributes(key)),
                ),
            ]);
            let secret = (
                &secrets.session,
                Vec::<u8>::new(),
                token.as_bytes().to_vec(),
                "text/plain",
            );
            let (_, prompt): (OwnedObjectPath, OwnedObjectPath) =
                collection.call("CreateItem", &(properties, secret, true))?;
            Ok(prompt.as_str() == "/")
        })();
        match kept {
            Ok(kept) => kept,
            Err(e) => {
                log::debug!("no Secret Service: {e}");
                false
            }
        }
    }

    pub fn remove(key: &str) {
        let removed = (|| -> zbus::Result<()> {
            let secrets = Secrets::open()?;
            for item in secrets.items(key)? {
                let item = secrets.proxy(item.as_str(), "org.freedesktop.Secret.Item")?;
                let _prompt: OwnedObjectPath = item.call("Delete", &())?;
            }
            Ok(())
        })();
        if let Err(e) = removed {
            log::debug!("could not take the server's token from the Secret Service: {e}");
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod keychain {
    pub fn get(_key: &str) -> Option<String> {
        None
    }

    pub fn set(_key: &str, _token: &str) -> bool {
        false
    }

    pub fn remove(_key: &str) {}
}

/// The refresh tokens kept in a file, by the server's address.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Tokens(std::collections::BTreeMap<String, String>);

impl Tokens {
    fn load(file: &Path) -> Self {
        std::fs::read_to_string(file)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }

    fn save(&self, file: &Path) {
        let written = serde_json::to_vec_pretty(self)
            .map_err(std::io::Error::other)
            .and_then(|json| crate::write_private(file, &json));
        if let Err(e) = written {
            log::warn!("could not keep the server's token: {e}");
        }
    }
}

/// A device's tokens for one server, renewed as they end, and kept where `store` says.
pub struct Session {
    held: Mutex<Held>,
    agent: ureq::Agent,
    /// The server's `/token`.
    url: String,
    store: Option<(TokenStore, String)>,
}

struct Held {
    access: String,
    /// When the access token ends, by the clock: it goes on while the computer sleeps.
    until: SystemTime,
    refresh: String,
}

impl Session {
    /// Renews at `url` with `refresh`; calls with `granted`'s access token until then.
    pub(crate) fn new(
        agent: ureq::Agent,
        url: String,
        refresh: String,
        granted: Option<&Granted>,
        store: Option<(TokenStore, String)>,
    ) -> Self {
        let (access, until) = match granted {
            Some(granted) => (granted.access.clone(), SystemTime::now() + granted.lasts),
            None => (String::new(), SystemTime::UNIX_EPOCH),
        };
        Self {
            held: Mutex::new(Held {
                access,
                until,
                refresh,
            }),
            agent,
            url,
            store,
        }
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The access token to call with, renewed first when it ends soon; none when there is
    /// none and it could not be renewed.
    pub fn access(&self) -> Option<String> {
        let mut held = self.held();
        if SystemTime::now() + RENEW_BEFORE >= held.until {
            self.renew(&mut held);
        }
        (!held.access.is_empty()).then(|| held.access.clone())
    }

    /// After the server refused `rejected`: whether there is another access token to call with
    /// now, renewed unless another call renewed it already.
    pub fn renew_after(&self, rejected: &str) -> bool {
        let mut held = self.held();
        if held.access != rejected && !held.access.is_empty() {
            return true;
        }
        self.renew(&mut held)
    }

    /// Renews the tokens in `held`; whether it could.
    fn renew(&self, held: &mut Held) -> bool {
        if held.refresh.is_empty() {
            return false;
        }
        let asked = serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": held.refresh,
        });
        let answer = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json")
            .send(asked.to_string())
            .map_err(failed)
            .and_then(read)
            .and_then(Granted::from_answer);
        match answer {
            Ok(granted) => {
                if let Some((store, key)) = &self.store {
                    store.set(key, &granted.refresh);
                }
                held.access = granted.access;
                held.until = SystemTime::now() + granted.lasts;
                held.refresh = granted.refresh;
                true
            }
            Err(e) => {
                log::warn!("could not renew the server's token: {e}");
                // Refused: it is no use again. Not reached: tried again next time.
                if !e.0.starts_with("could not reach") {
                    held.access.clear();
                    held.refresh.clear();
                }
                false
            }
        }
    }
}

/// What the player asks for the access token to read a track with: the one now with `None`,
/// another after the server refused the one given.
pub type Bearer = Arc<dyn Fn(Option<&str>) -> Option<String> + Send + Sync>;

/// `session`'s tokens, for the player.
pub(crate) fn bearer(session: &Arc<Session>) -> Bearer {
    let session = Arc::clone(session);
    Arc::new(move |rejected| {
        if let Some(rejected) = rejected
            && !session.renew_after(rejected)
        {
            return None;
        }
        session.access()
    })
}
