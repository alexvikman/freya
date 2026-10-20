//! How a scan of the music's folders goes.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ScanEvent {
    /// `done` of `total` new or changed files (audio files or CUE sheets) have been read and
    /// written.
    Progress {
        done: usize,
        total: usize,
    },
    /// The folder being looked through or read now, told every so often.
    Looking(PathBuf),
    /// `done` of `total` albums' covers have been made.
    Covers {
        done: usize,
        total: usize,
    },
    /// The internet was asked what `done` of the `total` artists who lack something lack.
    Artists {
        done: usize,
        total: usize,
    },
    /// And of the `total` genres it has not said anything of.
    Genres {
        done: usize,
        total: usize,
    },
    Finished(ScanSummary),
    Failed(String),
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanSummary {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
}

/// What a server tells its clients as it happens.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ServiceEvent {
    /// The client is told what happens from now on; whether a scan runs now.
    Connected { scanning: bool },
    /// A scan began, or asking the internet without one.
    ScanStarted,
    /// How it goes.
    Scan(ScanEvent),
    /// It ended, done or not; what it wrote is in the library.
    ScanEnded,
    /// Plays noted while a scan wrote are in the library now.
    PlaysWritten,
}

/// How the server keeps the library, as the user set it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct ServerSettings {
    /// The folders the library is made from.
    pub library_folders: Vec<PathBuf>,
    /// Whether the folders are watched, and what changes in them scanned at once.
    pub watch_folders: bool,
    /// How often the whole library is scanned, in hours, for what watching misses, as on
    /// network drives; 0 for never.
    pub scan_every_hours: u32,
    /// Whether the whole library is scanned each time the server starts; it is the first time
    /// anyway.
    pub scan_at_start: bool,
    /// Whether pictures of artists without one of their own come from Deezer, which is told
    /// the artists' names.
    pub artist_pictures_online: bool,
    /// Whether what Last.fm says of artists is asked, which is told their names.
    pub artist_bios_online: bool,
    /// Whether the opening of their Wikipedia article is asked, found by their MusicBrainz id.
    pub artist_wikipedia_online: bool,
    /// The user's personal fanart.tv API key, for wide pictures across artists' pages.
    pub fanart_key: Option<String>,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            library_folders: Vec::new(),
            watch_folders: true,
            scan_every_hours: 24,
            scan_at_start: false,
            artist_pictures_online: true,
            artist_bios_online: true,
            artist_wikipedia_online: true,
            fanart_key: None,
        }
    }
}
