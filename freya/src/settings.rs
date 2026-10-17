//! Settings and the playlist, kept between runs.

use std::path::PathBuf;

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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 0.8,
            shuffle: false,
            repeat: false,
            window_size: None,
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

/// The library database. It can always be rebuilt from the music folders.
pub fn library_path() -> Option<PathBuf> {
    dirs().map(|d| d.data_dir().join("library.sqlite3"))
}

/// Where the playlist is kept between runs.
pub fn playlist_path() -> Option<PathBuf> {
    dirs().map(|d| d.data_dir().join("playlist.m3u8"))
}

/// The folder the user's playlists are kept in, one M3U file each.
pub fn playlists_dir() -> Option<PathBuf> {
    dirs().map(|d| d.data_dir().join("playlists"))
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
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                log::warn!("ignoring invalid {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = settings_path() else {
            return;
        };
        let result = toml::to_string_pretty(self)
            .map_err(|e| e.to_string())
            .and_then(|text| {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                std::fs::write(&path, text).map_err(|e| e.to_string())
            });
        if let Err(e) = result {
            log::warn!("could not save settings to {}: {e}", path.display());
        }
    }
}
