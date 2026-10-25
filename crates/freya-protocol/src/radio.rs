//! The radio directory: stations the server knows of, to find by their tags and names, beside
//! the stations it keeps for its users (see [`crate::RadioStation`]).

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// What a tag says of a station. A station has tags of every kind it can: "jazz", "Italy",
/// "Italian" and "Lossless" at once.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum RadioTagKind {
    /// How the stream sounds: "Hi-Res", "Lossless", "High bitrate".
    Quality,
    Genre,
    /// The decade its music is of: "80s".
    Era,
    /// What it is besides music: "News", "Talk", "Sports".
    Format,
    Country,
    Language,
}

impl RadioTagKind {
    /// The kind a tag's id begins with, as in "genre:jazz".
    pub fn of(tag: &str) -> Option<Self> {
        let (kind, _) = tag.split_once(':')?;
        Some(match kind {
            "quality" => Self::Quality,
            "genre" => Self::Genre,
            "era" => Self::Era,
            "format" => Self::Format,
            "country" => Self::Country,
            "language" => Self::Language,
            _ => return None,
        })
    }

    /// How a tag's id begins: "genre".
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Quality => "quality",
            Self::Genre => "genre",
            Self::Era => "era",
            Self::Format => "format",
            Self::Country => "country",
            Self::Language => "language",
        }
    }
}

/// A tag, with how many stations of the directory have it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RadioTag {
    /// Its kind and name for a program, "genre:jazz" or "country:it".
    pub id: String,
    pub kind: RadioTagKind,
    /// As shown: "Jazz", "Italy".
    pub name: String,
    pub stations: u32,
}

/// A station of the directory.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DirectoryStation {
    /// Which it is, kept the same between the directory's updates: Radio Browser's id, or
    /// "freya:" and a name for a station Freya picked.
    pub id: String,
    pub name: String,
    /// The address of its stream, or of its .pls or .m3u file.
    pub url: String,
    pub homepage: Option<String>,
    /// The address of its logo, as the station gives it.
    pub logo: Option<String>,
    /// Its tags' ids, most telling first: quality, genres, eras, formats, country, languages.
    pub tags: Vec<String>,
    /// "MP3", "AAC", "OGG", "FLAC"; empty when not known.
    pub codec: String,
    /// In kilobits a second; 0 when not known, as for a lossless stream.
    pub bitrate: u32,
    /// Whether Freya picked it.
    pub featured: bool,
}

/// What to look for in the directory: every station with all of `tags` whose name, or a tag's,
/// has every word of `text` in it, the best known first.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct RadioQuery {
    pub text: String,
    pub tags: Vec<String>,
    /// How many of those found to pass over, and how many to give after them.
    pub offset: u32,
    pub limit: u32,
}

/// What the directory found.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct RadioResults {
    pub stations: Vec<DirectoryStation>,
    /// How many were found, of which `stations` are those asked for.
    pub total: u32,
    /// The tags of those found, but those asked for, with how many have each, the most first:
    /// what would find fewer of them.
    pub tags: Vec<RadioTag>,
}

/// A row of stations on the directory's first page: a tag, or a few together, and the best
/// known stations with them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RadioShelf {
    pub title: String,
    /// The tags its stations have; none for the stations Freya picked.
    pub tags: Vec<String>,
    pub stations: Vec<DirectoryStation>,
}

/// How the directory is.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct RadioDirectoryStatus {
    /// "loading" while it is read or first fetched, "ready", "off" when the server may not ask
    /// Radio Browser (only the stations Freya picked are in it then), or "failed".
    pub state: String,
    pub stations: u32,
    /// When Radio Browser's stations were fetched last.
    pub fetched: Option<SystemTime>,
    /// Why fetching them failed last, for an admin; empty when it did not.
    pub error: String,
}
