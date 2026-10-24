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
    /// The client is told what happens from now on: whether a scan runs now, how many events
    /// the server told before, so a client following it again knows whether it missed any,
    /// and whether it is told of each change to the library ([`ServiceEvent::Changed`]).
    Connected {
        scanning: bool,
        told: u64,
        #[serde(default)]
        changes: bool,
    },
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
    /// What the library holds changed, by a scan, a client or a play: answers kept of
    /// [`crate::CACHED_CALLS`] are to be asked again.
    Changed,
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
    /// Whether the server starts when its user logs in to its computer, so the library is
    /// there without anyone starting it.
    pub start_at_login: bool,
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
            start_at_login: true,
        }
    }
}

/// How updating the server goes, for its admins: Freya Server looks for a newer release of
/// itself, and installs it only when an admin says so.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct ServerUpdate {
    /// "idle", "checking", "up-to-date", "available", "downloading", "installed" (the server
    /// restarts into it) or "failed".
    pub state: String,
    /// The running server's version, and the newer one while one is available.
    pub current: String,
    pub version: String,
    /// The new version's release notes, as plain text.
    pub notes: String,
    /// 0 to 1 while downloading.
    pub progress: f32,
    pub error: String,
    /// Whether the server installs it itself; otherwise `page` is where to get it, as for a
    /// Windows installer or a `.deb`.
    pub can_install: bool,
    pub page: String,
}

/// Where a server takes clients on other computers, over TLS, and answers those looking for
/// servers on the network.
pub const NETWORK_PORT: u16 = 47475;
pub const DISCOVERY_PORT: u16 = 47476;
/// What a client looking for servers sends, to [`DISCOVERY_PORT`] of every computer on the
/// network; a server answers with a [`ServerFound`] in JSON.
pub const DISCOVERY_QUESTION: &[u8] = b"freya-server?";

/// What a client on the server's computer signs with the pairing key, by HMAC-SHA256, to pair
/// without sending the key: its nonce and the name it pairs as.
pub fn pairing_message(nonce: &[u8], device: &str) -> Vec<u8> {
    [b"freya pairing\0".as_slice(), nonce, device.as_bytes()].concat()
}

/// A connection code: what a new device types to know that the server it reached is the one
/// whose computer shows the code. Crockford's base 32, which leaves out letters that look like
/// digits, ten of them: 50 bits.
pub const CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const CODE_LENGTH: usize = 10;
/// How many PBKDF2 rounds a code's key takes, as a password's does: so that each guess at a
/// code, made from a proof the server gave, costs as much, and finding one takes thousands of
/// years while a code lasts minutes.
pub const CODE_ROUNDS: u32 = 600_000;

/// The code `typed` is, its look-alikes read as meant: "k7q2m-9xd4t" and "K7Q2M 9XD4T" are
/// "K7Q2M9XD4T", an O a 0, an I or L a 1; none when it is not one.
pub fn normalize_code(typed: &str) -> Option<String> {
    let code: String = typed
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect();
    let valid = code.len() == CODE_LENGTH && code.bytes().all(|b| CODE_ALPHABET.contains(&b));
    valid.then_some(code)
}

/// A code as it is shown: "K7Q2M-9XD4T".
pub fn show_code(code: &str) -> String {
    let (first, last) = code.split_at(code.len() / 2);
    format!("{first}-{last}")
}

/// The key a server proves it knows the code with: PBKDF2 of the code, salted with the SHA-256
/// of the certificate, so each server, and each certificate passed off as one, needs a search
/// of its own.
pub fn connection_key(code: &str, certificate_digest: &[u8]) -> [u8; 32] {
    let mut key = [0; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        std::num::NonZeroU32::new(CODE_ROUNDS).unwrap_or(std::num::NonZeroU32::MIN),
        certificate_digest,
        code.as_bytes(),
        &mut key,
    );
    key
}

/// What a server signs with the code's key, by HMAC-SHA256, to prove it knows the code: the
/// client's nonce and the SHA-256 of the certificate the client was given. Another computer in
/// its place, with a certificate of its own, cannot.
pub fn connection_message(nonce: &[u8], certificate_digest: &[u8]) -> Vec<u8> {
    [
        b"freya connection code\0".as_slice(),
        nonce,
        certificate_digest,
    ]
    .concat()
}

/// What a device signs with a code's key, by HMAC-SHA256, to prove to the server that it knows
/// the code too: to adopt a server, with its connection code, or to join it, with the code a
/// user was invited with. Apart from what the server signs, so neither stands for the other.
pub fn claim_message(nonce: &[u8], certificate_digest: &[u8]) -> Vec<u8> {
    [b"freya claim\0".as_slice(), nonce, certificate_digest].concat()
}

/// What a device that proved it knows a code may do: adopt the server, choosing its owner's
/// name and password, or join it as the user invited, choosing their password.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Claim {
    Adopt,
    Join { user: String },
}

/// A user of the server, as an admin sees them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct UserInfo {
    pub name: String,
    pub role: Role,
    /// Whether they chose a password, and can log in.
    pub joined: bool,
    /// Until when the code they were invited with lasts, while it does.
    pub invited_until: Option<std::time::SystemTime>,
    pub devices: u32,
    /// The server's owner, who cannot be removed.
    pub owner: bool,
}

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

/// What a user may do: an admin keeps the server, its settings, scans and the library everyone
/// shares; a listener plays it, with their own plays, favourites, playlists and Last.fm (see
/// [`crate::LISTENER_CALLS`]).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    #[default]
    Admin,
    Listener,
}

/// What a user sees of their account.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AccountInfo {
    pub user: String,
    #[serde(default)]
    pub role: Role,
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
