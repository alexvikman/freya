//! Moving a file from one place in the music's folders to another, so that nothing put in a
//! shared music folder leads it out of them, nor has it replace a file.
//!
//! On macOS and Linux each folder on the way is opened from the one before it, never through a
//! link, and the file is renamed from folder to folder by those handles: a folder swapped for
//! a link while it moves leads nowhere. Windows looks at each folder by its path just before
//! the move, which leaves a moment in which one swapped for a link would be followed.
#![allow(
    unsafe_code,
    reason = "opens folders and renames files relative to folder handles, which std cannot"
)]

use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path};

/// Moves the file at `from`, a path within the folder `from_root`, to `to`, a path within
/// `to_root`; the folders `to` is in are made as needed. A link is neither moved nor followed
/// below the roots, and a file already at `to` stops the move, unless it is the same file by
/// another case of its name.
pub fn move_within(from_root: &Path, from: &Path, to_root: &Path, to: &Path) -> io::Result<()> {
    platform::move_within(from_root, &names(from)?, to_root, &names(to)?)
}

/// The names of the folders and the file of a path within a folder: nothing but names.
fn names(within: &Path) -> io::Result<Vec<&OsStr>> {
    let names = within
        .components()
        .map(|part| match part {
            Component::Normal(name) => Ok(name),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "that is not a place within the library's folders",
            )),
        })
        .collect::<io::Result<Vec<_>>>()?;
    if names.is_empty() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(names)
}

fn not_a_file() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "it is not a file")
}

fn not_a_folder(name: &OsStr) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{} is not a folder", name.to_string_lossy()),
    )
}

#[cfg(unix)]
mod platform {
    use std::ffi::{CString, OsStr};
    use std::io;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;

