//! Music from a server on another computer, kept on this one so it plays again at once and
//! through a slow network (see `freya_audio::set_cache`): where, and how much. By default a
//! tenth of the free space on the disk, the music kept counted as free, at least 50 MB and at
//! most 10 GB; or as many gigabytes as the user chose.

use std::path::{Path, PathBuf};

const GB: u64 = 1_000_000_000;
const LEAST: u64 = 50_000_000;
const MOST: u64 = 10 * GB;

/// Where the music is kept.
fn dir() -> Option<PathBuf> {
    crate::settings::cache_dir().map(|dir| dir.join("music"))
}

/// Keeps music as `chosen` says: a number of gigabytes, or `None` for the default.
pub fn apply(chosen: Option<u32>) {
    let dir = dir();
    let limit = match chosen {
        Some(gb) => u64::from(gb) * GB,
        None => {
            // What is kept counts as free: set without a limit first, to count it.
            freya_audio::set_cache(dir.clone(), u64::MAX);
            let kept = freya_audio::cache_used();
            let free = dir.as_deref().and_then(free_space).unwrap_or(0);
            ((free + kept) / 10).clamp(LEAST, MOST)
        }
    };
    log::debug!("keeping up to {} of music from servers", size(limit));
    freya_audio::set_cache(dir, limit);
}

/// How much is kept, and the most that may be, for Settings.
pub fn describe(chosen: Option<u32>) -> String {
    let used = freya_audio::cache_used();
    match chosen {
        Some(gb) => format!("{} of at most {gb} GB", size(used)),
        None => format!("{} kept", size(used)),
    }
}

/// Forgets the music kept.
pub fn clear() {
    freya_audio::clear_cache();
}

/// A size as people read it: "820 MB", "1.2 GB".
fn size(bytes: u64) -> String {
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else {
        format!("{} MB", bytes / 1_000_000)
    }
}

/// The space free on the disk `dir` is on, for this user.
#[cfg(unix)]
#[allow(unsafe_code, reason = "asks the system the free space")]
fn free_space(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    std::fs::create_dir_all(dir).ok()?;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid C string, and room for the answer.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)]
    Some(stats.f_bavail as u64 * stats.f_frsize as u64)
}

#[cfg(windows)]
#[allow(unsafe_code, reason = "asks the system the free space")]
fn free_space(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    std::fs::create_dir_all(dir).ok()?;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: a valid wide string, and room for the one answer asked for.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(not(any(unix, windows)))]
fn free_space(_: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_people_say_them() {
        assert_eq!(size(820_400_000), "820 MB");
        assert_eq!(size(1_249_000_000), "1.2 GB");
        assert_eq!(size(0), "0 MB");
    }

    #[test]
    fn the_free_space_is_known() {
        let dir = std::env::temp_dir().join(format!("freya-free-{}", std::process::id()));
        assert!(free_space(&dir).is_some_and(|free| free > 0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
