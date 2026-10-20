//! Freya's server: the library, its pictures, and the scans that keep them in line with the
//! music's folders, for clients to show and play.

mod folder_watch;
pub mod http;
mod lastfm;
mod playlists;
mod serve;
mod service;
pub mod settings;

pub use lastfm::lastfm;
pub use serve::{Served, handler};
pub use service::{LocalLibrary, Service, ServiceConfig};

/// Where a server on this computer listens.
pub const LOCAL_ADDRESS: &str = "127.0.0.1:47474";
