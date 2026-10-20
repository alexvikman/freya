//! The server's folders, for a client on another computer to choose the music's folders from.

use std::path::{Path, PathBuf};

use freya_protocol::FolderListing;

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
