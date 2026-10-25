//! Freya's server: the library, its pictures, and the scans that keep them in line with the
//! music's folders, for clients to show and play.

pub mod accounts;
mod answers;
pub mod autostart;
pub mod discovery;
mod folder_watch;
mod folders;
#[cfg(test)]
mod hostile;
pub use freya_http as http;
mod lastfm;
#[cfg(any(target_os = "macos", windows))]
pub mod menu;
mod pages;
mod playlists;
mod radio;
mod scrobbler;
mod serve;
mod service;
pub mod settings;
mod stations;
#[cfg(windows)]
pub mod stop;
pub mod tls;
pub mod updates;

pub use lastfm::lastfm;
pub use serve::{Served, Updating, admit, found, handler};
pub use service::{LocalLibrary, Service, ServiceConfig};

pub use freya_protocol::LOCAL_ADDRESS;
