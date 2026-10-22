//! Freya's server: the library, its pictures, and the scans that keep them in line with the
//! music's folders, for clients to show and play.

pub mod accounts;
pub mod autostart;
pub mod discovery;
mod folder_watch;
mod folders;
pub mod http;
mod lastfm;
#[cfg(target_os = "macos")]
pub mod menu_bar;
mod pages;
mod playlists;
mod scrobbler;
mod serve;
mod service;
pub mod settings;
pub mod take_in;
pub mod tls;

pub use lastfm::lastfm;
pub use serve::{Served, found, handler};
pub use service::{LocalLibrary, Service, ServiceConfig};

/// Where a server on this computer listens.
pub const LOCAL_ADDRESS: &str = "127.0.0.1:47474";

/// The file in the server's folder that the pairing key is written to (see [`accounts`]).
pub const PAIRING_KEY: &str = "pairing.key";
