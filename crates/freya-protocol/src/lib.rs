//! What Freya's client and server say to each other: the library's tracks, artists, albums,
//! genres and playlists, what is known of them, and how scans go. Plain data, the same on both
//! sides.

pub mod artists;
pub mod library;
pub mod scan;

pub use artists::*;
pub use library::*;
pub use scan::*;
