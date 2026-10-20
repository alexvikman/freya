//! Freya's server: the library, its pictures, and the scans that keep them in line with the
//! music's folders, for clients to show and play.

mod folder_watch;
mod service;

pub use service::{Service, ServiceConfig};
