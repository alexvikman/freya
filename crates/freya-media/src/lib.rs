//! Music files and what is in and around them, for Freya and Freya Server alike: finding
//! playable files, their tags and CUE sheets, playlist entries and files, covers, and pictures
//! of artists, found online and kept with their thumbnails.

pub mod artist_image;
pub mod cover;
pub mod cue;
pub mod entry;
pub mod files;
pub mod pictures;
pub mod playlist_file;
pub mod tags;
pub mod thumbnails;

pub use entry::Entry;
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
