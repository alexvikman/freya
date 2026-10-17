//! The decoder thread: opens files, decodes, converts to the device format and fills the ring
//! buffer.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use rtrb::Producer;

use crate::convert::{RateConverter, map_channels};
use crate::source::Source;
use crate::state::Shared;
use crate::stream::Interrupt;
use crate::{Error, Media, TrackInfo};

/// How often the thread wakes up while waiting on a full ring buffer or for playback to end.
const POLL: Duration = Duration::from_millis(10);
/// How long to wait for the audio callback to acknowledge a flush.
const FLUSH_TIMEOUT: Duration = Duration::from_millis(500);

pub(crate) enum Command {
    Play(Media),
    SetNext(Option<Media>),
    Seek(Duration),
    Stop,
    Quit,
}

/// Notifications from the engine.
#[derive(Clone, Debug)]
pub enum Event {
    /// A track became audible.
    TrackStarted(Arc<TrackInfo>),
    /// The last track finished and nothing was queued.
    Finished,
    /// A track could not be opened or decoded. Playback continues with the next one, if any.
    Error(String),
}

struct Current {
    source: Source,
    serial: u64,
    info: Arc<TrackInfo>,
}

enum Wake {
    Command(Command),
    Timeout,
    Disconnected,
}

pub(crate) struct Engine {
    shared: Arc<Shared>,
    /// Raised by the player for commands that should not wait for a slow stream.
    interrupt: Interrupt,
    producer: Producer<f32>,
    commands: Receiver<Command>,
    events: Sender<Event>,
    rate: u32,
    channels: usize,

    current: Option<Current>,
    next: Option<Media>,
    /// Resampler and the input rate it was built for.
    converter: Option<(u32, RateConverter)>,
    mapped: Vec<f32>,
    /// Converted audio waiting for room in the ring buffer.
    pending: Vec<f32>,
    pending_pos: usize,
    /// Frames pushed into the ring buffer, on the same scale as `Shared::consumed`.
    written: u64,
    serial: u64,
    announced: Option<u64>,
}

impl Engine {
    pub fn new(
        shared: Arc<Shared>,
        interrupt: Interrupt,
        producer: Producer<f32>,
        commands: Receiver<Command>,
        events: Sender<Event>,
        rate: u32,
        channels: usize,
    ) -> Self {
        Self {
            shared,
            interrupt,
            producer,
            commands,
            events,
            rate,
            channels,
            current: None,
            next: None,
            converter: None,
            mapped: Vec::new(),
            pending: Vec::new(),
            pending_pos: 0,
            written: 0,
            serial: 0,
            announced: None,
        }
    }

    pub fn run(mut self) {
        loop {
            self.report_progress();

            let busy = self.current.is_some() || self.has_pending();
            let wake = if busy {
                self.poll_command()
            } else if self.shared.timeline().has_end() {
                // Everything is decoded; wait for the last audio to play out.
                self.wait_command(Some(POLL))
            } else {
                self.wait_command(None)
            };
            match wake {
                Wake::Command(cmd) => {
                    if !self.handle(cmd) {
                        return;
                    }
                    continue;
                }
                Wake::Disconnected => return,
                Wake::Timeout => {}
            }
            if !busy {
                continue;
            }

            if !self.push_pending() {
                // Ring buffer full: sleep until there is room or a command arrives.
                match self.wait_command(Some(POLL)) {
                    Wake::Command(cmd) => {
                        if !self.handle(cmd) {
                            return;
                        }
                    }
                    Wake::Disconnected => return,
                    Wake::Timeout => {}
                }
                continue;
            }
            if self.current.is_some() {
                self.decode_step();
            }
        }
    }

    fn poll_command(&self) -> Wake {
        match self.commands.try_recv() {
            Ok(cmd) => Wake::Command(cmd),
            Err(TryRecvError::Empty) => Wake::Timeout,
            Err(TryRecvError::Disconnected) => Wake::Disconnected,
        }
    }

    fn wait_command(&self, timeout: Option<Duration>) -> Wake {
        match timeout {
            Some(t) => match self.commands.recv_timeout(t) {
                Ok(cmd) => Wake::Command(cmd),
                Err(RecvTimeoutError::Timeout) => Wake::Timeout,
                Err(RecvTimeoutError::Disconnected) => Wake::Disconnected,
            },
            None => match self.commands.recv() {
                Ok(cmd) => Wake::Command(cmd),
                Err(_) => Wake::Disconnected,
            },
        }
    }

