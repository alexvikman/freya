//! Linux: MPRIS, the D-Bus interface through which desktops, media-key daemons and tools such
//! as playerctl show and control players. zbus serves the two interfaces on a thread of its
//! own; they answer from the state Freya last reported.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use freya_audio::PlaybackState;
use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::interface;
use zbus::names::BusName;
use zbus::zvariant::{ObjectPath, Value};

use super::{MediaRequest, Metadata, send};

const PATH: &str = "/org/mpris/MediaPlayer2";
const NAME: &str = "org.mpris.MediaPlayer2.freya";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

pub struct Controls {
    connection: Connection,
    state: Shared,
}

/// What Freya last reported, read by the interfaces when a client asks.
#[derive(Clone)]
struct Shared(Arc<Mutex<State>>);

struct State {
    metadata: Metadata,
    /// Counts tracks, so a client's request to seek in an earlier one can be told apart.
    track: u64,
    playback: PlaybackState,
    /// The position at `since`; it moves on from there while playing.
    position: Duration,
    since: Instant,
    volume: f64,
}

impl Controls {
    pub fn new(_window: &slint::Window, requests: Sender<MediaRequest>) -> Option<Self> {
        if !session_bus_available() {
            log::info!("no D-Bus session: no system media controls");
            return None;
        }
        Self::connect(requests)
            .inspect_err(|e| log::warn!("no system media controls: {e}"))
            .ok()
    }

    fn connect(requests: Sender<MediaRequest>) -> zbus::Result<Self> {
        let state = Shared(Arc::new(Mutex::new(State {
            metadata: Metadata::default(),
            track: 0,
            playback: PlaybackState::Stopped,
            position: Duration::ZERO,
            since: Instant::now(),
            volume: 1.0,
        })));
        let root = Root {
            requests: requests.clone(),
        };
        let player = Player {
            requests,
            state: state.clone(),
        };
        let connection = Builder::session()?
            .serve_at(PATH, root)?
            .serve_at(PATH, player)?
            .build()?;
        // MPRIS asks a second instance to add its process id to the name.
        let reply =
            connection.request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into())?;
        if reply != RequestNameReply::PrimaryOwner {
            let name = format!("{NAME}.instance{}", std::process::id());
            connection.request_name(name)?;
        }
        Ok(Self { connection, state })
    }

    pub fn set_metadata(&mut self, metadata: &Metadata) {
        let metadata = {
            let mut state = self.state.lock();
            state.metadata = metadata.clone();
            state.track += 1;
            state.metadata_value()
        };
        self.changed(&[("Metadata", metadata)]);
    }

    pub fn set_playback(&mut self, playback: PlaybackState, position: Duration) {
        let previous = {
            let mut state = self.state.lock();
            state.position = position;
            state.since = Instant::now();
            std::mem::replace(&mut state.playback, playback)
        };
        if playback != previous {
            self.changed(&[("PlaybackStatus", Value::from(status(playback)))]);
        } else {
            // Clients move the position along by themselves; a jump needs telling.
            let seeked = self.connection.emit_signal(
                None::<BusName>,
                PATH,
                PLAYER,
                "Seeked",
                &(micros(position),),
            );
            if let Err(e) = seeked {
                log::debug!("system media controls: {e}");
            }
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        let volume = f64::from(volume);
        self.state.lock().volume = volume;
        self.changed(&[("Volume", Value::from(volume))]);
    }

    /// Tells clients that properties of the player changed, with their new values.
    fn changed(&self, properties: &[(&str, Value)]) {
        let changed: HashMap<&str, &Value> = properties.iter().map(|(k, v)| (*k, v)).collect();
        let invalidated: &[&str] = &[];
        let sent = self.connection.emit_signal(
            None::<BusName>,
            PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(PLAYER, changed, invalidated),
        );
        if let Err(e) = sent {
            log::debug!("system media controls: {e}");
        }
    }
}

/// `org.mpris.MediaPlayer2`: the player as an application.
struct Root {
    requests: Sender<MediaRequest>,
}

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {
        send(&self.requests, MediaRequest::Raise);
    }

    fn quit(&self) {
        send(&self.requests, MediaRequest::Quit);
    }

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> String {
        "Freya".to_owned()
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        ["file", "http", "https"].map(String::from).to_vec()
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        [
            "audio/mpeg",
            "audio/flac",
            "audio/ogg",
            "audio/opus",
            "audio/aac",
            "audio/mp4",
            "audio/wav",
        ]
        .map(String::from)
        .to_vec()
    }
}

