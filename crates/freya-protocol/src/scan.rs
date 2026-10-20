//! How a scan of the music's folders goes.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ScanEvent {
    /// `done` of `total` new or changed files (audio files or CUE sheets) have been read and
    /// written.
    Progress {
        done: usize,
        total: usize,
    },
    /// The folder being looked through or read now, told every so often.
    Looking(PathBuf),
    /// `done` of `total` albums' covers have been made.
    Covers {
        done: usize,
        total: usize,
    },
    /// The internet was asked what `done` of the `total` artists who lack something lack.
    Artists {
        done: usize,
        total: usize,
    },
    /// And of the `total` genres it has not said anything of.
    Genres {
        done: usize,
        total: usize,
    },
    Finished(ScanSummary),
    Failed(String),
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanSummary {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
}

/// What a server tells its clients as it happens.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ServiceEvent {
    /// A scan began, or asking the internet without one.
    ScanStarted,
    /// How it goes.
    Scan(ScanEvent),
    /// It ended, done or not; what it wrote is in the library.
    ScanEnded,
    /// Plays noted while a scan wrote are in the library now.
    PlaysWritten,
}
