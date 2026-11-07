//! Where Freya and its server keep their files on this computer, as the system has it: the
//! user's home, and a program's settings, data and cache folders. The folders are the ones
//! Freya has always used, so what a user kept is found where it is:
//!
//! | | Linux | macOS | Windows |
//! |-|-------|-------|---------|
//! | settings | `$XDG_CONFIG_HOME/freya` or `~/.config/freya` | `~/Library/Application Support/Freya` | `%APPDATA%\Freya\config` |
//! | data | `$XDG_DATA_HOME/freya` or `~/.local/share/freya` | `~/Library/Application Support/Freya` | `%APPDATA%\Freya\data` |
//! | cache | `$XDG_CACHE_HOME/freya` or `~/.cache/freya` | `~/Library/Caches/Freya` | `%LOCALAPPDATA%\Freya\cache` |
//!
//! On Linux a program's folder is its name in lower case without spaces (`freyaserver`), on
//! macOS with spaces made dashes (`Freya Server` is `Freya-Server`), and on Windows as it is.
//! The AppData folders are asked of Windows rather than read from the environment, which a
//! program may have changed for its children.

use std::path::PathBuf;

/// A program's folders for this user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    /// Settings, which the user may want to keep across computers.
    pub config: PathBuf,
    /// What the program keeps: libraries, installed skins, keys.
    pub data: PathBuf,
    /// What is this computer's alone, such as how a running program is reached: on Windows
    /// the local AppData folder, which does not follow the user to other computers; the data
    /// folder elsewhere.
    pub data_local: PathBuf,
    /// Files that can always be made again.
    pub cache: PathBuf,
}

/// The folders of the program called `app`, such as "Freya" or "Freya Server". None when
/// the system does not say where this user's folders are.
pub fn of(app: &str) -> Option<Dirs> {
    platform::of(app)
}

/// The user's home folder.
pub fn home() -> Option<PathBuf> {
    platform::home()
}

