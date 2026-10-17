//! The Freya audio engine.
//!
//! [`Player`] decodes files on a background thread and plays them through the default output
//! device. Commands are non-blocking; progress is read with [`Player::status`] and
//! notifications arrive on [`Player::events`].

pub mod analysis;
mod ape;
mod convert;
mod engine;
mod eq;
mod null_output;
mod output;
mod source;
mod state;
mod stream;

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::StreamTrait;
use crossbeam_channel::{Receiver, Sender};

use crate::engine::{Command, Engine};
use crate::output::{Output, Renderer};
use crate::state::Shared;
use crate::stream::Interrupt;

pub use crate::engine::Event;
pub use crate::eq::{BAND_FREQUENCIES, EqSettings, MAX_GAIN_DB};
pub use crate::source::{Media, MediaTags, TrackInfo};
pub use crate::state::{PlaybackState, Status};
pub use crate::stream::{is_url, split_title};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot open {path}: {1}", path = .0.display())]
    Open(PathBuf, String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("seek error: {0}")]
    Seek(String),
    #[error("audio output error: {0}")]
    Output(String),
    /// Waiting for a stream was cut short to do something else. Not a failure.
    #[error("interrupted")]
    Interrupted,
}

/// Length of the ring buffer between decoder and output.
///
/// Pause, volume and seeking act immediately regardless; this only bounds how far the decoder
/// runs ahead.
const BUFFER_SECONDS: f64 = 0.25;

/// The audio device is stopped after this long with nothing playing, so it and the system's
/// audio service can rest. Long enough that pausing for a moment does not stop it.
const DEVICE_IDLE: Duration = Duration::from_secs(5);

pub struct Player {
    commands: Sender<Command>,
    interrupt: Interrupt,
    sample_rate: u32,
    visualization: std::sync::Mutex<rtrb::Consumer<f32>>,
    events: Receiver<Event>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    stream: Option<cpal::Stream>,
    /// Whether the device is running, and since when nothing has been playing.
    stream_running: Cell<bool>,
    idle_since: Cell<Option<Instant>>,
    _null_output: Option<null_output::NullOutput>,
}

impl Player {
    /// Creates a player on the system's default output device.
    pub fn new() -> Result<Self, Error> {
        let output = Output::open_default()?;
        let format = output.format();
        let (mut player, renderer) = Self::spawn(format.sample_rate, format.channels);
        player.stream = Some(output.start(renderer)?);
        player.stream_running.set(true);
        Ok(player)
    }

    /// A player that plays into nothing, at real-time speed. For tests and machines without
    /// audio output.
    pub fn silent(sample_rate: u32) -> Self {
        let (mut player, renderer) = Self::spawn(sample_rate, 2);
        player._null_output = Some(null_output::NullOutput::start(
            renderer,
            2,
            Some(sample_rate),
        ));
        player
    }

    /// Starts the engine without an audio device; the caller drives the returned renderer.
    fn spawn(sample_rate: u32, channels: usize) -> (Self, Renderer) {
        let capacity = (f64::from(sample_rate) * BUFFER_SECONDS) as usize * channels;
        let (producer, consumer) = rtrb::RingBuffer::new(capacity);
        // A quarter of a second of mono samples; the UI reads them many times a second.
        let (vis_producer, vis_consumer) =
            rtrb::RingBuffer::new((f64::from(sample_rate) * BUFFER_SECONDS) as usize);
        let shared = Arc::new(Shared::new(sample_rate));
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let interrupt = Interrupt::default();

        let engine = Engine::new(
            Arc::clone(&shared),
            interrupt.clone(),
            producer,
            cmd_rx,
            ev_tx,
            sample_rate,
            channels,
        );
        let thread = std::thread::Builder::new()
            .name("freya-decoder".into())
            .spawn(move || engine.run())
            .expect("spawn decoder thread");

        let renderer = Renderer::new(
            consumer,
            Arc::clone(&shared),
            sample_rate,
            channels,
            vis_producer,
        );
        let player = Self {
            commands: cmd_tx,
            interrupt,
            sample_rate,
            visualization: std::sync::Mutex::new(vis_consumer),
            events: ev_rx,
            shared,
            thread: Some(thread),
            stream: None,
            stream_running: Cell::new(false),
            idle_since: Cell::new(None),
            _null_output: None,
        };
        (player, renderer)
    }

