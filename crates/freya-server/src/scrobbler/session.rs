//! Where the Last.fm session key is kept between runs: the Credential Manager on Windows, and
//! elsewhere a file in the server's folder that only the user can read. The key lets Freya
//! scrobble for the user until they disconnect, so it never goes in the settings.
//!
//! Not the keychain on macOS: it lets an app read its items without asking only while the app
//! keeps the signature it had when it saved them, and an app signed ad hoc, as Freya is
//! without a Developer ID, gets a new one with every build. Every update, and every start of
//! a development build, asked for the user's permission. The key is worth little: it can only
//! scrobble, and the user can revoke it on last.fm.

/// The name the key is kept under, with the user's Last.fm name as the account.
const SERVICE: &str = "Freya Last.fm";

use std::path::Path;

/// Keeps `key` as `user`'s session key; elsewhere than on Windows, in `dir`.
pub fn save(dir: &Path, user: &str, key: &str) -> Result<(), String> {
    platform::save(dir, user, key)
}

/// `user`'s session key: `Ok(None)` when none is kept, and an error when it could not be read,
/// as when the Credential Manager is not available: the session may still be there, so the
/// user is not forgotten for it.
pub fn load(dir: &Path, user: &str) -> Result<Option<String>, String> {
    platform::load(dir, user)
}

/// Forgets `user`'s session key.
pub fn delete(dir: &Path, user: &str) {
    platform::delete(dir, user);
}

/// The file `user`'s session key is kept in, in `dir`, elsewhere than on Windows.
pub fn file_name(user: &str) -> String {
    let name: String = user
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{SERVICE} {name}.key")
}

#[cfg(windows)]
mod platform {
    use std::path::Path;
    use std::ptr;

    use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
        CredReadW, CredWriteW,
    };

    use super::SERVICE;

    fn target(user: &str) -> Vec<u16> {
        format!("{SERVICE}/{user}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    #[allow(
        unsafe_code,
        reason = "calls the Credential Manager with buffers that outlive the call"
    )]
    pub fn save(_dir: &Path, user: &str, key: &str) -> Result<(), String> {
        let mut target = target(user);
        let mut name: Vec<u16> = user.encode_utf16().chain(std::iter::once(0)).collect();
        let mut blob = key.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            UserName: name.as_mut_ptr(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        // SAFETY: every pointer in `credential` points into a buffer alive for the call.
        if unsafe { CredWriteW(&credential, 0) } != 0 {
            Ok(())
        } else {
            Err("the Credential Manager refused the Last.fm session".into())
        }
    }

    #[allow(
        unsafe_code,
        reason = "calls the Credential Manager and frees what it returns"
    )]
    pub fn load(_dir: &Path, user: &str) -> Result<Option<String>, String> {
        let target = target(user);
        let mut credential: *mut CREDENTIALW = ptr::null_mut();
        // SAFETY: a NUL-terminated target name, and a pointer for the result.
        if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) } == 0 {
            // SAFETY: reads this thread's last error, set by CredReadW.
            let error = unsafe { GetLastError() };
            if error == ERROR_NOT_FOUND {
                return Ok(None);
            }
            return Err(format!(
                "the Credential Manager could not be read ({error})"
            ));
        }
        // SAFETY: on success, `credential` points to a credential with a blob of the given
        // size, until it is freed below.
        unsafe {
            let c = &*credential;
            let blob = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
            let key = String::from_utf8(blob.to_vec()).ok();
            CredFree(credential.cast());
            Ok(key)
        }
    }

    #[allow(
        unsafe_code,
        reason = "calls the Credential Manager with a NUL-terminated name"
    )]
    pub fn delete(_dir: &Path, user: &str) {
        let target = target(user);
        // SAFETY: a NUL-terminated target name.
        unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
    }
}

#[cfg(not(windows))]
mod platform {
    use std::io::Write;
    use std::path::{Path, PathBuf};

    fn path(dir: &Path, user: &str) -> PathBuf {
        dir.join(super::file_name(user))
    }

    pub fn save(dir: &Path, user: &str, key: &str) -> Result<(), String> {
        save_at(&path(dir, user), key)
    }

    pub fn save_at(path: &Path, key: &str) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Readable and writable by the user only.
            options.mode(0o600);
        }
        let mut file = options.open(path).map_err(|e| e.to_string())?;
        // The mode above is only for a new file: one there already keeps its own otherwise.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        file.write_all(key.as_bytes()).map_err(|e| e.to_string())
    }

    pub fn load(dir: &Path, user: &str) -> Result<Option<String>, String> {
        load_at(&path(dir, user))
    }

    pub fn load_at(path: &Path) -> Result<Option<String>, String> {
        match std::fs::read_to_string(path) {
            Ok(key) => Ok(Some(key.trim().to_owned()).filter(|k| !k.is_empty())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("the Last.fm session could not be read: {e}")),
        }
    }

    pub fn delete(dir: &Path, user: &str) {
        let _ = std::fs::remove_file(path(dir, user));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::platform::{load_at, save_at};

    #[test]
    fn the_session_file_is_the_users_alone() {
        let dir = std::env::temp_dir().join(format!("freya-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("Freya Last.fm someone.key");
        assert_eq!(load_at(&path), Ok(None));
        // A file there already, readable by others, is made the user's alone.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        save_at(&path, "session-key").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(load_at(&path), Ok(Some("session-key".to_owned())));
        let _ = std::fs::remove_dir_all(dir);
    }
}
