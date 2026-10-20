//! Scrobbling to Last.fm, if the user connects their account (U8 in PLAN.md), which the server
//! does for them: the client tells it what plays, and shows how it is going.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use freya_protocol::Server;
pub use freya_protocol::{
    LastFmState as State, LastFmStatus as Status, LastFmTrack as Track, Scrobble,
};

/// Tracks this short are never scrobbled, as Last.fm asks.
const MIN_DURATION: Duration = Duration::from_secs(30);

pub struct LastFm {
    /// The server that scrobbles; none until connected.
    server: Option<Arc<dyn Server>>,
    /// How it is going, as the server last told.
    status: Status,
}

impl LastFm {
    pub fn new() -> Self {
        Self {
            server: None,
            status: Status::default(),
        }
    }

    /// Scrobbles through `server` from now on.
    pub fn use_server(&mut self, server: Arc<dyn Server>) {
        match server.lastfm_status() {
            Ok(status) => self.status = status,
            Err(e) => log::warn!("could not ask how scrobbling goes: {e}"),
        }
        self.server = Some(server);
    }

    /// How it is going, now that the server told.
    pub fn set_status(&mut self, status: Status) {
        self.status = status;
    }

    pub fn status(&self) -> Status {
        self.status.clone()
    }

    /// Opens Last.fm's page in the browser, where the user lets Freya scrobble for them; the
    /// server waits for them to.
    pub fn connect(&mut self) {
        let Some(server) = &self.server else {
            return;
        };
        match server.lastfm_connect() {
            Ok(page) => crate::update::open_page(&page),
            Err(e) => self.status.error = e.to_string(),
        }
    }

    /// Stops waiting for the user to allow Freya.
    pub fn cancel(&self) {
        self.tell("stop connecting to Last.fm", |server| {
            server.lastfm_cancel()
        });
    }

    /// Forgets the session and the plays waiting.
    pub fn disconnect(&self) {
        self.tell("disconnect from Last.fm", |server| {
            server.lastfm_disconnect()
        });
    }

    pub fn now_playing(&self, track: Track) {
        self.tell("tell Last.fm what plays", |server| {
            server.lastfm_now_playing(&track)
        });
    }

    pub fn scrobble(&self, scrobble: Scrobble) {
        self.tell("scrobble", |server| server.lastfm_scrobble(&scrobble));
    }

    fn tell(&self, what: &str, call: impl FnOnce(&dyn Server) -> freya_protocol::ApiResult<()>) {
        if let Some(server) = &self.server
            && let Err(e) = call(server.as_ref())
        {
            log::warn!("could not {what}: {e}");
        }
    }
}

/// The track Last.fm is told about, if it may be: tagged with an artist and a title (never
/// guessed from file names), and not a placeholder like "Unknown Artist".
pub fn track(
    artist: Option<&str>,
    title: Option<&str>,
    album: Option<&str>,
    duration: Option<Duration>,
) -> Option<Track> {
    let tag = |value: Option<&str>| {
        let value = value?.trim();
        (!value.is_empty() && !is_placeholder(value)).then(|| value.to_owned())
    };
    Some(Track {
        artist: tag(artist)?,
        title: tag(title)?,
        album: tag(album),
        duration: duration.map(|d| d.as_secs()),
    })
}

/// Whether a play of a track this long may be scrobbled: not when it is 30 seconds or
/// shorter. One of unknown length is, once enough of it was heard.
pub fn long_enough(duration: Option<Duration>) -> bool {
    duration.is_none_or(|d| d > MIN_DURATION)
}

/// Seconds since 1970, as Last.fm counts when a play started.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn is_placeholder(value: &str) -> bool {
    let value = value.to_lowercase();
    [
        "unknown",
        "unknown artist",
        "unknown track",
        "unknown title",
        "[unknown]",
        "<unknown>",
    ]
    .contains(&value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tagged_tracks_are_told_about() {
        let track = track(Some(" Opeth "), Some("Blackwater Park"), Some(""), None).unwrap();
        assert_eq!(track.artist, "Opeth");
        assert_eq!(track.album, None);
        assert!(super::track(None, Some("Song"), None, None).is_none());
        assert!(super::track(Some("Unknown Artist"), Some("Song"), None, None).is_none());
        assert!(super::track(Some("Artist"), Some("  "), None, None).is_none());
    }

    #[test]
    fn short_tracks_are_not_scrobbled() {
        assert!(!long_enough(Some(Duration::from_secs(30))));
        assert!(long_enough(Some(Duration::from_secs(31))));
        assert!(long_enough(None));
    }
}
