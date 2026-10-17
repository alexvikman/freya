//! Playlists, playlist files, tag reading, the media library and its album covers.

pub mod cover;
pub mod cue;
pub mod eq_presets;
pub mod files;
pub mod library;
pub mod playlist;
pub mod playlist_file;
pub mod tags;
pub mod thumbnails;

pub use library::{Library, ScanEvent, Scanner};
pub use playlist::{Entry, Playlist, SelectMode, SortKey};
pub use tags::{TagInfo, TagLoader, read_tags};
