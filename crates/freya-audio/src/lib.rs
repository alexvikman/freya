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
mod remote;
mod source;
mod state;
mod stream;

use std::cell::{Cell, RefCell};
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
pub use crate::remote::REQUEST_TIMEOUT as REMOTE_TIMEOUT;
pub use crate::source::{Bearer, Media, MediaTags, RemoteSource, TrackInfo};
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

/// How often a lost audio device is tried to be replaced.
const DEVICE_RETRY: Duration = Duration::from_secs(1);

pub struct Player {
    commands: Sender<Command>,
    interrupt: Interrupt,
    sample_rate: u32,
    visualization: std::sync::Mutex<rtrb::Consumer<f32>>,
    events: Receiver<Event>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    stream: RefCell<Option<cpal::Stream>>,
    /// Where the renderer comes back to from a stream that is dropped, and the renderer
    /// while no stream has it.
    handoff: output::Handoff,
    spare: RefCell<Option<Renderer>>,
    /// The format the engine plays in, which a new device must take.
    format: output::DeviceFormat,
    /// When a lost device was last tried to be replaced.
    last_recovery: Cell<Option<Instant>>,
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
        let (player, renderer) = Self::spawn(format.sample_rate, format.channels);
        *player.stream.borrow_mut() = Some(output.start(renderer, &player.handoff)?);
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
            stream: RefCell::new(None),
            handoff: output::Handoff::new(),
            spare: RefCell::new(None),
            format: output::DeviceFormat {
                sample_rate,
                channels,
            },
            last_recovery: Cell::new(None),
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
        let was = self.shared.paused.swap(paused, Ordering::AcqRel);
        if was && !paused {
            self.send(Command::Resume);
        }
    }

    pub fn toggle_pause(&self) {
        self.wake_device();
        if self.shared.paused.fetch_xor(true, Ordering::AcqRel) {
            self.send(Command::Resume);
        }
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

    /// How long the output has had nothing to play while not paused, all told: the silence
    /// before a track starts, after a seek, between tracks and after the last. For measuring.
    pub fn starved(&self) -> Duration {
        let frames = self
            .shared
            .starved
            .load(std::sync::atomic::Ordering::Relaxed);
        Duration::from_secs_f64(frames as f64 / f64::from(self.sample_rate))
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Stops the audio device after a while with nothing playing, starts it when something
    /// plays, and replaces it when it is gone. Call it regularly, e.g. a few times a second;
    /// commands start the device again by themselves.
    pub fn rest_device_when_idle(&self) {
        self.replace_lost_device();
        if self.status().state == PlaybackState::Playing {
            // A track that took long to open may start after the device was stopped.
            self.wake_device();
            return;
        }
        let since = self.idle_since.get().unwrap_or_else(Instant::now);
        self.idle_since.set(Some(since));
        if since.elapsed() >= DEVICE_IDLE
            && self.stream_running.get()
            && let Some(stream) = &*self.stream.borrow()
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
            && let Some(stream) = &*self.stream.borrow()
        {
            match stream.play() {
                Ok(()) => self.stream_running.set(true),
                Err(e) => log::warn!("could not restart the audio device: {e}"),
            }
        }
    }

    /// Builds the stream again, on the default device, when the device it played on is gone,
    /// as when a USB device is unplugged. Tries once a second until a device takes it.
    fn replace_lost_device(&self) {
        if !self.shared.device_lost.load(Ordering::Acquire)
            || self
                .last_recovery
                .get()
                .is_some_and(|at| at.elapsed() < DEVICE_RETRY)
        {
            return;
        }
        self.last_recovery.set(Some(Instant::now()));
        if let Some(stream) = self.stream.borrow_mut().take() {
            drop(stream);
            self.stream_running.set(false);
            *self.spare.borrow_mut() = self.handoff.take();
        }
        if self.spare.borrow().is_none() {
            log::error!("audio device lost, and its renderer with it");
            self.shared.device_lost.store(false, Ordering::Release);
            return;
        }
        let output = match Output::open_default_in(&self.format) {
            Ok(output) => output,
            Err(e) => {
                log::warn!("no audio device to replace the lost one: {e}");
                return;
            }
        };
        let renderer = self.spare.borrow_mut().take().expect("renderer");
        self.shared.device_lost.store(false, Ordering::Release);
        match output.start(renderer, &self.handoff) {
            Ok(stream) => {
                log::info!("audio device replaced");
                *self.stream.borrow_mut() = Some(stream);
                self.stream_running.set(true);
            }
            Err(e) => {
                log::warn!("the new audio device would not start: {e}");
                *self.spare.borrow_mut() = self.handoff.take();
                self.shared.device_lost.store(true, Ordering::Release);
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
