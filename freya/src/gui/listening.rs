//! When a track counts as played: once it has been listened to for half its length, or for
//! four minutes, whichever comes first. Only time spent listening counts, so skipping ahead
//! through a track does not.

use std::path::{Path, PathBuf};
use std::time::Duration;

use freya_protocol::Part;

/// Listening this long makes any track played.
const ENOUGH: Duration = Duration::from_secs(4 * 60);
/// A step in the position larger than this is a seek, not listening.
const MAX_STEP: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct Listening {
    /// The track playing, if it is one that can be counted.
    track: Option<(PathBuf, Option<Part>)>,
    needed: Duration,
    listened: Duration,
    last_position: Option<Duration>,
    counted: bool,
}

impl Listening {
    /// A track started, from its beginning; `None` for what is never counted, like radio.
    pub fn start(&mut self, track: Option<(&Path, Option<Part>)>, duration: Option<Duration>) {
        *self = Self {
            track: track.map(|(path, part)| (path.to_owned(), part)),
            needed: duration.map_or(ENOUGH, |d| (d / 2).min(ENOUGH)),
            ..Self::default()
        };
    }

    /// Follows the player. Returns the track once, when it has been listened to enough.
    pub fn update(&mut self, position: Duration, playing: bool) -> Option<(PathBuf, Option<Part>)> {
        let last = self.last_position.replace(position);
        if !playing || self.counted {
            return None;
        }
        if let Some(step) = last.and_then(|last| position.checked_sub(last))
            && step <= MAX_STEP
        {
            self.listened += step;
        }
        if self.listened < self.needed {
            return None;
        }
        self.counted = true;
        self.track.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    /// Plays from `from` to `to` seconds, a second at a time; returns how many plays counted.
    fn play(listening: &mut Listening, from: u64, to: u64) -> usize {
        (from..=to)
            .filter_map(|s| listening.update(secs(s), true))
            .count()
    }

    #[test]
    fn counts_half_a_track_once() {
        let mut listening = Listening::default();
        listening.start(Some((Path::new("/m/a.flac"), None)), Some(secs(180)));
        assert_eq!(play(&mut listening, 0, 89), 0);
        assert_eq!(
            listening.update(secs(90), true),
            Some((PathBuf::from("/m/a.flac"), None))
        );
        assert_eq!(play(&mut listening, 91, 180), 0, "counted once");
    }

    #[test]
    fn long_tracks_count_after_four_minutes() {
        let mut listening = Listening::default();
        listening.start(Some((Path::new("/m/long.flac"), None)), Some(secs(3600)));
        assert_eq!(play(&mut listening, 0, 239), 0);
        assert_eq!(play(&mut listening, 240, 241), 1);
    }

    #[test]
    fn seeking_and_pausing_do_not_count() {
        let mut listening = Listening::default();
        listening.start(Some((Path::new("/m/a.flac"), None)), Some(secs(180)));
        assert_eq!(play(&mut listening, 0, 10), 0);
        // Skipped ahead, to near the end.
        assert_eq!(play(&mut listening, 170, 180), 0);
        // Paused: the position does not move, and nothing counts.
        assert_eq!(listening.update(secs(180), false), None);
        // Back to the start, listening on.
        assert_eq!(play(&mut listening, 0, 70), 1);
    }

    #[test]
    fn radio_never_counts() {
        let mut listening = Listening::default();
        listening.start(None, None);
        assert_eq!(play(&mut listening, 0, 600), 0);
    }
}
