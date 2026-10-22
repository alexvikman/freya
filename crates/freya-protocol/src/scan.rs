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
    /// The client is told what happens from now on: whether a scan runs now, and how many
    /// events the server told before, so a client following it again knows whether it missed
    /// any.
    Connected { scanning: bool, told: u64 },
    /// The client lost the server's events, and looks for it again: told by the client itself,
    /// never by a server.
    Lost,
    /// A scan began, or asking the internet without one.
    ScanStarted,
    /// How it goes.
    Scan(ScanEvent),
    /// It ended, done or not; what it wrote is in the library.
    ScanEnded,
    /// Plays noted while a scan wrote are in the library now.
    PlaysWritten,
    /// The user's playlists changed: one was made, changed, renamed or taken away.
    PlaylistsChanged,
    /// How scrobbling to Last.fm goes, now that it changed.
    LastFm(crate::LastFmStatus),
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
    /// Whether clients on other computers may connect, over TLS, and find the server.
    pub network: bool,
    /// What the server is called where clients find it; the computer's name when empty.
    pub name: String,
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
            network: true,
            name: String::new(),
        }
    }
}

/// Where a server takes clients on other computers, over TLS, and answers those looking for
/// servers on the network.
pub const NETWORK_PORT: u16 = 47475;
pub const DISCOVERY_PORT: u16 = 47476;
/// What a client looking for servers sends, to [`DISCOVERY_PORT`] of every computer on the
/// network; a server answers with a [`ServerFound`] in JSON.
pub const DISCOVERY_QUESTION: &[u8] = b"freya-server?";

/// A server on the network, as it answers one looking for servers.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ServerFound {
    pub name: String,
    /// Where it takes clients over TLS.
    pub port: u16,
    /// Its certificate's fingerprint, for the user to compare.
    pub fingerprint: String,
    pub version: String,
}

/// What a user sees of their account.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AccountInfo {
    pub user: String,
    /// Whether they can log in on other devices.
    pub has_password: bool,
    pub devices: Vec<DeviceInfo>,
}

/// The folders in a folder on the server, for choosing the music's folders from another
/// computer.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderListing {
    /// The folder listed; none for where to start: the user's home, and the drives.
    pub folder: Option<PathBuf>,
    /// The folder it is in; none at the start.
    pub parent: Option<PathBuf>,
    /// By name; hidden ones left out.
    pub folders: Vec<PathBuf>,
}

/// A device a user connected, with a token of its own.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub created: std::time::SystemTime,
    /// When it last called the server, to the hour; none when not since it was kept.
    #[serde(default)]
    pub used: Option<std::time::SystemTime>,
    /// How it was connected; none for a device from before it was kept.
    #[serde(default)]
    pub joined: Option<Joined>,
    /// The device asking.
    pub this: bool,
}

/// How a device was connected to its user.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Joined {
    /// With the pairing key, on the server's own computer.
    Paired,
    /// By logging in, in the browser.
    LoggedIn,
}
