//! The server's folders, for a client on another computer to choose the music's folders from;
//! and whether a file is the music's at all.

use std::path::{Path, PathBuf};

use freya_protocol::{FolderListing, FolderPlace};

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
    // A regular file before it is opened, and opened without waiting: a FIFO put in its place
    // would otherwise hold the opening for ever.
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
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

/// The folders in `folder`, by name, hidden ones left out, and the places to start from; the
/// user's home for none. On Windows, `\\computer` lists that computer's shared folders.
pub fn listing(folder: Option<&Path>) -> FolderListing {
    let places = places();
    let Some(folder) = folder
        .map(Path::to_owned)
        .or_else(|| directories::UserDirs::new().map(|dirs| dirs.home_dir().to_owned()))
    else {
        return FolderListing {
            places,
            ..FolderListing::default()
        };
    };
    #[cfg(windows)]
    if let Some(computer) = windows::computer_only(&folder) {
        return match windows::shares(computer) {
            Ok(folders) => FolderListing {
                folder: Some(folder.clone()),
                parent: None,
                folders,
                places,
                error: None,
                backups: Vec::new(),
            },
            Err(e) => FolderListing {
                folder: Some(folder.clone()),
                places,
                error: Some(e),
                ..FolderListing::default()
            },
        };
    }
    let read = std::fs::read_dir(&folder);
    let error = read.as_ref().err().map(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "There is no such folder.".to_owned(),
        std::io::ErrorKind::PermissionDenied => "Freya Server may not look in it.".to_owned(),
        _ => format!("It cannot be listed: {e}"),
    });
    let entries: Vec<PathBuf> = read
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| !hidden(entry))
        .map(|entry| entry.path())
        .collect();
    let mut folders: Vec<PathBuf> = entries.iter().filter(|p| p.is_dir()).cloned().collect();
    folders.sort_by_key(|path| path.to_string_lossy().to_lowercase());
    let mut backups: Vec<PathBuf> = entries
        .into_iter()
        .filter(|path| {
            path.is_file()
                && (crate::backup::is_backup_name(path) || crate::transfer::is_export_name(path))
        })
        .collect();
    backups.sort_by_key(|path| path.to_string_lossy().to_lowercase());
    FolderListing {
        parent: folder.parent().map(Path::to_owned),
        folder: Some(folder),
        folders,
        places,
        error,
        backups,
    }
}

/// Whether `entry` is hidden: by a dot before its name, and on Windows by its attribute too, as
/// the links an account's folder keeps for old programs are ("Cookies", "Application Data").
fn hidden(entry: &std::fs::DirEntry) -> bool {
    if entry.file_name().to_string_lossy().starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_HIDDEN;
        if entry
            .metadata()
            .is_ok_and(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
        {
            return true;
        }
    }
    false
}

fn place(path: PathBuf, name: impl Into<String>, kind: &str) -> FolderPlace {
    FolderPlace {
        path,
        name: name.into(),
        kind: kind.to_owned(),
    }
}

/// Where to start looking: the user's home, and the drives and volumes the computer has,
/// network ones among them.
fn places() -> Vec<FolderPlace> {
    let mut places: Vec<FolderPlace> = directories::UserDirs::new()
        .map(|dirs| {
            let home = dirs.home_dir().to_owned();
            let name = home
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Home".to_owned());
            place(home, name, "home")
        })
        .into_iter()
        .collect();
    #[cfg(windows)]
    places.extend(windows::drives());
    #[cfg(not(windows))]
    {
        let root_name = if cfg!(target_os = "macos") {
            "Computer"
        } else {
            "File System"
        };
        places.push(place(PathBuf::from("/"), root_name, "drive"));
        // Where macOS and Linux mount drives, network ones among them.
        let user = std::env::var("USER").unwrap_or_default();
        for mounts in ["/Volumes", "/mnt", "/media", &format!("/media/{user}")] {
            let Ok(entries) = std::fs::read_dir(mounts) else {
                continue;
            };
            let mut found: Vec<FolderPlace> = entries
                .flatten()
                .filter(|entry| !hidden(entry) && entry.path().is_dir())
                // The system's own volume is the computer's root, already there.
                .filter(|entry| {
                    std::fs::canonicalize(entry.path()).ok().as_deref() != Some(Path::new("/"))
                })
                .filter(|entry| mounts != "/media" || entry.file_name().to_string_lossy() != user)
                .map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    place(entry.path(), name, "volume")
                })
                .collect();
            found.sort_by_key(|p| p.name.to_lowercase());
            places.extend(found);
        }
    }
    places
}

