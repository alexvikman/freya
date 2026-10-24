//! Playlists, playlist files, tag reading, the media library, its album covers and the
//! pictures of artists downloaded for it.

pub mod artist_image;
pub mod cover;
pub mod cue;
pub mod eq_presets;
pub mod files;
/// The media library itself, in SQLite, and its scans: the server's. A client goes without
/// (`default-features = false`).
#[cfg(feature = "db")]
pub mod library;
pub mod pictures;
pub mod playlist;
pub mod playlist_file;
pub mod search;
pub mod tags;
pub mod thumbnails;
pub mod wikipedia;

#[cfg(feature = "db")]
pub use library::{Library, ScanEvent, Scanner};
pub use playlist::{Entry, Playlist, SelectMode, SortKey};
pub use tags::{TagInfo, TagLoader, read_tags};

/// A picture in `bytes`, decoded within bounds: no wider or higher than 12,000 pixels, nor
/// needing more than 256 MB. Pictures come from files others may have written, from servers and
/// from the internet: one small as a file and huge decoded cannot take the memory.
pub fn decode_picture(bytes: &[u8]) -> image::ImageResult<image::DynamicImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    reader.decode()
}
