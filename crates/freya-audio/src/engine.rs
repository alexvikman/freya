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
/// The longest the engine sleeps while the ring buffer is full. Commands wake it at once; this
/// bounds how late it notices the next track starting to play.
const REFILL_WAIT_MAX: Duration = Duration::from_millis(50);
/// How long to wait for the audio callback to acknowledge a flush.
const FLUSH_TIMEOUT: Duration = Duration::from_millis(500);

pub(crate) enum Command {
    Play(Media),
    SetNext(Option<Media>),
    Seek(Duration),
    Stop,
    /// Playback was resumed: the engine may have stopped waking up while paused.
    Resume,
    Quit,
}

/// Notifications from the engine.
#[derive(Clone, Debug)]
pub enum Event {
    /// A track became audible.
    TrackStarted(Arc<TrackInfo>),
    /// The last track finished and nothing was queued.
    Finished,
    /// The media with this [`Media::id`] could not be opened, whether asked to play or queued
    /// next; the engine plays nothing in its place, so the caller decides what comes instead.
    Unplayable(u64, String),
    /// Something went wrong while a track played, like a damaged part of it: the engine goes
    /// on with what is queued next, if anything.
    Error(String),
}

struct Current {
    source: Source,
    serial: u64,
    info: Arc<TrackInfo>,
}

// Lives only until it is matched, so a large command costs nothing worth a box.
#[allow(clippy::large_enum_variant)]
enum Wake {
    Command(Command),
    Timeout,
    Disconnected,
}

/// A track opened ahead: what it plays, its source once opened, and what gives up on it.
struct OpenedAhead {
    media: Media,
    opened: crossbeam_channel::Receiver<Result<Source, Error>>,
    given_up: Interrupt,
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
    /// The next track, opened ahead on a thread when it is read from a server, so it follows
    /// without a gap.
    ahead: Option<OpenedAhead>,
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
            ahead: None,
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
                // Everything is decoded; wait for the last audio to play out, or, while paused,
                // for playback to resume.
                self.wait_command((!self.paused()).then_some(POLL))
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
                // Ring buffer full: sleep until there is room or a command arrives. Paused,
                // nothing makes room until playback resumes, which sends a command.
                let wait = (!self.paused()).then(|| self.refill_wait());
                match self.wait_command(wait) {
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

    fn paused(&self) -> bool {
        self.shared.paused.load(Ordering::Acquire)
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
            Command::SetNext(media) => {
                self.open_ahead(media.as_ref());
                self.next = media;
            }
            Command::Seek(position) => self.seek(position),
            Command::Stop => {
                self.reset_output();
                self.current = None;
            }
            Command::Resume => {}
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
            Err(e) => self.emit_unplayable(media, &e),
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
        // To the end or past it, as holding → does: the track is over. The reader would refuse
        // the position, and what is buffered was already dropped by then.
        if playing
            .track
            .duration
            .is_some_and(|length| position >= length)
        {
            self.skip_to_next();
            return;
        }

        // With gapless playback the decoder may already be on the next track. The seek
        // applies to the one being heard, so reopen it and put the other one back in the queue.
        if self.current.as_ref().map(|c| c.serial) != Some(playing.serial) {
            if let Some(ahead) = self.current.take() {
                self.next = Some(ahead.info.media());
            }
            match self.open(&playing.track.media(), Some(playing.serial)) {
                Ok(cur) => self.current = Some(cur),
                Err(e) => {
                    // Nothing could go on decoding it: go on with what follows.
                    self.emit(Event::Error(e.to_string()));
                    self.skip_to_next();
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
        match cur.source.seek(position) {
            Ok(actual) if actual != position => self.replace_timeline(actual, serial, &info),
            Ok(_) => {}
            // Where the reader is now is not known, so the position shown would drift from
            // what is heard: go on with what follows instead.
            Err(e) => {
                log::warn!("seek failed: {e}");
                self.skip_to_next();
            }
        }
    }

    /// Ends the track being heard at once: what is queued next plays, or playback finishes.
    fn skip_to_next(&mut self) {
        self.reset_output();
        self.current = None;
        self.announced = None;
        match self.next.take() {
            Some(media) => self.play(&media),
            None => self.emit(Event::Finished),
        }
    }

    /// From what is written next, the timeline is only `track`, from `position`.
    fn replace_timeline(&self, position: Duration, serial: u64, track: &Arc<TrackInfo>) {
        let mut timeline = self.shared.timeline();
        timeline.clear();
        timeline.push(self.written, position, serial, Arc::clone(track));
    }

    /// Opens `media`, the next track, on a thread of its own when it is read from a server,
    /// giving up on the one opened ahead before.
    fn open_ahead(&mut self, media: Option<&Media>) {
        if let Some(ahead) = self.ahead.take() {
            ahead.given_up.raise();
        }
        let Some(media) = media.filter(|media| media.remote.is_some()) else {
            return;
        };
        let (tell, opened) = crossbeam_channel::bounded(1);
        let given_up = Interrupt::default();
        let (opening, interrupt) = (media.clone(), given_up.clone());
        let started = std::thread::Builder::new()
            .name("freya-open-next".into())
            .spawn(move || {
                // Its first audio decoded too, which may be far from what opening it read.
                let opened = Source::open(&opening, &interrupt).and_then(|mut source| {
                    source.prime()?;
                    Ok(source)
                });
                let _ = tell.send(opened);
            });
        if started.is_ok() {
            self.ahead = Some(OpenedAhead {
                media: media.clone(),
                opened,
                given_up,
            });
        }
    }

    /// The source of `media`: opened ahead when it was, else opened now. Waiting for it gives
    /// up, as opening it would, when a command comes that should not wait.
    fn source(&mut self, media: &Media) -> Result<Source, Error> {
        let opened_ahead = self
            .ahead
            .as_ref()
            .is_some_and(|ahead| ahead.media == *media);
        if let Some(OpenedAhead {
            opened, given_up, ..
        }) = self.ahead.take_if(|_| opened_ahead)
        {
            loop {
                match opened.recv_timeout(Duration::from_millis(20)) {
                    Ok(source) => return source,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        if self.interrupt.is_raised() {
                            given_up.raise();
                            return Err(Error::Interrupted);
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }
            }
        }
        Source::open(media, &self.interrupt)
    }

    fn open(&mut self, media: &Media, serial: Option<u64>) -> Result<Current, Error> {
        let source = self.source(media)?;
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
                Err(e) => self.emit_unplayable(&media, &e),
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

    /// How long until the ring buffer has drained to half full, at most [`REFILL_WAIT_MAX`].
    /// Waking only then, rather than every few milliseconds to top it up a little, saves most
    /// of the engine's CPU time while it plays: every wake costs more than the decoding.
    fn refill_wait(&self) -> Duration {
        let capacity = self.producer.buffer().capacity();
        let missing = (capacity / 2).saturating_sub(self.producer.slots());
        let frames = missing / self.channels.max(1);
        let wait = Duration::from_secs_f64(frames as f64 / f64::from(self.rate.max(1)));
        wait.clamp(POLL, REFILL_WAIT_MAX)
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

    /// Reports that `media` could not be opened, unless a command only cut the wait short.
    fn emit_unplayable(&self, media: &Media, error: &Error) {
        if !matches!(error, Error::Interrupted) {
            self.emit(Event::Unplayable(media.id, error.to_string()));
        }
    }

    fn emit(&self, event: Event) {
        // Nobody listening is fine, e.g. during shutdown.
        let _ = self.events.send(event);
    }
}