/// Where this user's programs keep their settings: `$XDG_CONFIG_HOME` or `~/.config`.
#[cfg(target_os = "linux")]
pub fn config_home() -> Option<PathBuf> {
    Some(platform::xdg("XDG_CONFIG_HOME").unwrap_or(home()?.join(".config")))
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::PathBuf;

    use super::Dirs;

    pub fn of(app: &str) -> Option<Dirs> {
        let home = home()?;
        // "Freya Server" is "freyaserver".
        let app = app.split_whitespace().collect::<String>().to_lowercase();
        let data = xdg("XDG_DATA_HOME")
            .unwrap_or_else(|| home.join(".local/share"))
            .join(&app);
        Some(Dirs {
            config: xdg("XDG_CONFIG_HOME")
                .unwrap_or_else(|| home.join(".config"))
                .join(&app),
            data_local: data.clone(),
            data,
            cache: xdg("XDG_CACHE_HOME")
                .unwrap_or_else(|| home.join(".cache"))
                .join(&app),
        })
    }

    pub fn home() -> Option<PathBuf> {
        super::unix::home()
    }

    /// An XDG base folder from the environment, if set to an absolute path, as the
    /// specification asks.
    pub fn xdg(variable: &str) -> Option<PathBuf> {
        let path = PathBuf::from(std::env::var_os(variable)?);
        path.is_absolute().then_some(path)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::PathBuf;

    use super::Dirs;

    pub fn of(app: &str) -> Option<Dirs> {
        let home = home()?;
        // "Freya Server" is "Freya-Server".
        let app = app.replace(' ', "-");
        let support = home.join("Library/Application Support").join(&app);
        Some(Dirs {
            config: support.clone(),
            data_local: support.clone(),
            data: support,
            cache: home.join("Library/Caches").join(&app),
        })
    }

    pub fn home() -> Option<PathBuf> {
        super::unix::home()
    }
}

#[cfg(unix)]
mod unix {
    use std::ffi::{CStr, OsString};
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    /// `$HOME`, or the home folder the user's account names when it is not set, as when a
    /// service starts the program.
    pub fn home() -> Option<PathBuf> {
        std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .or_else(account_home)
            .map(PathBuf::from)
    }

    /// The home folder the user's account names.
    #[allow(unsafe_code, reason = "asks the system the user's account")]
    pub fn account_home() -> Option<OsString> {
        let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // The buffer the account's strings are copied into: the size the system suggests, or
        // a generous one when it does not say; doubled while the system asks for more.
        let suggested = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
        let mut buffer = vec![0u8; usize::try_from(suggested).unwrap_or(0).max(2048)];
        loop {
            // SAFETY: `passwd`, `buffer` and `result` are valid for the call, and `buffer`'s
            // length is the one given.
            let status = unsafe {
                libc::getpwuid_r(
                    libc::getuid(),
                    &mut passwd,
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut result,
                )
            };
            if status == libc::ERANGE && buffer.len() < 1 << 20 {
                buffer.resize(buffer.len() * 2, 0);
                continue;
            }
            if status != 0 || result.is_null() || passwd.pw_dir.is_null() {
                return None;
            }
            // SAFETY: the system made `pw_dir` point into `buffer`, which lives on, at a string
            // ending in a nul.
            let dir = unsafe { CStr::from_ptr(passwd.pw_dir) }.to_bytes();
            return (!dir.is_empty()).then(|| OsString::from_vec(dir.to_vec()));
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_LocalAppData, FOLDERID_Profile, FOLDERID_RoamingAppData, KF_FLAG_DEFAULT,
        SHGetKnownFolderPath,
    };

    use super::Dirs;

    pub fn of(app: &str) -> Option<Dirs> {
        let roaming = known_folder(&FOLDERID_RoamingAppData)?.join(app);
        let local = known_folder(&FOLDERID_LocalAppData)?.join(app);
        Some(Dirs {
            config: roaming.join("config"),
            data: roaming.join("data"),
            data_local: local.join("data"),
            cache: local.join("cache"),
        })
    }

    pub fn home() -> Option<PathBuf> {
        known_folder(&FOLDERID_Profile)
    }

    #[allow(
        unsafe_code,
        reason = "asks Windows where one of the user's folders is"
    )]
    fn known_folder(id: &windows_sys::core::GUID) -> Option<PathBuf> {
        let mut path: *mut u16 = std::ptr::null_mut();
        // SAFETY: `id` and `path` are valid for the call. The string Windows allocates ends in
        // a nul, is read up to it, and is freed as its documentation asks, set or not.
        let status = unsafe {
            SHGetKnownFolderPath(id, KF_FLAG_DEFAULT as u32, std::ptr::null_mut(), &mut path)
        };
        let folder = if status == 0 && !path.is_null() {
            let mut length = 0;
            while unsafe { *path.add(length) } != 0 {
                length += 1;
            }
            let wide = unsafe { std::slice::from_raw_parts(path, length) };
            Some(PathBuf::from(OsString::from_wide(wide)))
        } else {
            None
        };
        unsafe { CoTaskMemFree(path.cast()) };
        folder.filter(|folder| !folder.as_os_str().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folders_are_under_the_home() {
        let home = home().expect("a home folder");
        assert!(home.is_absolute());
        let dirs = of("Freya").expect("Freya's folders");
        for dir in [&dirs.config, &dirs.data, &dirs.data_local, &dirs.cache] {
            assert!(dir.is_absolute(), "{}", dir.display());
        }
        if cfg!(target_os = "macos") {
            assert_eq!(dirs.data, home.join("Library/Application Support/Freya"));
            assert_eq!(dirs.cache, home.join("Library/Caches/Freya"));
            assert_eq!(
                of("Freya Server").unwrap().data,
                home.join("Library/Application Support/Freya-Server")
            );
        }
        if cfg!(target_os = "linux") {
            assert!(dirs.config.ends_with("freya"), "{}", dirs.config.display());
            assert!(of("Freya Server").unwrap().data.ends_with("freyaserver"));
        }
        if cfg!(windows) {
            assert!(dirs.config.ends_with("Freya\\config"));
            assert!(dirs.cache.ends_with("Freya\\cache"));
            assert!(
                of("Freya Server")
                    .unwrap()
                    .data
                    .ends_with("Freya Server\\data")
            );
        }
    }

    /// The account names the home folder too, for when HOME is not set.
    #[cfg(unix)]
    #[test]
    fn the_account_names_the_home() {
        let from_account = PathBuf::from(unix::account_home().expect("the account's home"));
        assert!(from_account.is_absolute(), "{}", from_account.display());
        // Normally the same as HOME; a test runner may set HOME elsewhere.
        if std::env::var_os("HOME").is_some_and(|h| h == from_account.as_os_str()) {
            assert_eq!(home().unwrap(), from_account);
        }
    }
}
