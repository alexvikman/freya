//! What Freya's client and server say to each other: the library's tracks, artists, albums,
//! genres and playlists, what is known of them, and how scans go. Plain data, the same on both
//! sides.

pub mod api;
pub mod artists;
pub mod lastfm;
pub mod library;
pub mod scan;

pub use api::*;
pub use artists::*;
pub use lastfm::*;
pub use library::*;
pub use scan::*;

/// Where a server on this computer listens.
pub const LOCAL_ADDRESS: &str = "127.0.0.1:47474";

/// In the server's folder, which Freya on its computer reads: the server's TLS certificate,
/// in DER, which Freya pins.
pub const CERTIFICATE_FILE: &str = "tls-certificate.der";
/// In the server's folder while it is not adopted: the code it is adopted with, and until when
/// it lasts in seconds since 1970, a line each. Freya on the server's computer reads it, so its
/// user types no code there.
pub const ADOPTION_CODE: &str = "adoption-code";

/// The SHA-256 of a certificate in DER, in groups of four hex digits: shown by both sides, to
/// compare.
pub fn fingerprint(certificate: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, certificate);
    let hex: Vec<String> = digest
        .as_ref()
        .chunks(2)
        .map(|pair| pair.iter().map(|b| format!("{b:02X}")).collect())
        .collect();
    hex.join(" ")
}
