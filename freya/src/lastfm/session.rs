//! Where the Last.fm session key is kept between runs: the login keychain on macOS, the
//! Credential Manager on Windows, and elsewhere a file in Freya's settings folder that only
//! the user can read. The key lets Freya scrobble for the user until they disconnect, so it
//! never goes in `settings.toml`.

/// The name the key is kept under, with the user's Last.fm name as the account.
const SERVICE: &str = "Freya Last.fm";

/// Keeps `key` as `user`'s session key.
pub fn save(user: &str, key: &str) -> Result<(), String> {
    platform::save(user, key)
}

/// `user`'s session key, if one is kept.
pub fn load(user: &str) -> Option<String> {
    platform::load(user)
}

/// Forgets `user`'s session key.
pub fn delete(user: &str) {
    platform::delete(user);
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ptr::{self, NonNull};

    use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFRetained, CFString, CFType};
    use objc2_security::{
        SecItemAdd, SecItemCopyMatching, SecItemDelete, kSecAttrAccount, kSecAttrService,
        kSecClass, kSecClassGenericPassword, kSecMatchLimit, kSecMatchLimitOne, kSecReturnData,
        kSecValueData,
    };

    use super::SERVICE;

    /// `errSecSuccess`.
    const SUCCESS: i32 = 0;

    /// A query for the keychain item of `user`, with `extra` keys and values.
    #[allow(unsafe_code, reason = "reads constants the Security framework exports")]
    fn query(
        user: &str,
        extra: &[(&CFString, &CFType)],
    ) -> CFRetained<CFDictionary<CFString, CFType>> {
        let service = CFString::from_str(SERVICE);
        let account = CFString::from_str(user);
        // SAFETY: the keys are immutable strings the Security framework always provides.
        let (class, generic, service_key, account_key) = unsafe {
            (
                kSecClass,
                kSecClassGenericPassword,
                kSecAttrService,
                kSecAttrAccount,
            )
        };
        let mut keys: Vec<&CFString> = vec![class, service_key, account_key];
        let mut values: Vec<&CFType> = vec![generic.as_ref(), service.as_ref(), account.as_ref()];
        for (key, value) in extra {
            keys.push(key);
            values.push(value);
        }
        CFDictionary::from_slices(&keys, &values)
    }

    #[allow(
        unsafe_code,
        reason = "calls the Security framework with dictionaries it accepts"
    )]
    pub fn save(user: &str, key: &str) -> Result<(), String> {
        delete(user);
        let data = CFData::from_bytes(key.as_bytes());
        // SAFETY: a constant the Security framework always provides.
        let value_key = unsafe { kSecValueData };
        let item = query(user, &[(value_key, data.as_ref())]);
        // SAFETY: the dictionary holds a generic password's attributes; no result is asked for.
        let status = unsafe { SecItemAdd(item.as_ref(), ptr::null_mut()) };
        if status == SUCCESS {
            Ok(())
        } else {
            Err(format!(
                "the keychain refused the Last.fm session ({status})"
            ))
        }
    }

    #[allow(
        unsafe_code,
        reason = "calls the Security framework, and takes ownership of its result"
    )]
    pub fn load(user: &str) -> Option<String> {
        // SAFETY: constants the Security framework always provides.
        let (return_data, limit, one) =
            unsafe { (kSecReturnData, kSecMatchLimit, kSecMatchLimitOne) };
        let yes = CFBoolean::new(true);
        let search = query(user, &[(return_data, yes.as_ref()), (limit, one.as_ref())]);
        let mut result: *const CFType = ptr::null();
        // SAFETY: a valid query, and a pointer for the one item it returns.
        let status = unsafe { SecItemCopyMatching(search.as_ref(), &mut result) };
        if status != SUCCESS {
            return None;
        }
        // SAFETY: with kSecReturnData and one match, the result is a CFData the caller owns.
        let data: CFRetained<CFData> =
            unsafe { CFRetained::from_raw(NonNull::new(result.cast_mut())?.cast()) };
        String::from_utf8(data.to_vec()).ok()
    }

    #[allow(
        unsafe_code,
        reason = "calls the Security framework with a query it accepts"
    )]
    pub fn delete(user: &str) {
        let item = query(user, &[]);
        // SAFETY: a valid query for a generic password.
        unsafe { SecItemDelete(item.as_ref()) };
    }
}

#[cfg(windows)]
mod platform {
    use std::ptr;

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
    pub fn save(user: &str, key: &str) -> Result<(), String> {
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
    pub fn load(user: &str) -> Option<String> {
        let target = target(user);
        let mut credential: *mut CREDENTIALW = ptr::null_mut();
        // SAFETY: a NUL-terminated target name, and a pointer for the result.
        if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) } == 0 {
            return None;
        }
        // SAFETY: on success, `credential` points to a credential with a blob of the given
        // size, until it is freed below.
        unsafe {
            let c = &*credential;
            let blob = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
            let key = String::from_utf8(blob.to_vec()).ok();
            CredFree(credential.cast());
            key
        }
    }

    #[allow(
        unsafe_code,
        reason = "calls the Credential Manager with a NUL-terminated name"
    )]
    pub fn delete(user: &str) {
        let target = target(user);
        // SAFETY: a NUL-terminated target name.
        unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use std::io::Write;
    use std::path::PathBuf;

    use super::SERVICE;

    fn path(user: &str) -> Option<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "Freya")?;
        let name: String = user
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        Some(dirs.config_dir().join(format!("{SERVICE} {name}.key")))
    }

    pub fn save(user: &str, key: &str) -> Result<(), String> {
        let path = path(user).ok_or("no settings folder")?;
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
        let mut file = options.open(&path).map_err(|e| e.to_string())?;
        file.write_all(key.as_bytes()).map_err(|e| e.to_string())
    }

    pub fn load(user: &str) -> Option<String> {
        let key = std::fs::read_to_string(path(user)?).ok()?;
        Some(key.trim().to_owned()).filter(|k| !k.is_empty())
    }

    pub fn delete(user: &str) {
        if let Some(path) = path(user) {
            let _ = std::fs::remove_file(path);
        }
    }
}
