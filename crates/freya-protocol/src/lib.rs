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

/// In the server's folder, which a client on its computer reads: the file the pairing key is
/// written to, and the server's TLS certificate, in DER, which the client pins.
pub const PAIRING_KEY: &str = "pairing.key";
pub const CERTIFICATE_FILE: &str = "tls-certificate.der";

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
