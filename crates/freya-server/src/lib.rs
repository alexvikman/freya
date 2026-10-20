//! Freya's server: the library, its pictures, and the scans that keep them in line with the
//! music's folders, for clients to show and play.

mod folder_watch;
pub mod http;
mod serve;
mod service;

pub use serve::handler;
pub use service::{LocalLibrary, Service, ServiceConfig};
