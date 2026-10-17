//! The system's media controls: media keys, macOS "Now Playing", Windows' media overlay and
//! Linux's MPRIS. They show what plays and send play, pause, next and so on back to Freya.
//! Each system has its own small `Controls` in a module of its own; this file decides what to
//! tell them and when.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use freya_audio::{PlaybackState, Status, TrackInfo};

#[cfg(target_os = "linux")]
mod mpris;
#[cfg(target_os = "macos")]
mod now_playing;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod other;
#[cfg(windows)]
mod smtc;

#[cfg(target_os = "linux")]
use mpris as platform;
#[cfg(target_os = "macos")]
use now_playing as platform;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
use other as platform;
#[cfg(windows)]
use smtc as platform;

/// The system is told the position again when it is this far from where it would expect.
/// It moves the position along by itself while playing.
const POSITION_DRIFT: Duration = Duration::from_millis(1500);

/// What the system asks Freya to do. Each system sends only some of these.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "only Linux's MPRIS sends every one")
)]
pub enum MediaRequest {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Stop,
    /// Moves the position by this many seconds, backwards when negative.
    SeekBy(f64),
    SetPosition(Duration),
    /// From 0.0 to 1.0.
    SetVolume(f64),
    /// A file (`file://`) or a stream address to play.
    OpenUri(String),
    /// Brings Freya's window to the front.
    Raise,
    Quit,
}

/// What plays, as the system shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    /// The album, or a stream's station.
    pub album: Option<String>,
    pub duration: Option<Duration>,
    /// An image file of the cover.
    pub cover: Option<PathBuf>,
}

pub struct SystemMedia {
    controls: platform::Controls,
    requests: Receiver<MediaRequest>,
    /// What the system was last told.
    metadata: Metadata,
    state: Option<PlaybackState>,
    volume: Option<f32>,
    /// The position last reported, and when.
    position: Option<(Duration, Instant)>,
}

impl SystemMedia {
    /// Connects to the system's media controls for `window`. `None` where there are none,
    /// e.g. without a D-Bus session on Linux.
    pub fn new(window: &slint::Window) -> Option<Self> {
        let (sender, requests) = channel();
        let controls = platform::Controls::new(window, sender)?;
        Some(Self {
            controls,
            requests,
            metadata: Metadata::default(),
            state: None,
            volume: None,
            position: None,
        })
    }

    /// Requests from the system since the last call: media keys, the Now Playing widget.
    pub fn poll(&self) -> impl Iterator<Item = MediaRequest> + '_ {
        self.requests.try_iter()
    }

    /// Tells the system what plays, when it changed. `cover` is an image file of the cover;
    /// `volume` goes from 0.0 to 1.0.
    pub fn update(
        &mut self,
        status: &Status,
        track: Option<&TrackInfo>,
        cover: Option<&Path>,
        volume: f32,
    ) {
        let playing = status.state != PlaybackState::Stopped;
        let track = track.filter(|_| playing);
        let metadata = Metadata {
            title: track.map(|t| t.title.clone().unwrap_or_else(|| t.display_title())),
            artist: track.and_then(|t| t.artist.clone()),
            album: track.and_then(|t| t.album.clone().or_else(|| t.station.clone())),
            duration: track.and_then(|t| t.duration),
            cover: cover.filter(|_| playing).map(Path::to_owned),
        };

        if metadata != self.metadata {
            log::debug!(
                "system media: {:?} by {:?}",
                metadata.title,
                metadata.artist
            );
            self.controls.set_metadata(&metadata);
            self.metadata = metadata;
            // A new track starts its own position.
            self.position = None;
        }

        let drifted = match self.position {
            None => true,
            Some((at, when)) => {
                let expected = if status.state == PlaybackState::Playing {
                    at + when.elapsed()
                } else {
                    at
                };
                expected.abs_diff(status.position) > POSITION_DRIFT
            }
        };
        if Some(status.state) != self.state || (playing && drifted) {
            log::debug!("system media: {:?} at {:?}", status.state, status.position);
            self.controls.set_playback(status.state, status.position);
            self.state = Some(status.state);
            self.position = Some((status.position, Instant::now()));
        }

        if Some(volume) != self.volume {
            self.controls.set_volume(volume);
            self.volume = Some(volume);
        }
    }
}

/// Sends a request to Freya; it is gone when Freya has closed the controls.
fn send(requests: &Sender<MediaRequest>, request: MediaRequest) {
    let _ = requests.send(request);
}
