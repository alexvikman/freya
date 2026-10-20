//! Scrobbling to Last.fm, which the server does for the user once they connect their account.

use serde::{Deserialize, Serialize};

/// A track as Last.fm is told about it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastFmTrack {
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    /// Whole seconds.
    pub duration: Option<u64>,
}

/// A play to scrobble: the track, and when it started (seconds since 1970, UTC).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scrobble {
    pub track: LastFmTrack,
    pub started: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LastFmState {
    /// The server has no Last.fm API key.
    #[default]
    Unavailable,
    Disconnected,
    /// The session kept for the user could not be read (a locked keychain, say). Shown as
    /// disconnected, but the user is not forgotten: the session may still be there next time.
    Unreadable,
    /// Waiting for the user to allow Freya on Last.fm's page.
    Authorizing,
    Connected,
}

impl LastFmState {
    /// The name the skin API uses.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Disconnected | Self::Unreadable => "disconnected",
            Self::Authorizing => "authorizing",
            Self::Connected => "connected",
        }
    }
}

/// How scrobbling is going, for the settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastFmStatus {
    pub state: LastFmState,
    /// The connected Last.fm user.
    pub user: String,
    /// Plays waiting to be sent.
    pub waiting: usize,
    /// What went wrong last, until something works again.
    pub error: String,
}