/// The drives, a computer's shares, and network drives, as Windows has them.
#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "asks Windows for its drives and a computer's shares"
)]
mod windows {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};

    use freya_protocol::FolderPlace;
    use windows_sys::Win32::NetworkManagement::NetManagement::NetApiBufferFree;
    use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, NetShareEnum, SHARE_INFO_1,
        STYPE_DISKTREE, STYPE_MASK, STYPE_SPECIAL,
    };
    use windows_sys::Win32::System::WindowsProgramming::{DRIVE_CDROM, DRIVE_REMOTE};

    /// The longest a share or volume name, or a network drive's address, is taken to be.
    const MOST_CHARS: usize = 1024;

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain([0])
            .collect()
    }

    fn from_wide(chars: &[u16]) -> String {
        let length = chars.iter().position(|c| *c == 0).unwrap_or(chars.len());
        std::ffi::OsString::from_wide(&chars[..length])
            .to_string_lossy()
            .into_owned()
    }

    /// The computer `path` names alone, as `\\nas`, without a share.
    pub fn computer_only(path: &Path) -> Option<&str> {
        let text = path.to_str()?.strip_prefix(r"\\")?;
        let computer = text.trim_end_matches('\\');
        (!computer.is_empty() && !computer.contains(['\\', '/', '?'])).then_some(computer)
    }

    /// The folders `computer` shares, as paths, by name; not the shares Windows makes for
    /// itself (`C$`, `ADMIN$`).
    pub fn shares(computer: &str) -> Result<Vec<PathBuf>, String> {
        let server = wide(&format!(r"\\{computer}"));
        let mut buffer: *mut u8 = std::ptr::null_mut();
        let (mut read, mut total) = (0u32, 0u32);
        // SAFETY: Windows writes a buffer of `read` SHARE_INFO_1 entries, freed below.
        let status = unsafe {
            NetShareEnum(
                server.as_ptr(),
                1,
                &mut buffer,
                u32::MAX,
                &mut read,
                &mut total,
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            if !buffer.is_null() {
                // SAFETY: the buffer Windows gave.
                unsafe { NetApiBufferFree(buffer.cast()) };
            }
            return Err(format!(
                "{computer} did not list its shared folders (error {status}). Type a share's path, like \\\\{computer}\\music."
            ));
        }
        let mut shares = Vec::new();
        if !buffer.is_null() {
            // SAFETY: `read` entries, as NetShareEnum says.
            let infos =
                unsafe { std::slice::from_raw_parts(buffer.cast::<SHARE_INFO_1>(), read as usize) };
            for info in infos {
                if info.shi1_type & STYPE_MASK != STYPE_DISKTREE
                    || info.shi1_type & STYPE_SPECIAL != 0
                {
                    continue;
                }
                // SAFETY: a NUL-terminated name Windows wrote.
                let name = unsafe { wide_string(info.shi1_netname) };
                if !name.is_empty() && !name.ends_with('$') {
                    shares.push(PathBuf::from(format!(r"\\{computer}\{name}")));
                }
            }
            // SAFETY: the buffer Windows gave, no longer read.
            unsafe { NetApiBufferFree(buffer.cast()) };
        }
        shares.sort_by_key(|path| path.to_string_lossy().to_lowercase());
        Ok(shares)
    }

    /// The NUL-terminated string at `text`, at most [`MOST_CHARS`] long.
    ///
    /// SAFETY: `text` is null, or a NUL-terminated wide string.
    unsafe fn wide_string(text: *const u16) -> String {
        if text.is_null() {
            return String::new();
        }
        let mut length = 0;
        // SAFETY: read up to its NUL, as the caller promises there is one.
        while length < MOST_CHARS && unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: `length` characters before the NUL.
        from_wide(unsafe { std::slice::from_raw_parts(text, length) })
    }

    /// The drives, named as Explorer names them; network drives with their share, and none
    /// without a disc in it.
    pub fn drives() -> Vec<FolderPlace> {
        // SAFETY: a plain query.
        let letters = unsafe { GetLogicalDrives() };
        let mut drives = Vec::new();
        for (i, letter) in ('A'..='Z').enumerate() {
            if letters & (1 << i) == 0 {
                continue;
            }
            let root = format!(r"{letter}:\");
            let wide_root = wide(&root);
            // SAFETY: a NUL-terminated path.
            let kind = unsafe { GetDriveTypeW(wide_root.as_ptr()) };
            if kind == DRIVE_REMOTE {
                let device = wide(&format!("{letter}:"));
                let mut remote = vec![0u16; MOST_CHARS];
                let mut length = remote.len() as u32;
                // SAFETY: a buffer of the length given.
                let status = unsafe {
                    WNetGetConnectionW(device.as_ptr(), remote.as_mut_ptr(), &mut length)
                };
                let name = if status == 0 {
                    format!("{letter}: {}", from_wide(&remote))
                } else {
                    format!("Network Drive ({letter}:)")
                };
                drives.push(super::place(PathBuf::from(&root), name, "network"));
                continue;
            }
            let mut label = vec![0u16; 261];
            // SAFETY: a buffer of the length given; the rest are not asked for.
            let known = unsafe {
                GetVolumeInformationW(
                    wide_root.as_ptr(),
                    label.as_mut_ptr(),
                    label.len() as u32,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                )
            };
            if known == 0 {
                // A drive without a disc in it.
                continue;
            }
            let label = from_wide(&label);
            let name = match (label.is_empty(), kind == DRIVE_CDROM) {
                (false, _) => format!("{label} ({letter}:)"),
                (true, true) => format!("CD Drive ({letter}:)"),
                (true, false) => format!("Local Disk ({letter}:)"),
            };
            drives.push(super::place(PathBuf::from(&root), name, "drive"));
        }
        drives
    }
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
        #[cfg(windows)]
        {
            let hidden = dir.join("Hidden by attribute");
            std::fs::create_dir_all(&hidden).unwrap();
            let status = std::process::Command::new("attrib")
                .arg("+h")
                .arg(&hidden)
                .status()
                .unwrap();
            assert!(status.success());
        }
        let listing = listing(Some(&dir));
        assert_eq!(listing.folders, [dir.join("A"), dir.join("b")]);
        assert_eq!(listing.parent.as_deref(), dir.parent());
        let start = super::listing(None);
        assert_eq!(
            start.folder,
            directories::UserDirs::new().map(|dirs| dirs.home_dir().to_owned()),
            "the home folder first"
        );
        assert_eq!(start.places[0].kind, "home");
        assert!(start.places.iter().any(|p| p.kind == "drive"));
        let gone = super::listing(Some(&dir.join("gone")));
        assert!(gone.folders.is_empty());
        assert_eq!(gone.error.as_deref(), Some("There is no such folder."));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn only_regular_files_within_the_music_are_the_musics() {
        let dir = std::env::temp_dir().join(format!("freya-music-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let music = dir.join("music");
        std::fs::create_dir_all(music.join("album")).unwrap();
        let track = music.join("album/track.flac");
        std::fs::write(&track, b"music").unwrap();
        let secret = dir.join("accounts.json");
        std::fs::write(&secret, b"secret").unwrap();
        // A link from the music to a file outside it, and one within it.
        let out = music.join("album/out.flac");
        std::os::unix::fs::symlink(&secret, &out).unwrap();
        let within = music.join("within.flac");
        std::os::unix::fs::symlink(&track, &within).unwrap();
        let folders = [music.clone()];
        assert!(music_file(&track, &folders).is_some());
        assert!(music_file(&within, &folders).is_some());
        assert_eq!(music_file(&out, &folders), None);
        assert_eq!(music_file(&secret, &folders), None);
        assert_eq!(
            music_file(&music.join("album"), &folders),
            None,
            "not a file"
        );
        assert_eq!(
            music_file(&music.join("album/../../accounts.json"), &folders),
            None
        );
        // Opened, the file itself is asked where it is.
        assert!(open_music_file(&track, &folders).is_some());
        assert!(open_music_file(&within, &folders).is_some());
        assert!(
            open_music_file(&out, &folders).is_none(),
            "opened, it is outside"
        );
        assert!(open_music_file(&secret, &folders).is_none());
        // A FIFO in the music is not opened, and holds nothing up.
        let fifo = music.join("album/fifo.flac");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        assert!(open_music_file(&fifo, &folders).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
