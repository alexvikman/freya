//! Scrobbles waiting to be sent, kept on disk so none are lost to a closed Freya or a missing
//! network. The queue belongs to one Last.fm user: it is never sent to another account.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::api::Scrobble;

/// At most this many plays wait; beyond it the oldest go. Last.fm ignores plays older than two
/// weeks anyway, and this is more than that of listening.
const MAX_WAITING: usize = 10_000;

#[derive(Default, Serialize, Deserialize)]
pub struct Queue {
    /// The Last.fm user the plays are for.
    user: String,
    scrobbles: Vec<Scrobble>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Queue {
    /// The queue kept at `path` for `user`. A queue kept for another user is left out.
    pub fn load(path: Option<PathBuf>, user: &str) -> Self {
        let kept: Option<Self> = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok());
        let mut queue = kept.filter(|q| q.user == user).unwrap_or_default();
        queue.user = user.to_owned();
        queue.path = path;
        queue
    }

    pub fn len(&self) -> usize {
        self.scrobbles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scrobbles.is_empty()
    }

    pub fn push(&mut self, scrobble: Scrobble) {
        self.scrobbles.push(scrobble);
        if self.scrobbles.len() > MAX_WAITING {
            let excess = self.scrobbles.len() - MAX_WAITING;
            self.scrobbles.drain(..excess);
        }
        self.save();
    }

    /// The oldest `count` plays, in the order they were played.
    pub fn oldest(&self, count: usize) -> &[Scrobble] {
        &self.scrobbles[..count.min(self.scrobbles.len())]
    }

    /// Forgets the oldest `count` plays, once Last.fm has them.
    pub fn remove_oldest(&mut self, count: usize) {
        self.scrobbles.drain(..count.min(self.scrobbles.len()));
        self.save();
    }

    /// Forgets every play, as when the user disconnects.
    pub fn clear(&mut self) {
        self.scrobbles.clear();
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let result = serde_json::to_string(self)
            .map_err(std::io::Error::other)
            .and_then(|text| {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                // Written whole and then renamed, so a crash leaves the old queue or the new.
                let partial = path.with_extension("json.partial");
                std::fs::write(&partial, text)?;
                std::fs::rename(&partial, path)
            });
        if let Err(e) = result {
            log::warn!("could not keep the Last.fm queue: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lastfm::api::Track;

    fn play(started: u64) -> Scrobble {
        Scrobble {
            track: Track {
                artist: "Artist".into(),
                title: format!("Song {started}"),
                album: None,
                duration: Some(200),
            },
            started,
        }
    }

    #[test]
    fn keeps_plays_in_order_across_runs() {
        let dir = std::env::temp_dir().join(format!("freya-queue-{}", std::process::id()));
        let path = dir.join("lastfm-queue.json");
        let mut queue = Queue::load(Some(path.clone()), "anna");
        queue.push(play(1));
        queue.push(play(2));
        queue.push(play(3));
        queue.remove_oldest(1);

        let again = Queue::load(Some(path.clone()), "anna");
        assert_eq!(again.oldest(10), [play(2), play(3)]);
        // Another account never gets them.
        assert!(Queue::load(Some(path.clone()), "bo").is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
