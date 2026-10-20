//! Last.fm's web service, as Freya uses it: what it says of artists and genres, and signed calls
//! for signing in, "now playing" and scrobbles. <https://www.last.fm/api>

mod api;
mod md5;

pub use api::*;
