//! Settings and the playlist, kept between runs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::gui::equalizer::EqualizerSettings;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: bool,
    /// Window size in logical pixels.
    pub window_size: Option<(f32, f32)>,
    /// Where the window was last, its outer top left in pixels, and whether it was maximized:
    /// it opens there again.
    pub window_position: Option<(i32, i32)>,
    pub window_maximized: bool,
    /// Skin file, folder or package; `None` is the built-in skin.
    pub skin: Option<PathBuf>,
    /// "system", "light" or "dark".
    pub theme: String,
    pub equalizer_visible: bool,
    /// "spectrum", "oscilloscope" or "off".
    pub visualizer_mode: String,
    /// Visualization frames per second. Every frame redraws the whole window, so this is the
    /// main cost of visualization: about 6 % CPU at 30 and 11 % at 60 on an M-series MacBook.
    pub visualizer_fps: u32,
    /// Folders the library is made from.
    pub library_folders: Vec<PathBuf>,
    pub equalizer: EqualizerSettings,
    /// Whether Freya looks for updates on its own, at launch and once a day.
    pub check_for_updates: bool,
    /// When Freya last looked, in seconds since 1970.
    pub last_update_check: Option<u64>,
    /// A version the user does not want offered again.
    pub skipped_update: Option<String>,
    /// Set when settings.toml could not be read as a whole: the fields that could be are kept,
    /// and the library is not brought in line with a folder list that may be missing.
    #[serde(skip)]
    pub damaged: bool,
    /// The Last.fm user Freya scrobbles for; their session key is kept apart (see
    /// lastfm/session.rs).
    pub lastfm_user: Option<String>,
    /// Whether plays are scrobbled while an account is connected.
    pub lastfm_scrobbling: bool,
    /// How an artist's albums are ordered: "release-date" or "name".
    pub album_sort: String,
    /// How an artist's albums are shown: "grid" or "list" (each with its tracks).
    pub album_view: String,
    /// How the artists are ordered: "name", "most-played" or "recently-added".
    pub artist_sort: String,
    /// How the albums are ordered: "added", "played", "name" or "artist".
    pub albums_sort: String,
    /// How the playlists are ordered: "added" or "name".
    pub playlists_sort: String,
    /// How the artists are shown: "grid" (pictures) or "list".
    pub artist_view: String,
    /// Whether pictures of artists without one of their own come from Deezer, which is told
    /// the artists' names.
    pub artist_pictures_online: bool,
    /// Whether artists' pages show what Last.fm says of them, which is told their names.
    pub artist_bios_online: bool,
    /// Whether they show the opening of their Wikipedia article, found by their MusicBrainz id.
    pub artist_wikipedia_online: bool,
    /// The user's personal fanart.tv API key, for wide pictures across artists' pages.
    pub fanart_key: Option<String>,
    /// Whether the music folders are watched, and what changes in them scanned at once.
    pub watch_folders: bool,
    /// How often the whole library is scanned, in hours, for what watching misses, as on
    /// network drives; 0 for never.
    pub scan_every_hours: u32,
    /// Whether the whole library is scanned each time Freya starts; it is the first time
    /// anyway.
    pub scan_at_start: bool,
    /// The server on another computer the library is on.
    pub server: Option<SavedServer>,
    /// Whose library Up Next was saved from: the certificate of the server elsewhere, in hex,
    /// or none for this computer's. Restored only for the same.
    pub queue_from: Option<String>,
}

/// A server on another computer, as this client pinned it when it logged in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedServer {
    pub name: String,
    /// "192.168.1.20:47475".
    pub address: String,
    /// Its certificate, in DER, as hex.
    pub certificate: String,
}

impl SavedServer {
    pub fn new(name: &str, pin: &freya_client::Pin) -> Self {
        Self {
            name: name.to_owned(),
            address: pin.address.to_string(),
            certificate: pin.certificate.iter().map(|b| format!("{b:02x}")).collect(),
        }
    }

    /// Where it is and its certificate; none when what was kept cannot be read.
    pub fn pin(&self) -> Option<freya_client::Pin> {
        let certificate = (0..self.certificate.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(self.certificate.get(i..i + 2)?, 16).ok())
            .collect::<Option<Vec<u8>>>()?;
        Some(freya_client::Pin {
            address: self.address.parse().ok()?,
            certificate,
        })
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 0.8,
            shuffle: false,
            repeat: false,
            window_size: None,
            window_position: None,
            window_maximized: false,
            skin: None,
            theme: "system".to_owned(),
            equalizer_visible: false,
            visualizer_mode: "spectrum".to_owned(),
            visualizer_fps: 30,
            library_folders: Vec::new(),
            equalizer: EqualizerSettings::default(),
            check_for_updates: true,
            last_update_check: None,
            skipped_update: None,
            lastfm_user: None,
            lastfm_scrobbling: true,
            album_sort: "release-date".to_owned(),
            album_view: "grid".to_owned(),
            artist_sort: "name".to_owned(),
            albums_sort: "added".to_owned(),
            playlists_sort: "added".to_owned(),
            artist_view: "grid".to_owned(),
            artist_pictures_online: true,
            artist_bios_online: true,
            artist_wikipedia_online: true,
            fanart_key: None,
            watch_folders: true,
            scan_every_hours: 24,
            scan_at_start: false,
            server: None,
            queue_from: None,
            damaged: false,
        }
    }
}

fn dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "Freya")
}