    /// Returns `false` when the engine should shut down.
    fn handle(&mut self, cmd: Command) -> bool {
        if matches!(cmd, Command::Play(_) | Command::Stop | Command::Quit) {
            self.interrupt.lower();
        }
        match cmd {
            Command::Play(media) => self.play(&media),
            Command::SetNext(media) => self.next = media,
            Command::Seek(position) => self.seek(position),
            Command::Stop => {
                self.reset_output();
                self.current = None;
            }
            Command::Quit => return false,
        }
        true
    }

    fn play(&mut self, media: &Media) {
        self.reset_output();
        self.current = None;
        self.shared.paused.store(false, Ordering::Relaxed);
        match self.open(media, None) {
            Ok(cur) => {
                self.shared.timeline().push(
                    self.written,
                    Duration::ZERO,
                    cur.serial,
                    Arc::clone(&cur.info),
                );
                self.current = Some(cur);
            }
            Err(e) => self.emit_error(&e),
        }
    }

    fn seek(&mut self, position: Duration) {
        // Radio cannot be wound; dropping what is buffered would only make a gap.
        if self.current.as_ref().is_some_and(|c| c.source.is_live()) {
            return;
        }
        let consumed = self.shared.consumed.load(Ordering::Acquire);
        let Some(playing) = self.shared.timeline().current(consumed) else {
            return;
        };

        // With gapless playback the decoder may already be on the next track. The seek
        // applies to the one being heard, so reopen it and put the other one back in the queue.
        if self.current.as_ref().map(|c| c.serial) != Some(playing.serial) {
            if let Some(ahead) = self.current.take() {
                self.next = Some(ahead.info.media());
            }
            match self.open(&playing.track.media(), Some(playing.serial)) {
                Ok(cur) => self.current = Some(cur),
                Err(e) => {
                    self.emit(Event::Error(e.to_string()));
                    return;
                }
            }
        }

        // The track stays current throughout, at the position asked for while the source
        // seeks: a status read meanwhile must not find nothing playing.
        self.flush_output();
        let cur = self.current.as_mut().expect("opened above");
        let info = Arc::clone(&cur.info);
        let serial = cur.serial;
        self.replace_timeline(position, serial, &info);
        let cur = self.current.as_mut().expect("opened above");
        let actual = cur.source.seek(position).unwrap_or_else(|e| {
            log::warn!("seek failed: {e}");
            playing.position
        });
        if actual != position {
            self.replace_timeline(actual, serial, &info);
        }
    }

    /// From what is written next, the timeline is only `track`, from `position`.
    fn replace_timeline(&self, position: Duration, serial: u64, track: &Arc<TrackInfo>) {
        let mut timeline = self.shared.timeline();
        timeline.clear();
        timeline.push(self.written, position, serial, Arc::clone(track));
    }

    fn open(&mut self, media: &Media, serial: Option<u64>) -> Result<Current, Error> {
        let source = Source::open(media, &self.interrupt)?;
        let serial = serial.unwrap_or_else(|| {
            self.serial += 1;
            self.serial
        });
        Ok(Current {
            info: Arc::new(source.info().clone()),
            source,
            serial,
        })
    }

    /// Discards everything buffered so that new audio is heard immediately, and forgets what
    /// was playing.
    fn reset_output(&mut self) {
        self.flush_output();
        self.shared.timeline().clear();
    }

    /// Discards everything buffered so that new audio is heard immediately. The timeline still
    /// says what was playing, for the caller to change.
    fn flush_output(&mut self) {
        self.shared.flush.store(true, Ordering::Release);
        let deadline = Instant::now() + FLUSH_TIMEOUT;
        while self.shared.flush.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                log::warn!("audio callback did not acknowledge flush");
                self.shared.flush.store(false, Ordering::Release);
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        self.pending.clear();
        self.pending_pos = 0;
        if let Some((_, conv)) = &mut self.converter {
            conv.reset();
        }
        self.written = self.shared.consumed.load(Ordering::Acquire);
    }