    /// Stops whatever is playing and starts `media`: a file, or a part of one, or an
    /// `http://` or `https://` URL of an internet radio stream.
    pub fn play(&self, media: impl Into<Media>) {
        self.send(Command::Play(media.into()));
    }

    /// Sets the track to continue with, gaplessly, when the current one ends.
    pub fn set_next(&self, media: Option<Media>) {
        self.send(Command::SetNext(media));
    }

    pub fn seek(&self, position: Duration) {
        self.send(Command::Seek(position));
    }

    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    pub fn set_paused(&self, paused: bool) {
        if !paused {
            self.wake_device();
        }
        self.shared.paused.store(paused, Ordering::Relaxed);
    }

    pub fn toggle_pause(&self) {
        self.wake_device();
        self.shared.paused.fetch_xor(true, Ordering::Relaxed);
    }

    /// Volume from 0.0 to 1.0.
    pub fn volume(&self) -> f32 {
        self.shared.volume()
    }

    pub fn set_volume(&self, volume: f32) {
        self.shared.set_volume(volume);
    }

    /// Balance from -1.0 (left) to 1.0 (right).
    pub fn balance(&self) -> f32 {
        self.shared.balance()
    }

    pub fn set_balance(&self, balance: f32) {
        self.shared.set_balance(balance);
    }

    pub fn equalizer(&self) -> EqSettings {
        self.shared.eq.get()
    }

    /// Takes effect immediately, in the audio callback.
    pub fn set_equalizer(&self, settings: &EqSettings) {
        self.shared.eq.set(settings);
    }

    /// Turns copying of played samples for visualization on or off. Off costs nothing in the
    /// audio callback.
    pub fn set_visualization(&self, on: bool) {
        self.shared.visualize.store(on, Ordering::Relaxed);
        if !on {
            let _ = self.visualization_samples(&mut Vec::new());
        }
    }

    /// Appends the samples played since the last call, mixed to mono, to `out`. Returns how
    /// many were added.
    pub fn visualization_samples(&self, out: &mut Vec<f32>) -> usize {
        let Ok(mut consumer) = self.visualization.lock() else {
            return 0;
        };
        let n = consumer.slots();
        let Ok(chunk) = consumer.read_chunk(n) else {
            return 0;
        };
        let (a, b) = chunk.as_slices();
        out.extend_from_slice(a);
        out.extend_from_slice(b);
        chunk.commit_all();
        n
    }

    /// The sample rate of the output, which visualization samples are in.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn status(&self) -> Status {
        self.shared.status()
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Stops the audio device after a while with nothing playing, and notes when playing
    /// starts again. Call it regularly, e.g. a few times a second; commands start the device
    /// again by themselves.
    pub fn rest_device_when_idle(&self) {
        if self.status().state == PlaybackState::Playing {
            self.idle_since.set(None);
            return;
        }
        let since = self.idle_since.get().unwrap_or_else(Instant::now);
        self.idle_since.set(Some(since));
        if since.elapsed() >= DEVICE_IDLE
            && self.stream_running.get()
            && let Some(stream) = &self.stream
        {
            match stream.pause() {
                Ok(()) => {
                    self.stream_running.set(false);
                    log::debug!("audio device stopped while idle");
                }
                // Some devices cannot pause; they just keep playing silence.
                Err(e) => log::debug!("audio device cannot pause: {e}"),
            }
        }
    }

    /// Starts the audio device if it was stopped while idle. The engine needs it running:
    /// playing, seeking and stopping wait for the device to drop what it buffered.
    fn wake_device(&self) {
        self.idle_since.set(None);
        if !self.stream_running.get()
            && let Some(stream) = &self.stream
        {
            match stream.play() {
                Ok(()) => self.stream_running.set(true),
                Err(e) => log::warn!("could not restart the audio device: {e}"),
            }
        }
    }

    fn send(&self, cmd: Command) {
        // These wait for the device to drop what it buffered, so it must be running.
        if matches!(cmd, Command::Play(_) | Command::Seek(_) | Command::Stop) {
            self.wake_device();
        }
        // These must not wait for a stream that is slow to connect or stalled.
        if matches!(cmd, Command::Play(_) | Command::Stop | Command::Quit) {
            self.interrupt.raise();
        }
        // The engine only exits when the player is dropped.
        let _ = self.commands.send(cmd);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.send(Command::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests;
