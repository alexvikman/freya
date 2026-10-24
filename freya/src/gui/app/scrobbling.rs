//! Telling Last.fm what plays, and what was heard.

use super::*;

impl App {
    /// The file, and the part of it, that a track started plays, when its plays are counted:
    /// not for radio.
    pub(super) fn countable<'a>(&self, track: &'a TrackInfo) -> Option<(&'a Path, Option<Part>)> {
        if track.station.is_some() || freya_audio::is_url(&track.path.to_string_lossy()) {
            return None;
        }
        // The playlist knows whether a track starting at 0 is a whole file or a CUE sheet's
        // first track.
        let part = match self.playlist.current_entry() {
            Some(entry) if entry.plays(&track.path, track.start) => entry.part,
            _ => (!track.start.is_zero() || track.end.is_some()).then_some(Part {
                start: track.start,
                end: track.end,
            }),
        };
        Some((&track.path, part))
    }

    /// A track started: Last.fm shows it as playing, if it may be scrobbled at all (tagged,
    /// not radio).
    pub(super) fn lastfm_started(&mut self, track: &TrackInfo) {
        self.lastfm_play = track
            .station
            .is_none()
            .then(|| {
                lastfm::track(
                    track.artist.as_deref(),
                    track.title.as_deref(),
                    track.album.as_deref(),
                    track.duration,
                )
            })
            .flatten()
            .map(|t| LastFmPlay {
                track: t,
                started: lastfm::now(),
                scrobble: lastfm::long_enough(track.duration),
            });
        self.lastfm_now_playing();
        self.lastfm_was_playing = true;
    }

    pub(super) fn lastfm_now_playing(&self) {
        if self.settings.lastfm_scrobbling
            && let Some(play) = &self.lastfm_play
        {
            self.lastfm.now_playing(play.track.clone());
        }
    }

    /// The track that plays has been heard enough to count: Last.fm gets the play.
    pub(super) fn lastfm_heard(&self) {
        if self.settings.lastfm_scrobbling
            && let Some(play) = self.lastfm_play.as_ref().filter(|p| p.scrobble)
        {
            self.lastfm.scrobble(lastfm::Scrobble {
                track: play.track.clone(),
                started: play.started,
            });
        }
    }
}
