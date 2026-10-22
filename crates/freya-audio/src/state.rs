//! State shared between the public API, the decoder thread and the audio callback.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::TrackInfo;
use crate::eq::SharedEq;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

/// Snapshot of playback, for drawing UI.
#[derive(Clone, Debug)]
pub struct Status {
    pub state: PlaybackState,
    pub track: Option<Arc<TrackInfo>>,
    pub position: Duration,
}

pub(crate) struct Shared {
    volume: AtomicU32,
    balance: AtomicU32,
    pub paused: AtomicBool,
    /// Set by the decoder thread to ask the callback to discard everything in the ring buffer.
    pub flush: AtomicBool,
    /// Frames played by the callback since the stream started. Never decreases.
    pub consumed: AtomicU64,
    /// Frames the callback had nothing for while not paused, as gaps, a slow start or the end
    /// of what was queued. Never decreases.
    pub starved: AtomicU64,
    pub eq: SharedEq,
    /// Whether the audio callback copies played samples for visualization.
    pub visualize: AtomicBool,
    /// Set by the audio stream when its device is gone and the stream must be built again.
    pub device_lost: AtomicBool,
    timeline: Mutex<Timeline>,
}

impl Shared {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            volume: AtomicU32::new(1.0f32.to_bits()),
            balance: AtomicU32::new(0.0f32.to_bits()),
            paused: AtomicBool::new(false),
            flush: AtomicBool::new(false),
            consumed: AtomicU64::new(0),
            starved: AtomicU64::new(0),
            eq: SharedEq::new(),
            visualize: AtomicBool::new(false),
            device_lost: AtomicBool::new(false),
            timeline: Mutex::new(Timeline::new(sample_rate)),
        }
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    /// Ignores a value that is not a number, which would silence the output and be saved.
    pub fn set_volume(&self, v: f32) {
        if v.is_nan() {
            return;
        }
        self.volume
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn balance(&self) -> f32 {
        f32::from_bits(self.balance.load(Ordering::Relaxed))
    }

    pub fn set_balance(&self, b: f32) {
        if b.is_nan() {
            return;
        }
        self.balance
            .store(b.clamp(-1.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Gain for the left and right channel. Volume is squared so the slider feels even to the
    /// ear.
    pub fn gains(&self) -> (f32, f32) {
        let v = self.volume();
        let gain = v * v;
        let b = self.balance();
        (gain * (1.0 - b).min(1.0), gain * (1.0 + b).min(1.0))
    }

    pub fn timeline(&self) -> MutexGuard<'_, Timeline> {
        self.timeline.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> Status {
        let consumed = self.consumed.load(Ordering::Acquire);
        match self.timeline().current(consumed) {
            Some(playing) => Status {
                state: if self.paused.load(Ordering::Relaxed) {
                    PlaybackState::Paused
                } else {
                    PlaybackState::Playing
                },
                track: Some(playing.track),
                position: playing.position,
            },
            None => Status {
                state: PlaybackState::Stopped,
                track: None,
                position: Duration::ZERO,
            },
        }
    }
}

/// The segment of the timeline that is audible right now.
pub(crate) struct Playing {
    pub serial: u64,
    pub track: Arc<TrackInfo>,
    pub position: Duration,
}

struct Segment {
    /// Stream position (in frames) where the segment becomes audible.
    stream_start: u64,
    /// Track position (in frames) at `stream_start`. Non-zero after a seek.
    offset: u64,
    serial: u64,
    track: Arc<TrackInfo>,
}

/// Which track starts where in the output stream.
///
/// The decoder runs ahead of playback by one ring buffer, and with gapless playback the next
/// track is already buffered before it is heard. The timeline makes position and current track
/// follow what is actually playing.
pub(crate) struct Timeline {
    rate: u32,
    segments: VecDeque<Segment>,
    /// Where in the stream audio ends, if no further track is queued.
    end: Option<u64>,
}

impl Timeline {
    fn new(rate: u32) -> Self {
        Self {
            rate,
            segments: VecDeque::new(),
            end: None,
        }
    }

    pub fn clear(&mut self) {
        self.segments.clear();
        self.end = None;
    }

    pub fn push(
        &mut self,
        stream_start: u64,
        offset: Duration,
        serial: u64,
        track: Arc<TrackInfo>,
    ) {
        self.end = None;
        self.segments.push_back(Segment {
            stream_start,
            offset: (offset.as_secs_f64() * f64::from(self.rate)).round() as u64,
            serial,
            track,
        });
    }

    /// From `stream_start` on, the last track is described by `track`: the radio started a new
    /// song. Position carries on.
    pub fn change_track(&mut self, stream_start: u64, track: Arc<TrackInfo>) {
        let Some(last) = self.segments.back() else {
            return;
        };
        let offset = last.offset + stream_start.saturating_sub(last.stream_start);
        let serial = last.serial;
        self.segments.push_back(Segment {
            stream_start,
            offset,
            serial,
            track,
        });
    }

    pub fn set_end(&mut self, at: u64) {
        self.end = Some(at);
    }

    pub fn is_finished(&self, consumed: u64) -> bool {
        self.end.is_some_and(|end| consumed >= end)
    }

    pub fn has_end(&self) -> bool {
        self.end.is_some()
    }

    /// The track being heard right now, and the position within it.
    pub fn current(&mut self, consumed: u64) -> Option<Playing> {
        if self.is_finished(consumed) {
            return None;
        }
        // Drop segments that have finished playing.
        while self.segments.len() > 1 && self.segments[1].stream_start <= consumed {
            self.segments.pop_front();
        }
        let seg = self.segments.front()?;
        let frames = seg.offset + consumed.saturating_sub(seg.stream_start);
        Some(Playing {
            serial: seg.serial,
            track: Arc::clone(&seg.track),
            position: Duration::from_secs_f64(frames as f64 / f64::from(self.rate)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(name: &str) -> Arc<TrackInfo> {
        Arc::new(TrackInfo {
            path: name.into(),
            ..Default::default()
        })
    }

    #[test]
    fn volume_and_balance_ignore_what_is_not_a_number() {
        // MPRIS can send NaN; it silenced Freya and was saved.
        let shared = Shared::new(48_000);
        shared.set_volume(0.5);
        shared.set_volume(f32::NAN);
        assert_eq!(shared.volume(), 0.5);
        shared.set_balance(f32::NAN);
        assert_eq!(shared.balance(), 0.0);
    }

    #[test]
    fn follows_gapless_boundary() {
        let mut tl = Timeline::new(1000);
        tl.push(0, Duration::ZERO, 1, track("a"));
        tl.push(5000, Duration::ZERO, 2, track("b"));
        tl.set_end(8000);

        let p = tl.current(4000).unwrap();
        assert_eq!(p.track.path.to_str(), Some("a"));
        assert_eq!(p.position, Duration::from_secs(4));

        let p = tl.current(6500).unwrap();
        assert_eq!(p.track.path.to_str(), Some("b"));
        assert_eq!(p.serial, 2);
        assert_eq!(p.position, Duration::from_millis(1500));

        assert!(tl.current(8000).is_none());
    }

    #[test]
    fn a_new_title_takes_over_where_it_is_heard() {
        let mut tl = Timeline::new(1000);
        tl.push(0, Duration::ZERO, 1, track("radio"));
        let retitled = Arc::new(TrackInfo {
            path: "radio".into(),
            title: Some("Next song".into()),
            ..Default::default()
        });
        tl.change_track(3000, retitled);
        assert_eq!(tl.current(2000).unwrap().track.title, None);
        let p = tl.current(4000).unwrap();
        assert_eq!(p.track.title.as_deref(), Some("Next song"));
        assert_eq!(p.serial, 1, "still the same track");
        assert_eq!(p.position, Duration::from_secs(4), "position carries on");
    }

    #[test]
    fn seek_offset_is_added() {
        let mut tl = Timeline::new(1000);
        tl.push(200, Duration::from_secs(60), 1, track("a"));
        assert_eq!(tl.current(1200).unwrap().position, Duration::from_secs(61));
    }
}
