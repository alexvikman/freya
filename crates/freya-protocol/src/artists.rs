//! What is known of artists beyond their tracks: what the user changed, their pictures, what
//! the internet says of them, and the artists and albums MusicBrainz knows.

use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// What the user changed about an artist; `None` where they changed nothing.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ArtistEdit {
    /// The name shown instead of the tags' own.
    pub name: Option<String>,
    /// The name sorted by: "Beatles, The".
    pub sort_name: Option<String>,
    /// Their MusicBrainz id, over the tags' and MusicBrainz's search.
    pub mbid: Option<String>,
    /// Their biography, in place of what Last.fm says.
    pub biography: Option<String>,
    /// What tells them from others of the name, as MusicBrainz has it for `mbid`.
    pub note: Option<String>,
    /// What the top of their page shows: their wide picture, or their picture, whichever
    /// they have; none for the wide one when they have one.
    pub header: Option<Header>,
}

/// What the top of an artist's page shows.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Header {
    Wide,
    Picture,
}

impl Header {
    pub fn name(self) -> &'static str {
        match self {
            Self::Wide => "wide",
            Self::Picture => "picture",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "wide" => Some(Self::Wide),
            "picture" => Some(Self::Picture),
            _ => None,
        }
    }
}

impl ArtistEdit {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The same, with blank fields as unchanged.
    pub fn trimmed(&self) -> Self {
        let field = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let mbid = field(&self.mbid);
        Self {
            name: field(&self.name),
            sort_name: field(&self.sort_name),
            // Of the id, and gone with it.
            note: field(&self.note).filter(|_| mbid.is_some()),
            mbid,
            biography: field(&self.biography),
            header: self.header,
        }
    }
}

/// How long an answer is believed before the internet is asked again: pictures are added,
/// and biographies written, all the time.
pub const ASK_AGAIN_AFTER: Duration = Duration::from_secs(30 * 24 * 3600);

/// A picture of an artist.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PictureKind {
    /// Square, for the grid of artists and beside their name.
    Portrait,
    /// Wide, across the top of their page.
    Backdrop,
}

impl PictureKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Portrait => "portrait",
            Self::Backdrop => "backdrop",
        }
    }
}

/// The source of a picture the user chose themselves.
pub const CHOSEN: &str = "user";

/// The source of a picture a scan found in the artist's folder.
pub const FOLDER: &str = "folder";

/// Which downloaded picture is an artist's, where from, and when it was asked for.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PictureRow {
    /// Its SHA-256 among the downloaded pictures; none when the source had none, or the user
    /// chose none.
    pub picture: Option<String>,
    pub source: String,
    pub asked: SystemTime,
}

impl PictureRow {
    /// Whether the user chose it.
    pub fn chosen(&self) -> bool {
        self.source == CHOSEN
    }

    /// Whether it is recent enough not to ask again.
    pub fn recent(&self) -> bool {
        self.asked.elapsed().is_ok_and(|age| age < ASK_AGAIN_AFTER)
    }

    /// Whether it came from the internet: neither the user's choice nor their folder's.
    pub fn from_internet(&self) -> bool {
        self.source != CHOSEN && self.source != FOLDER
    }
}

/// An answer from the internet, and when it was given.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Asked<T> {
    pub answer: T,
    pub asked: SystemTime,
}

impl<T> Asked<T> {
    /// Whether it is recent enough not to ask again.
    pub fn recent(&self) -> bool {
        self.asked.elapsed().is_ok_and(|age| age < ASK_AGAIN_AFTER)
    }
}

/// What Last.fm says of an artist, in plain text.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ArtistText {
    pub summary: String,
    pub biography: String,
    pub tags: Vec<String>,
    pub url: String,
}

/// Between an artist's name and the MusicBrainz id in the name of an artist split off.
const SPLIT: char = '\u{1f}';

/// The artist an album is filed under once matched to `mbid`, another artist than the one
/// its tags name `name`.
pub fn split_artist(name: &str, mbid: &str) -> String {
    format!("{name}{SPLIT}{mbid}")
}

/// An artist's name as their name in the library has it: without the id of one split off.
pub fn plain_name(artist: &str) -> &str {
    artist.split(SPLIT).next().unwrap_or(artist)
}

/// An album matched to the artist in MusicBrainz who made it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AlbumMatch {
    /// The album as its tags name it.
    pub album_artist: String,
    pub album: String,
    pub release_group: Option<String>,
    pub artist_mbid: String,
    /// The artist it is filed under, when not `album_artist`.
    pub artist_key: Option<String>,
}

/// An artist MusicBrainz knows, with what tells them from others of the same name.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ArtistMatch {
    pub mbid: String,
    pub name: String,
    /// MusicBrainz's words to tell them apart: "Norwegian jazz/metal/rock group".
    pub disambiguation: String,
    /// "Group", "Person".
    pub kind: String,
    /// Where they are from: "Norway".
    pub area: String,
    /// When they began, and ended, as MusicBrainz has it: "1999", "1999-05".
    pub begin: String,
    pub end: String,
}

/// An album MusicBrainz knows (a release group), with the artist who made it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct AlbumCandidate {
    pub release_group: String,
    pub title: String,
    /// "Album", "EP", "Single".
    pub kind: String,
    /// When it first came out: "2010-01-18".
    pub date: String,
    pub artist: ArtistMatch,
}

/// The opening of an artist's article on Wikipedia, in plain text.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Article {
    pub title: String,
    /// Its first paragraph.
    pub summary: String,
    /// The whole opening, before the first heading.
    pub biography: String,
    pub url: String,
}
