//! What the library holds: tracks, and the artists, albums, genres and playlists they make up.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// The part of an audio file that one track is: from `start` to `end`, or to the end of the
/// file. Positions are whole milliseconds, wherever they come from, so that the end of one
/// track and the start of the next compare equal.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Part {
    pub start: Duration,
    pub end: Option<Duration>,
}

impl Part {
    /// How long the track is; `file` is the length of the whole file, needed when the track
    /// runs to its end.
    pub fn duration(&self, file: Option<Duration>) -> Option<Duration> {
        self.end.or(file).map(|end| end.saturating_sub(self.start))
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Track {
    pub id: i64,
    pub path: PathBuf,
    /// The part of the file this track is, for a track of a CUE sheet.
    pub part: Option<Part>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<u32>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub duration: Option<Duration>,
}

impl Track {
    /// The title, or the file name when the track has none.
    pub fn display_title(&self) -> String {
        self.title.clone().unwrap_or_else(|| file_stem(&self.path))
    }

    /// The tags as the playlist wants them.
    pub fn tags(&self) -> TagInfo {
        TagInfo {
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            album_artist: self.album_artist.clone(),
            artist_mbid: None,
            genre: None,
            year: self.year,
            track: self.track,
            disc: self.disc,
            duration: self.duration,
        }
    }
}

/// What an artist's page says of them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ArtistSummary {
    pub albums: u32,
    pub tracks: u32,
    pub duration: Duration,
    /// Their most frequent genres, most first.
    pub genres: Vec<String>,
}

/// How artists are listed.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtistOrder {
    Name,
    /// Most played first, counting every play of every one of their tracks.
    MostPlayed,
    /// The one whose music came last first.
    RecentlyAdded,
}

/// An artist the library has albums by. `name` is `None` for untagged tracks.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Artist {
    /// As the tags name them; how they are known.
    pub name: Option<String>,
    /// The name the user gave them in Freya, when they did.
    pub shown_name: Option<String>,
    pub albums: u32,
    pub tracks: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Album {
    pub artist: Option<String>,
    pub name: Option<String>,
    pub year: Option<u32>,
    pub tracks: u32,
    /// One of the album's tracks, to find its cover from.
    pub track: PathBuf,
}

/// A genre the library has tracks in. `name` is `None` for tracks without one.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Genre {
    pub name: Option<String>,
    pub albums: u32,
    pub tracks: u32,
}

/// A track that has been played.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Played {
    pub track: Track,
    /// How many times.
    pub plays: u32,
    /// When it was last played.
    pub last: SystemTime,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub tracks: u32,
    pub albums: u32,
    pub artists: u32,
}

/// How the albums are ordered.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlbumOrder {
    /// By when they came into the library, or were made favourites, the latest first.
    #[default]
    Added,
    /// By when they were last played, the latest first; those never played last.
    Played,
    /// By title.
    Name,
    /// By artist, then year.
    Artist,
}

/// An album as its favourite is kept: its album artist and title.
pub type AlbumKey = (Option<String>, Option<String>);

/// An album's cover as the last scan made it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AlbumCover {
    pub cover: Option<String>,
    pub color: Option<[u8; 3]>,
    pub track: String,
    pub images: i64,
}

/// What a playlist has beside its tracks; each `None` when the user gave none.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaylistInfo {
    pub description: Option<String>,
    pub picture: Option<String>,
    pub backdrop: Option<String>,
}

/// A track in a playlist: its file, and where in it it starts, in milliseconds.
pub type PlaylistTrack = (PathBuf, i64);

/// What the user gave a genre; each `None` when they gave none.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct GenreInfo {
    pub picture: Option<String>,
    pub backdrop: Option<String>,
    pub description: Option<String>,
}

/// What the internet says of a genre.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct GenreArticle {
    /// Its first paragraph, and the whole.
    pub summary: String,
    pub description: String,
    pub url: String,
    /// "Wikipedia" or "Last.fm".
    pub source: String,
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}
