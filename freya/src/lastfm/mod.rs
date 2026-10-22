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
/// Where Last.fm asks the user to let Freya scrobble: the only page a server may have the
/// client open.
const AUTH_PAGE: &str = "https://www.last.fm/api/auth/";

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

    /// Scrobbles through `server`, which said how it goes, `status`.
    pub fn use_server(
        &mut self,
        server: Arc<dyn Server>,
        status: freya_protocol::ApiResult<Status>,
    ) {
        match status {
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
            Ok(page) if is_auth_page(&page) => crate::update::open_page(&page),
            Ok(page) => {
                log::warn!("the server gave a page that is not Last.fm's: {page}");
                self.status.error = "The server gave a page that is not Last.fm's.".into();
            }
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

    /// Tells the server what plays, the window not waiting for it.
    pub fn now_playing(&self, track: Track) {
        let Some(server) = self.server.clone() else {
            return;
        };
        crate::errands::run(move || {
            if let Err(e) = server.lastfm_now_playing(&track) {
                log::warn!("could not tell Last.fm what plays: {e}");
            }
        });
    }

    /// Tells the server a play to scrobble, the window not waiting for it.
    pub fn scrobble(&self, scrobble: Scrobble) {
        let Some(server) = self.server.clone() else {
            return;
        };
        crate::errands::run(move || {
            if let Err(e) = server.lastfm_scrobble(&scrobble) {
                log::warn!("could not scrobble: {e}");
            }
        });
    }

    fn tell(&self, what: &str, call: impl FnOnce(&dyn Server) -> freya_protocol::ApiResult<()>) {
        if let Some(server) = &self.server
            && let Err(e) = call(server.as_ref())
        {
            log::warn!("could not {what}: {e}");
        }
    }
}

/// Whether `page` is Last.fm's page for letting Freya scrobble, and nothing else a server
/// could have this computer open.
fn is_auth_page(page: &str) -> bool {
    page.starts_with(AUTH_PAGE) && !page.chars().any(|c| c.is_whitespace() || c.is_control())
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
    fn only_lastfms_page_is_opened() {
        assert!(is_auth_page(
            "https://www.last.fm/api/auth/?api_key=k&token=t"
        ));
        assert!(!is_auth_page("file:///etc/passwd"));
        assert!(!is_auth_page("https://www.last.fm.evil.example/api/auth/"));
        assert!(!is_auth_page("https://www.last.fm/api/auth/?x=1 --args"));
        assert!(!is_auth_page("-a Terminal"));
    }

    #[test]
    fn short_tracks_are_not_scrobbled() {
        assert!(!long_enough(Some(Duration::from_secs(30))));
        assert!(long_enough(Some(Duration::from_secs(31))));
        assert!(long_enough(None));
    }
}