    pub fn move_within(
        from_root: &Path,
        from: &[&OsStr],
        to_root: &Path,
        to: &[&OsStr],
    ) -> io::Result<()> {
        let (from_folder, from_name) = folder_of(from_root, from, false)?;
        let file = stat_at(&from_folder, &from_name)?;
        if file.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(super::not_a_file());
        }
        let (to_folder, to_name) = folder_of(to_root, to, true)?;
        match stat_at(&to_folder, &to_name) {
            // The same file, its name in another case.
            Ok(there) if there.st_dev == file.st_dev && there.st_ino == file.st_ino => {
                rename_at(&from_folder, &from_name, &to_folder, &to_name)
            }
            _ => rename_new_at(&from_folder, &from_name, &to_folder, &to_name),
        }
    }

    fn c_name(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
    }

    /// The folder the last of `names` is in, opened from `root` one folder at a time, each
    /// made first when `make`; and that last name.
    fn folder_of(root: &Path, names: &[&OsStr], make: bool) -> io::Result<(OwnedFd, CString)> {
        let (last, folders) = names.split_last().ok_or(io::ErrorKind::InvalidInput)?;
        let mut folder: OwnedFd = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(root)?
            .into();
        for name in folders {
            folder = open_folder(&folder, name, make)?;
        }
        Ok((folder, c_name(last)?))
    }

    /// The folder `name` in `folder`, made first when `make`: a folder, never a link to one.
    fn open_folder(folder: &OwnedFd, name: &OsStr, make: bool) -> io::Result<OwnedFd> {
        let c = c_name(name)?;
        if make {
            // SAFETY: an open folder's handle and a name ending in NUL, alive for the call.
            if unsafe { libc::mkdirat(folder.as_raw_fd(), c.as_ptr(), 0o777) } != 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::AlreadyExists {
                    return Err(e);
                }
            }
        }
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: as above; O_NOFOLLOW refuses a link where the folder should be.
        let opened = unsafe { libc::openat(folder.as_raw_fd(), c.as_ptr(), flags) };
        if opened < 0 {
            let e = io::Error::last_os_error();
            return Err(match e.raw_os_error() {
                Some(libc::ELOOP | libc::ENOTDIR) => super::not_a_folder(name),
                _ => e,
            });
        }
        // SAFETY: a handle just opened, owned by nothing else.
        Ok(unsafe { OwnedFd::from_raw_fd(opened) })
    }

    /// What is at `name` in `folder`: a link itself, not what it leads to.
    fn stat_at(folder: &OwnedFd, name: &CString) -> io::Result<libc::stat> {
        let mut found = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: an open folder's handle, a name ending in NUL and room for the answer.
        let done = unsafe {
            libc::fstatat(
                folder.as_raw_fd(),
                name.as_ptr(),
                found.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if done != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstatat filled it.
        Ok(unsafe { found.assume_init() })
    }

    fn rename_at(
        from_folder: &OwnedFd,
        from: &CString,
        to_folder: &OwnedFd,
        to: &CString,
    ) -> io::Result<()> {
        // SAFETY: open folders' handles and names ending in NUL, alive for the call.
        let done = unsafe {
            libc::renameat(
                from_folder.as_raw_fd(),
                from.as_ptr(),
                to_folder.as_raw_fd(),
                to.as_ptr(),
            )
        };
        match done {
            0 => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// Renames unless something is at `to`, in one step where the file system can. One that
    /// cannot, as some network shares, is looked at first, which leaves a moment in which a
    /// file made at `to` would be replaced; within the same folders still.
    fn rename_new_at(
        from_folder: &OwnedFd,
        from: &CString,
        to_folder: &OwnedFd,
        to: &CString,
    ) -> io::Result<()> {
        match rename_exclusive(from_folder, from, to_folder, to) {
            Err(e) if cannot_exclusive(&e) => {
                if stat_at(to_folder, to).is_ok() {
                    return Err(io::ErrorKind::AlreadyExists.into());
                }
                rename_at(from_folder, from, to_folder, to)
            }
            done => done,
        }
    }

    #[cfg(target_os = "macos")]
    fn rename_exclusive(
        from_folder: &OwnedFd,
        from: &CString,
        to_folder: &OwnedFd,
        to: &CString,
    ) -> io::Result<()> {
        // SAFETY: open folders' handles and names ending in NUL, alive for the call.
        let done = unsafe {
            libc::renameatx_np(
                from_folder.as_raw_fd(),
                from.as_ptr(),
                to_folder.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        match done {
            0 => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn rename_exclusive(
        from_folder: &OwnedFd,
        from: &CString,
        to_folder: &OwnedFd,
        to: &CString,
    ) -> io::Result<()> {
        // SAFETY: open folders' handles and names ending in NUL, alive for the call.
        let done = unsafe {
            libc::renameat2(
                from_folder.as_raw_fd(),
                from.as_ptr(),
                to_folder.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        match done {
            0 => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
    fn rename_exclusive(_: &OwnedFd, _: &CString, _: &OwnedFd, _: &CString) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Whether renaming without replacing failed only as the file system does not do it.
    fn cannot_exclusive(e: &io::Error) -> bool {
        match e.raw_os_error() {
            Some(code) => {
                [libc::EINVAL, libc::ENOTSUP, libc::EOPNOTSUPP, libc::ENOSYS].contains(&code)
            }
            None => e.kind() == io::ErrorKind::Unsupported,
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    pub fn move_within(
        from_root: &Path,
        from: &[&OsStr],
        to_root: &Path,
        to: &[&OsStr],
    ) -> io::Result<()> {
        let from: PathBuf = std::iter::once(from_root.as_os_str())
            .chain(from.iter().copied())
            .collect();
        // The file itself, not a link to one; a junction is a link too.
        if !std::fs::symlink_metadata(&from).is_ok_and(|kind| kind.is_file()) {
            return Err(super::not_a_file());
        }
        let (last, folders) = to.split_last().ok_or(io::ErrorKind::InvalidInput)?;
        let mut folder = to_root.to_path_buf();
        for name in folders {
            folder.push(name);
            make_folder(&folder, name)?;
        }
        let to = folder.join(last);
        let same = match (std::fs::canonicalize(&from), std::fs::canonicalize(&to)) {
            (Ok(from), Ok(to)) => from == to,
            _ => false,
        };
        if same && std::fs::symlink_metadata(&to).is_ok_and(|kind| kind.is_file()) {
            // The same file, its name in another case.
            return std::fs::rename(&from, &to);
        }
        rename_new(&from, &to)
    }

    fn make_folder(folder: &Path, name: &OsStr) -> io::Result<()> {
        match std::fs::create_dir(folder) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
            _ => match std::fs::symlink_metadata(folder) {
                Ok(kind) if kind.is_dir() => Ok(()),
                _ => Err(super::not_a_folder(name)),
            },
        }
    }

    /// Renames unless something is at `to`: without MOVEFILE_REPLACE_EXISTING, a file there
    /// stops it.
    fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
        let wide = |path: &Path| -> Vec<u16> {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        };
        let (from, to) = (wide(from), wide(to));
        // SAFETY: both are paths ending in NUL, alive for the call.
        let done = unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
        };
        match done {
            0 => Err(io::Error::last_os_error()),
            _ => Ok(()),
        }
    }
}