/// `org.mpris.MediaPlayer2.Player`: what plays, and the buttons.
struct Player {
    requests: Sender<MediaRequest>,
    state: Shared,
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn play(&self) {
        send(&self.requests, MediaRequest::Play);
    }

    fn pause(&self) {
        send(&self.requests, MediaRequest::Pause);
    }

    fn play_pause(&self) {
        send(&self.requests, MediaRequest::Toggle);
    }

    fn stop(&self) {
        send(&self.requests, MediaRequest::Stop);
    }

    fn next(&self) {
        send(&self.requests, MediaRequest::Next);
    }

    fn previous(&self) {
        send(&self.requests, MediaRequest::Previous);
    }

    /// Moves the position by `offset` microseconds.
    fn seek(&self, offset: i64) {
        send(&self.requests, MediaRequest::SeekBy(offset as f64 / 1e6));
    }

    /// Moves to `position` microseconds into `track`, if that is still what plays.
    fn set_position(&self, track: ObjectPath<'_>, position: i64) {
        let current = self.state.lock().track_id();
        if track == current && position >= 0 {
            let position = Duration::from_micros(position as u64);
            send(&self.requests, MediaRequest::SetPosition(position));
        }
    }

    fn open_uri(&self, uri: String) {
        send(&self.requests, MediaRequest::OpenUri(uri));
    }

    #[zbus(property)]
    fn playback_status(&self) -> String {
        status(self.state.lock().playback).to_owned()
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<&'static str, Value<'static>> {
        self.state.lock().metadata_entries()
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.state.lock().volume
    }

    #[zbus(property)]
    fn set_volume(&self, volume: f64) {
        send(&self.requests, MediaRequest::SetVolume(volume));
    }

    /// Clients ask for the position when they need it; it is not announced as it moves.
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        let state = self.state.lock();
        let moved = match state.playback {
            PlaybackState::Playing => state.since.elapsed(),
            _ => Duration::ZERO,
        };
        micros(state.position + moved)
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

impl Shared {
    /// The state, also after a panic on another thread: each field is valid on its own.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl State {
    /// MPRIS names each track with an object path; this one when nothing plays.
    fn track_id(&self) -> ObjectPath<'static> {
        if self.metadata.title.is_none() {
            return ObjectPath::from_static_str_unchecked(
                "/org/mpris/MediaPlayer2/TrackList/NoTrack",
            );
        }
        ObjectPath::from_string_unchecked(format!("/org/freya/Track/{}", self.track))
    }

    /// The MPRIS metadata map: `mpris:` and `xesam:` keys.
    fn metadata_entries(&self) -> HashMap<&'static str, Value<'static>> {
        let metadata = &self.metadata;
        let mut entries = HashMap::from([("mpris:trackid", Value::from(self.track_id()))]);
        if let Some(title) = &metadata.title {
            entries.insert("xesam:title", Value::from(title.clone()));
        }
        if let Some(artist) = &metadata.artist {
            entries.insert("xesam:artist", Value::from(vec![artist.clone()]));
        }
        if let Some(album) = &metadata.album {
            entries.insert("xesam:album", Value::from(album.clone()));
        }
        if let Some(duration) = metadata.duration {
            entries.insert("mpris:length", Value::from(micros(duration)));
        }
        if let Some(cover) = &metadata.cover {
            entries.insert("mpris:artUrl", Value::from(file_url(cover)));
        }
        entries
    }

    fn metadata_value(&self) -> Value<'static> {
        Value::from(self.metadata_entries())
    }
}

fn status(playback: PlaybackState) -> &'static str {
    match playback {
        PlaybackState::Stopped => "Stopped",
        PlaybackState::Paused => "Paused",
        PlaybackState::Playing => "Playing",
    }
}

/// MPRIS counts time in microseconds.
fn micros(duration: Duration) -> i64 {
    i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// MPRIS talks over the D-Bus session bus. Without one, as in a container, there is nothing
/// to connect to, which is no failure.
fn session_bus_available() -> bool {
    std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
        || std::env::var_os("XDG_RUNTIME_DIR")
            .is_some_and(|dir| Path::new(&dir).join("bus").exists())
}
