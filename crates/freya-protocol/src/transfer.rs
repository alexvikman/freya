//! What the users made with a library, exported to be taken into another: their plays,
//! favourites, playlists, stations and what they changed of artists, albums and genres. Tracks
//! are named so another library finds its own of them: by their path, by their path within the
//! music folder they are in, and by their tags.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::RadioStation;

/// The version of [`Export`] written; one from a later Freya is refused.
pub const EXPORT_VERSION: u32 = 1;

/// A track, as an export names it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct TrackRef {
    /// Its file, as the library that exported it has it.
    pub path: PathBuf,
    /// Its file within the music folder it is in, "Artist/Album/01.flac"; none outside them.
    pub within: Option<String>,
    /// Where it starts in its file, for a track of a CUE sheet; none for a whole file.
    pub start_ms: Option<i64>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
}

/// A play of a track.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PlayRow {
    pub user: i64,
    pub track: TrackRef,
    pub at: SystemTime,
    pub playlist: Option<String>,
}

/// A radio station listened to.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StationPlayRow {
    pub user: i64,
    pub name: String,
    pub url: String,
    pub at: SystemTime,
}

/// A favourite: an album (by its album artist and title), an artist, or a playlist.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FavoriteRow {
    pub user: i64,
    /// "album", "artist" or "playlist".
    pub kind: String,
    /// The album's artist, the artist, or the playlist; "" for an album without one.
    pub name: String,
    /// The album's title; "" for the others.
    pub album: String,
    pub at: SystemTime,
}

/// What a playlist says of itself, its pictures by their SHA-256 among the export's.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PlaylistInfoRow {
    pub user: i64,
    pub playlist: String,
    pub description: Option<String>,
    pub picture: Option<String>,
    pub backdrop: Option<String>,
}

/// When a track was added to a playlist.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PlaylistAddedRow {
    pub user: i64,
    pub playlist: String,
    pub track: TrackRef,
    pub added: SystemTime,
}

/// What was changed of an artist.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ArtistEditRow {
    pub artist: String,
    pub name: Option<String>,
    pub sort_name: Option<String>,
    pub mbid: Option<String>,
    pub biography: Option<String>,
    pub note: Option<String>,
    pub header: Option<String>,
}

/// An album matched to MusicBrainz by the user.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AlbumMatchRow {
    pub album_artist: String,
    pub album: String,
    pub release_group: Option<String>,
    pub artist_mbid: String,
    pub artist_key: Option<String>,
}

/// What was given of a genre, its pictures by their SHA-256 among the export's.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GenreInfoRow {
    pub genre: String,
    pub picture: Option<String>,
    pub backdrop: Option<String>,
    pub description: Option<String>,
}

/// A picture the user chose for an artist, by its SHA-256 among the export's.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ArtistPictureRow {
    pub artist: String,
    /// "portrait" or "backdrop".
    pub kind: String,
    pub picture: String,
}

/// What the library's database holds that the users made.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Made {
    pub plays: Vec<PlayRow>,
    pub station_plays: Vec<StationPlayRow>,
    pub favorites: Vec<FavoriteRow>,
    pub playlist_info: Vec<PlaylistInfoRow>,
    pub playlist_added: Vec<PlaylistAddedRow>,
    pub artist_edits: Vec<ArtistEditRow>,
    pub album_matches: Vec<AlbumMatchRow>,
    pub genre_info: Vec<GenreInfoRow>,
    pub artist_pictures: Vec<ArtistPictureRow>,
}

impl Made {
    /// The pictures it names, by their SHA-256.
    pub fn pictures(&self) -> Vec<String> {
        let mut pictures: Vec<String> = self
            .playlist_info
            .iter()
            .flat_map(|row| [row.picture.clone(), row.backdrop.clone()])
            .chain(
                self.genre_info
                    .iter()
                    .flat_map(|row| [row.picture.clone(), row.backdrop.clone()]),
            )
            .flatten()
            .chain(self.artist_pictures.iter().map(|row| row.picture.clone()))
            .collect();
        pictures.sort();
        pictures.dedup();
        pictures
    }
}

/// A playlist, its tracks as an export names them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ExportedPlaylist {
    pub name: String,
    pub created: Option<SystemTime>,
    pub tracks: Vec<ExportedEntry>,
}

/// A track of a playlist, with what the playlist shows of it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ExportedEntry {
    pub track: TrackRef,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
}

/// A user of the server exported, by the id their rows have.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ExportedUser {
    pub id: i64,
    pub name: String,
}

/// An export, as its file's `export.json` holds it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Export {
    pub version: u32,
    pub exported: SystemTime,
    /// The music folders of the library it was exported from.
    pub folders: Vec<PathBuf>,
    pub users: Vec<ExportedUser>,
    pub made: Made,
    pub playlists: Vec<ExportedPlaylist>,
    pub stations: Vec<RadioStation>,
}

/// What an import took in, and what it found no track for.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub plays: usize,
    pub favorites: usize,
    pub playlists: usize,
    pub stations: usize,
    /// Changes to artists, albums and genres.
    pub edits: usize,
    /// Tracks found nowhere in this library, as "Artist – Title", at most a hundred, and how
    /// many in all.
    pub missing: Vec<String>,
    pub missing_count: usize,
    /// Users of the export this server has none of: their rows were left out.
    pub users_left_out: Vec<String>,
}
