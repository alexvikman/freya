//! The server's folders, for a client on another computer to choose the music's folders from;
//! and whether a file is the music's at all.

use std::path::{Path, PathBuf};

use freya_protocol::FolderListing;

/// The music file at `path` as it is after links: a regular file within one of `folders`, as
/// they are after links too. None for anything else, such as a link from a music folder to a
/// file outside them all, which someone who can write in a shared folder might make.
pub fn music_file(path: &Path, folders: &[PathBuf]) -> Option<PathBuf> {
    let real = std::fs::canonicalize(path).ok()?;
    if !std::fs::metadata(&real).ok()?.is_file() {
        return None;
    }
    folders
        .iter()
        .filter_map(|folder| std::fs::canonicalize(folder).ok())
        .any(|folder| real.starts_with(folder))
        .then_some(real)
}

/// The music file at `path`, opened: a regular file within one of `folders`, as the system says
/// of the file opened itself. So a link swapped in between a check and the opening is caught:
/// what is served is what was opened, wherever it is.
pub fn open_music_file(path: &Path, folders: &[PathBuf]) -> Option<std::fs::File> {
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let opened = path_of(&file)?;
    folders
        .iter()
        .filter_map(|folder| std::fs::canonicalize(folder).ok())
        .any(|folder| opened.starts_with(folder))
        .then_some(file)
}

/// Where the file opened as `file` is, as the system knows it.
#[cfg(target_os = "macos")]
#[allow(unsafe_code, reason = "asks the system the path of a file it opened")]
fn path_of(file: &std::fs::File) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;
    let mut path = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes at most PATH_MAX bytes, NUL-terminated, into the buffer given.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, path.as_mut_ptr()) } == -1 {
        return None;
    }
    let length = path.iter().position(|b| *b == 0)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&path[..length])))
}

#[cfg(target_os = "linux")]
fn path_of(file: &std::fs::File) -> Option<PathBuf> {
    use std::os::unix::io::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()
}

#[cfg(windows)]
#[allow(unsafe_code, reason = "asks the system the path of a file it opened")]
fn path_of(file: &std::fs::File) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };
    let mut path = vec![0u16; 32_768];
    // SAFETY: a handle of a file open now, and a buffer of the length given.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            path.as_mut_ptr(),
            path.len() as u32,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    } as usize;
    if length == 0 || length >= path.len() {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsString::from_wide(
        &path[..length],
    )))
}

/// Elsewhere the file opened cannot be told, and nothing is served.
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn path_of(_file: &std::fs::File) -> Option<PathBuf> {
    None
}

/// The folders in `folder`, by name, hidden ones left out; or, for none, where to start: the
/// user's home and the drives.
pub fn listing(folder: Option<&Path>) -> FolderListing {
    let Some(folder) = folder else {
        return FolderListing {
            folder: None,
            parent: None,
            folders: starts(),
        };
    };
    let mut folders: Vec<PathBuf> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    folders.sort_by_key(|path| path.to_string_lossy().to_lowercase());
    FolderListing {
        folder: Some(folder.to_owned()),
        parent: folder.parent().map(Path::to_owned),
        folders,
    }
}

/// Where to start looking: the user's home, and the drives mounted on the computer.
fn starts() -> Vec<PathBuf> {
    let mut starts: Vec<PathBuf> = directories::UserDirs::new()
        .map(|dirs| dirs.home_dir().to_owned())
        .into_iter()
        .collect();
    if cfg!(windows) {
        starts.extend(
            (b'A'..=b'Z')
                .map(|letter| PathBuf::from(format!("{}:\\", letter as char)))
                .filter(|drive| drive.is_dir()),
        );
    } else {
        // Where macOS and Linux mount drives, network ones among them.
        let user = std::env::var("USER").unwrap_or_default();
        for mounts in ["/Volumes", "/mnt", "/media", &format!("/media/{user}")] {
            starts.extend(listing(Some(Path::new(mounts))).folders);
        }
    }
    starts.dedup();
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_folders_in_a_folder() {
        let dir = std::env::temp_dir().join(format!("freya-folders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for name in ["b", "A", ".hidden"] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
        }
        std::fs::write(dir.join("file.flac"), b"").unwrap();
        let listing = listing(Some(&dir));
        assert_eq!(listing.folders, [dir.join("A"), dir.join("b")]);
        assert_eq!(listing.parent.as_deref(), dir.parent());
        assert!(super::listing(None).folder.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
