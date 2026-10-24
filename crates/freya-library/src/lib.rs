//! Freya Server's media library: the music's tracks, albums, artists, plays, favourites and
//! playlists in SQLite, the scans that keep it in line with the music's folders, and what is
//! fetched online for it. Freya, the player, has none of it: it asks the server.

pub mod library;
pub mod wikipedia;

// What the library is made from, by the paths its modules know them by.
pub(crate) use freya_media::{artist_image, cover, cue, files, pictures, tags, thumbnails};

pub use library::{Library, ScanEvent, Scanner};
