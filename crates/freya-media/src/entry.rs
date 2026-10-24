//! An entry of a playlist: a file, or a track of a CUE sheet in one, with what is known of it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cue::Part;
use crate::tags::TagInfo;

#[derive(Clone, Debug)]
pub struct Entry {
    /// Names this entry in its playlist, unique there and kept through every edit, so a track
    /// that plays can be told from another entry for the same file. Given by
    /// a playlist as it is added; 0 outside a playlist. Not part of what makes entries equal.
    pub id: u64,
    pub path: PathBuf,
    /// The part of the file this entry plays, for a track of a CUE sheet.
    pub part: Option<Part>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
    pub selected: bool,
    /// Whether tags have been read from the file (as opposed to coming from a playlist file).
    pub tags_loaded: bool,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        // Everything but `id`.
        let Self {
            id: _,
            path,
            part,
            title,
            artist,
            album,
            duration,
            selected,
            tags_loaded,
        } = self;
        (
            path,
            part,
            title,
            artist,
            album,
            duration,
            selected,
            tags_loaded,
        ) == (
            &other.path,
            &other.part,
            &other.title,
            &other.artist,
            &other.album,
            &other.duration,
            &other.selected,
            &other.tags_loaded,
        )
    }
}

impl Entry {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            id: 0,
            path: path.into(),
            part: None,
            title: None,
            artist: None,
            album: None,
            duration: None,
            selected: false,
            tags_loaded: false,
        }
    }

    /// An entry whose tags are known already: a library track, or a track of a CUE sheet.
    pub fn with_tags(path: impl Into<PathBuf>, part: Option<Part>, tags: &TagInfo) -> Self {
        let duration = tags
            .duration
            .or_else(|| part.and_then(|p| p.duration(None)));
        Self {
            part,
            title: tags.title.clone(),
            artist: tags.artist.clone(),
            album: tags.album.clone(),
            duration,
            // The last track of a CUE sheet learns its length from the file.
            tags_loaded: part.is_none() || duration.is_some(),
            ..Self::new(path)
        }
    }

    /// Where in the file the entry starts: zero for a whole file.
    pub fn start(&self) -> Duration {
        self.part.map_or(Duration::ZERO, |p| p.start)
    }

    /// Whether this entry plays `path` from `start`.
    pub fn plays(&self, path: &Path, start: Duration) -> bool {
        self.path == path && self.start() == start
    }

    /// "Artist - Title" if known, otherwise the file name without extension.
    pub fn display_title(&self) -> String {
        match (&self.artist, &self.title) {
            (Some(artist), Some(title)) => format!("{artist} - {title}"),
            (None, Some(title)) => title.clone(),
            _ => self
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.path.to_string_lossy().into_owned()),
        }
    }

    /// Takes the tags read from the entry's file.
    pub fn apply_tags(&mut self, tags: &TagInfo) {
        match self.part {
            // The file's tags describe the whole disc. Only its length is of use, for the
            // length of its last track.
            Some(part) => {
                if self.duration.is_none() {
                    self.duration = part.duration(tags.duration);
                }
            }
            None => {
                if tags.title.is_some() {
                    self.title.clone_from(&tags.title);
                    self.artist.clone_from(&tags.artist);
                    self.album.clone_from(&tags.album);
                }
                if tags.duration.is_some() {
                    self.duration = tags.duration;
                }
            }
        }
        self.tags_loaded = true;
    }
}
