//! What Freya kept in its own folders before the server kept it, taken in by the server: each
//! part the first time the server has none of its own.

use std::path::Path;

use freya_protocol::ServerSettings;

use crate::settings;

/// Where Freya kept things, and where the server keeps them.
pub struct Folders<'a> {
    /// Freya's data folder: the library, pictures, playlists and Last.fm's waiting plays.
    pub freya_data: &'a Path,
    /// Freya's settings folder: its settings, and Last.fm's session elsewhere than on Windows.
    pub freya_settings: &'a Path,
    /// The server's folder.
    pub data: &'a Path,
    /// The server's settings.
    pub settings_file: &'a Path,
    /// Where the server's scrobbling keeps its session and waiting plays.
    pub scrobbles: &'a Path,
}

/// Moves the library, its pictures and the playlists into the server's folder, reads the
/// library's settings from Freya's, and takes the Last.fm account Freya scrobbled for.
pub fn take_in(folders: &Folders<'_>) {
    let Folders {
        freya_data,
        freya_settings,
        data,
        settings_file,
        scrobbles,
    } = *folders;
    // The database with its journal, together.
    let groups: [&[&str]; 3] = [
        &[
            "library.sqlite3",
            "library.sqlite3-wal",
            "library.sqlite3-shm",
        ],
        &["pictures"],
        &["playlists"],
    ];
    for group in groups {
        if !data.join(group[0]).exists() {
            for name in group {
                move_over(&freya_data.join(name), &data.join(name));
            }
        }
    }
    let Ok(text) = std::fs::read_to_string(freya_settings.join("settings.toml")) else {
        return;
    };
    if !settings_file.exists() {
        // The library's settings have the same names there; the rest is the client's.
        match toml::from_str::<ServerSettings>(&text) {
            Ok(taken) => match settings::save(settings_file, &taken) {
                Ok(()) => log::info!("took the library's settings from Freya's"),
                Err(e) => log::warn!("could not keep the settings: {e}"),
            },
            Err(e) => log::warn!("could not read the library's settings from Freya's: {e}"),
        }
    }
    // The Last.fm account, with its session and waiting plays.
    #[derive(serde::Deserialize)]
    struct Scrobbling {
        lastfm_user: Option<String>,
    }
    let user_file = scrobbles.join(crate::scrobbler::USER_FILE);
    let user = toml::from_str::<Scrobbling>(&text)
        .ok()
        .and_then(|s| s.lastfm_user);
    if let Some(user) = user
        && !user_file.exists()
    {
        let _ = std::fs::create_dir_all(scrobbles);
        let key = crate::scrobbler::session_file(&user);
        move_over(&freya_settings.join(&key), &scrobbles.join(&key));
        move_over(
            &freya_data.join(crate::scrobbler::QUEUE_FILE),
            &scrobbles.join(crate::scrobbler::QUEUE_FILE),
        );
        match std::fs::write(&user_file, &user) {
            Ok(()) => log::info!("took the Last.fm account Freya scrobbled for"),
            Err(e) => log::warn!("could not keep who scrobbles: {e}"),
        }
    }
}

/// Moves `from` to `to`, when it is there.
fn move_over(from: &Path, to: &Path) {
    if !from.exists() {
        return;
    }
    match std::fs::rename(from, to) {
        Ok(()) => log::info!("moved {} to {}", from.display(), to.display()),
        Err(e) => log::warn!("could not move {}: {e}", from.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_in_what_freya_kept_once() {
        let dir = std::env::temp_dir().join(format!("freya-take-in-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (freya_data, freya_settings, data) =
            (dir.join("data"), dir.join("config"), dir.join("server"));
        for folder in [&freya_data, &freya_settings, &data] {
            std::fs::create_dir_all(folder).unwrap();
        }
        std::fs::write(freya_data.join("library.sqlite3"), "db").unwrap();
        std::fs::write(freya_data.join("library.sqlite3-wal"), "wal").unwrap();
        std::fs::create_dir_all(freya_data.join("playlists")).unwrap();
        std::fs::write(freya_data.join("lastfm-queue.json"), "{}").unwrap();
        std::fs::write(
            freya_settings.join("settings.toml"),
            "volume = 0.5\nlibrary_folders = [\"/music\"]\nscan_every_hours = 6\nlastfm_user = \"anna\"\n",
        )
        .unwrap();
        std::fs::write(freya_settings.join("Freya Last.fm anna.key"), "key").unwrap();
        // The server has its own pictures already: Freya's stay.
        std::fs::create_dir_all(freya_data.join("pictures")).unwrap();
        std::fs::create_dir_all(data.join("pictures")).unwrap();

        let folders = Folders {
            freya_data: &freya_data,
            freya_settings: &freya_settings,
            data: &data,
            settings_file: &data.join("settings.toml"),
            scrobbles: &data.join("lastfm"),
        };
        take_in(&folders);
        assert!(data.join("library.sqlite3").is_file());
        assert!(data.join("library.sqlite3-wal").is_file());
        assert!(data.join("playlists").is_dir());
        assert!(
            freya_data.join("pictures").is_dir(),
            "the server had its own"
        );
        let (settings, damaged) = settings::load(&data.join("settings.toml"));
        assert!(!damaged);
        assert_eq!(settings.library_folders, [Path::new("/music")]);
        assert_eq!(settings.scan_every_hours, 6);
        let lastfm = data.join("lastfm");
        assert_eq!(
            std::fs::read_to_string(lastfm.join("lastfm-user")).unwrap(),
            "anna"
        );
        assert!(lastfm.join("lastfm-queue.json").is_file());
        if cfg!(not(windows)) {
            assert!(lastfm.join("Freya Last.fm anna.key").is_file());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