fn settings_path() -> Option<PathBuf> {
    dirs().map(|d| d.config_dir().join("settings.toml"))
}

/// Where installed skins are kept.
pub fn skins_dir() -> Option<PathBuf> {
    dirs().map(|d| d.data_dir().join("skins"))
}

/// Files that can always be made again, like the cover shown in the system's Now Playing.
pub fn cache_dir() -> Option<PathBuf> {
    dirs().map(|d| d.cache_dir().to_owned())
}

/// The code Freya Server on this computer is adopted with, in its folder while it is not
/// adopted yet: readable by its user alone, as Freya here is.
pub fn adoption_code_path() -> Option<PathBuf> {
    dirs().map(|d| {
        d.data_dir()
            .join("server")
            .join(freya_protocol::ADOPTION_CODE)
    })
}

/// The certificate Freya's server on this computer keeps in its folder, which the client here
/// pins.
pub fn server_certificate_path() -> Option<PathBuf> {
    dirs().map(|d| {
        d.data_dir()
            .join("server")
            .join(freya_protocol::CERTIFICATE_FILE)
    })
}

/// Where this computer keeps the refresh tokens servers gave it, by their address: the
/// system's keychain where Freya uses one, else a file only the user can read.
pub fn token_store() -> Option<freya_client::TokenStore> {
    dirs().map(|d| freya_client::TokenStore::new(d.config_dir().join("servers.json"), true))
}

/// What this computer is called, for the server's list of devices.
pub fn device_name() -> String {
    let named = std::env::var("COMPUTERNAME").ok().or_else(|| {
        let out = std::process::Command::new("hostname").output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    });
    named
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "This computer".into())
}

/// Freya's log, of this run and earlier ones.
pub fn log_path() -> Option<PathBuf> {
    dirs().map(|d| d.data_dir().join("freya.log"))
}

/// Where the playlist is kept between runs.
pub fn playlist_path() -> Option<PathBuf> {
    dirs().map(|d| d.data_dir().join("playlist.m3u8"))
}

/// Saved radio stations, as an M3U playlist.
pub fn stations_path() -> Option<PathBuf> {
    dirs().map(|d| d.data_dir().join("stations.m3u8"))
}

impl Settings {
    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                log::warn!("could not read {}: {e}", path.display());
                return Self {
                    damaged: true,
                    ..Self::default()
                };
            }
        };
        let settings = Self::parse(&text);
        if settings.damaged {
            // Kept for the user, as the next save writes what could be read.
            let copy = path.with_extension("invalid.toml");
            log::warn!(
                "{} could not be read in full; a copy is kept as {}",
                path.display(),
                copy.display()
            );
            let _ = std::fs::write(copy, &text);
        }
        settings
    }

    /// Reads settings.toml. A field that cannot be read, like `visualizer_fps = 60.0` typed
    /// by hand, is left at its default rather than taking every other setting with it.
    fn parse(text: &str) -> Self {
        if let Ok(settings) = toml::from_str::<Self>(text) {
            return settings;
        }
        let Ok(table) = text.parse::<toml::Table>() else {
            return Self {
                damaged: true,
                ..Self::default()
            };
        };
        let readable: toml::Table = table
            .into_iter()
            .filter(|(key, value)| {
                let one = toml::Table::from_iter([(key.clone(), value.clone())]);
                let ok = Self::deserialize(one).is_ok();
                if !ok {
                    log::warn!("ignoring the setting {key}, which cannot be read");
                }
                ok
            })
            .collect();
        Self {
            damaged: true,
            ..Self::deserialize(readable).unwrap_or_default()
        }
    }

    pub fn save(&self) {
        let Some(path) = settings_path() else {
            return;
        };
        let result = toml::to_string_pretty(self)
            .map_err(std::io::Error::other)
            .and_then(|text| write_atomically(&path, text.as_bytes()));
        if let Err(e) = result {
            log::warn!("could not save settings to {}: {e}", path.display());
        }
    }
}

/// Writes `bytes` to `path` through a new file renamed over it, so a crash, a full disk or a
/// lost power supply leaves the old file or the new one, never half of one.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("partial");
    // Synced through the handle it was written with: Windows refuses to flush a file opened
    // only for reading.
    let mut file = std::fs::File::create(&partial)?;
    std::io::Write::write_all(&mut file, bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_field_costs_only_itself() {
        let text = "volume = 0.5\nvisualizer_fps = 60.0\nlibrary_folders = [\"/music\"]\n";
        let settings = Settings::parse(text);
        assert!(settings.damaged);
        assert_eq!(settings.volume, 0.5);
        assert_eq!(
            settings.visualizer_fps, 30,
            "the bad field takes its default"
        );
        assert_eq!(settings.library_folders, [PathBuf::from("/music")]);
    }

    #[test]
    fn a_broken_file_is_marked_damaged() {
        let settings = Settings::parse("volume = 0.5\nlibrary_folders = [\"/mus");
        assert!(settings.damaged);
        assert!(settings.library_folders.is_empty());
        assert!(!Settings::parse("volume = 0.5\n").damaged);
    }

    #[test]
    fn writes_replace_the_file_whole() {
        let dir = std::env::temp_dir().join(format!("freya-settings-{}", std::process::id()));
        let path = dir.join("settings.toml");
        write_atomically(&path, b"a = 1").unwrap();
        write_atomically(&path, b"a = 2").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 2");
        assert!(!path.with_extension("partial").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