    fn decode_step(&mut self) {
        let Some(cur) = self.current.as_mut() else {
            return;
        };
        match cur.source.next_chunk() {
            Ok(Some(chunk)) => {
                self.mapped.clear();
                map_channels(
                    chunk.samples,
                    chunk.channels,
                    self.channels,
                    &mut self.mapped,
                );
                if chunk.rate == self.rate {
                    if let Some((_, conv)) = &mut self.converter {
                        conv.finish(&mut self.pending);
                    }
                    self.converter = None;
                    self.pending.extend_from_slice(&self.mapped);
                } else {
                    if self
                        .converter
                        .as_ref()
                        .is_some_and(|(r, _)| *r != chunk.rate)
                    {
                        if let Some((_, conv)) = &mut self.converter {
                            conv.finish(&mut self.pending);
                        }
                        self.converter = None;
                    }
                    let (_, conv) = self.converter.get_or_insert_with(|| {
                        (
                            chunk.rate,
                            RateConverter::new(chunk.rate, self.rate, self.channels),
                        )
                    });
                    conv.process(&self.mapped, &mut self.pending);
                }
            }
            Ok(None) => self.end_of_track(),
            // A command is waiting; the main loop takes it next.
            Err(Error::Interrupted) => {}
            Err(e) => {
                self.emit(Event::Error(e.to_string()));
                self.end_of_track();
            }
        }
        self.follow_info_change();
    }

    /// Makes a new song on the radio show from where it is heard.
    fn follow_info_change(&mut self) {
        let boundary = self.written + self.pending_frames();
        let Some(cur) = self.current.as_mut() else {
            return;
        };
        let Some(info) = cur.source.take_info_change() else {
            return;
        };
        cur.info = Arc::new(info.clone());
        self.shared
            .timeline()
            .change_track(boundary, Arc::clone(&cur.info));
    }

    fn end_of_track(&mut self) {
        if self.continue_in_same_file() {
            return;
        }
        // Drain the resampler so the track's last samples are not lost, and so the boundary to
        // the next track lands on an exact frame.
        if let Some((_, conv)) = &mut self.converter {
            conv.finish(&mut self.pending);
        }
        let boundary = self.written + self.pending_frames();
        self.current = None;

        if let Some(media) = self.next.take() {
            match self.open(&media, None) {
                Ok(cur) => {
                    self.shared.timeline().push(
                        boundary,
                        Duration::ZERO,
                        cur.serial,
                        Arc::clone(&cur.info),
                    );
                    self.current = Some(cur);
                    return;
                }
                Err(e) => self.emit_error(&e),
            }
        }
        self.shared.timeline().set_end(boundary);
    }

    /// Goes on decoding the same file when the next track is the part of it that follows, as
    /// with the tracks of a CUE sheet. The resampler is not drained: the audio goes on. That
    /// puts the boundary early by the few samples still in the resampler.
    fn continue_in_same_file(&mut self) -> bool {
        let boundary = self.written + self.pending_frames();
        let (Some(cur), Some(next)) = (self.current.as_mut(), self.next.as_ref()) else {
            return false;
        };
        if !cur.source.continue_with(next) {
            return false;
        }
        self.next = None;
        self.serial += 1;
        cur.serial = self.serial;
        cur.info = Arc::new(cur.source.info().clone());
        self.shared
            .timeline()
            .push(boundary, Duration::ZERO, cur.serial, Arc::clone(&cur.info));
        true
    }

    /// Frames converted but not yet in the ring buffer.
    fn pending_frames(&self) -> u64 {
        ((self.pending.len() - self.pending_pos) / self.channels) as u64
    }

    fn has_pending(&self) -> bool {
        self.pending_pos < self.pending.len()
    }

    /// Pushes as much pending audio as fits. Returns `true` if everything was pushed.
    fn push_pending(&mut self) -> bool {
        let ch = self.channels;
        let rest = &self.pending[self.pending_pos..];
        let room = self.producer.slots() / ch * ch;
        let n = rest.len().min(room);
        if n > 0 {
            let (pushed, _) = self.producer.push_partial_slice(&rest[..n]);
            self.pending_pos += pushed.len();
            self.written += (pushed.len() / ch) as u64;
        }
        if self.has_pending() {
            return false;
        }
        self.pending.clear();
        self.pending_pos = 0;
        true
    }

    fn report_progress(&mut self) {
        let consumed = self.shared.consumed.load(Ordering::Acquire);
        let mut timeline = self.shared.timeline();
        if let Some(playing) = timeline.current(consumed) {
            if self.announced != Some(playing.serial) {
                self.announced = Some(playing.serial);
                drop(timeline);
                self.emit(Event::TrackStarted(playing.track));
            }
        } else if timeline.is_finished(consumed) {
            timeline.clear();
            drop(timeline);
            self.announced = None;
            self.emit(Event::Finished);
        }
    }

    /// Reports an error, unless it only means a command cut a wait short.
    fn emit_error(&self, error: &Error) {
        if !matches!(error, Error::Interrupted) {
            self.emit(Event::Error(error.to_string()));
        }
    }

    fn emit(&self, event: Event) {
        // Nobody listening is fine, e.g. during shutdown.
        let _ = self.events.send(event);
    }
}
