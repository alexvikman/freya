//! Reading tags and durations without decoding audio.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use lofty::prelude::*;
use lofty::tag::ItemKey;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TagInfo {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    /// MusicBrainz's id of the album's artist, as Picard writes it: of the album artist, else
    /// of the artist.
    pub artist_mbid: Option<String>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub duration: Option<Duration>,
}

/// Reads tags and duration from a file. Returns `None` if the file cannot be parsed.
pub fn read_tags(path: &Path) -> Option<TagInfo> {
    // Without the pictures, which can be megabytes per file and are read for covers alone.
    let options = lofty::config::ParseOptions::new().read_cover_art(false);
    let file = lofty::probe::Probe::open(path)
        .ok()?
        .options(options)
        .read()
        .ok()?;
    let duration = Some(file.properties().duration()).filter(|d| !d.is_zero());
    let tag = file.primary_tag().or_else(|| file.first_tag());
    let text = |s: Option<std::borrow::Cow<'_, str>>| {
        s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
    };
    Some(TagInfo {
        title: tag.and_then(|t| text(t.title())),
        artist: tag.and_then(|t| text(t.artist())),
        album: tag.and_then(|t| text(t.album())),
        album_artist: tag.and_then(|t| text(t.get_string(ItemKey::AlbumArtist).map(Into::into))),
        artist_mbid: tag.and_then(|t| {
            [
                ItemKey::MusicBrainzReleaseArtistId,
                ItemKey::MusicBrainzArtistId,
            ]
            .iter()
            .find_map(|key| t.get_string(*key).and_then(mbid))
        }),
        genre: tag.and_then(|t| text(t.genre())),
        year: tag.and_then(|t| t.date()).map(|d| u32::from(d.year)),
        track: tag.and_then(|t| t.track()),
        disc: tag.and_then(|t| t.disk()),
        duration,
    })
}

/// The first MusicBrainz id in a tag's text: one with several artists holds an id for each.
fn mbid(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
        .find(|id| {
            id.len() == 36
                && id
                    .char_indices()
                    .all(|(i, c)| [8, 13, 18, 23].contains(&i) == (c == '-'))
        })
        .map(str::to_ascii_lowercase)
}

/// Reads tags on a background thread so adding thousands of files stays responsive.
pub struct TagLoader {
    requests: Sender<PathBuf>,
    results: Receiver<(PathBuf, TagInfo)>,
}

impl Default for TagLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl TagLoader {
    pub fn new() -> Self {
        let (req_tx, req_rx) = channel::<PathBuf>();
        let (res_tx, res_rx) = channel();
        std::thread::Builder::new()
            .name("freya-tags".into())
            .spawn(move || {
                for path in req_rx {
                    // Unreadable files still get a result, so they are not requested again.
                    let tags = read_tags(&path).unwrap_or_default();
                    if res_tx.send((path, tags)).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn tag thread");
        Self {
            requests: req_tx,
            results: res_rx,
        }
    }

    pub fn request(&self, path: PathBuf) {
        let _ = self.requests.send(path);
    }

    /// Results that have arrived since the last call.
    pub fn poll(&self) -> impl Iterator<Item = (PathBuf, TagInfo)> + '_ {
        self.results.try_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../freya-audio/tests/fixtures")
            .join(name)
    }

    #[test]
    fn reads_id3_and_ape_tags_and_duration() {
        for name in ["sine.mp3", "sine.ape"] {
            let tags = read_tags(&fixture(name)).unwrap();
            assert_eq!(tags.title.as_deref(), Some("Sine Test"), "{name}");
            assert_eq!(tags.artist.as_deref(), Some("Freya"), "{name}");
            let secs = tags.duration.unwrap().as_secs_f64();
            assert!((secs - 1.0).abs() < 0.1, "{name}: {secs}");
        }
    }

    #[test]
    fn takes_the_first_musicbrainz_id() {
        let id = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
        assert_eq!(mbid(id).as_deref(), Some(id));
        let two = format!(
            "{} / 5b11f4ce-a62d-471e-81fc-a69a8278c7da",
            id.to_uppercase()
        );
        assert_eq!(mbid(&two).as_deref(), Some(id));
        assert_eq!(mbid("not an id"), None);
        assert_eq!(mbid("a74b1b7f71a5401194419d0b5e4122711abc"), None);
    }

    #[test]
    fn loader_returns_results_for_unreadable_files() {
        let loader = TagLoader::new();
        loader.request(fixture("sine.flac"));
        loader.request(PathBuf::from("/nonexistent.mp3"));
        let mut results = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while results.len() < 2 && std::time::Instant::now() < deadline {
            results.extend(loader.poll());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(results.len(), 2);
        assert!(results[0].1.duration.is_some());
        assert_eq!(results[1].1, TagInfo::default());
    }
}
