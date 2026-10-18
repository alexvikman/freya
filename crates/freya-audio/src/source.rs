//! Decoding a media file into interleaved `f32` samples using symphonia.

use std::fs::File;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::{Hint, Probe};
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTag};
use symphonia::core::units::{Time, TimeBase};

use crate::Error;
use crate::ape;
use crate::stream::{self, Interrupt, Stream, Titles};

/// Symphonia's decoders, plus Opus from libopus and Monkey's Audio of our own, which Symphonia
/// does not have.
pub(crate) static CODECS: LazyLock<CodecRegistry> = LazyLock::new(|| {
    let mut codecs = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut codecs);
    #[cfg(feature = "opus")]
    codecs.register_audio_decoder::<symphonia_adapter_libopus::OpusDecoder>();
    codecs.register_audio_decoder::<ape::ApeDecoder>();
    codecs
});

/// Symphonia's formats and tag readers, plus Monkey's Audio.
pub(crate) static PROBE: LazyLock<Probe> = LazyLock::new(|| {
    let mut probe = Probe::default();
    symphonia::default::register_enabled_formats(&mut probe);
    probe.register_format::<ape::ApeReader<'_>>();
    probe
});

/// What to play: a file or stream, or a part of a file, like a track of a CUE sheet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Media {
    pub path: PathBuf,
    /// Where to start; zero for the whole file.
    pub start: Duration,
    /// Where to stop; `None` for the end of the file.
    pub end: Option<Duration>,
    /// What to show instead of the file's tags, which describe the whole file rather than
    /// the part played.
    pub tags: MediaTags,
    /// The caller's name for this request, handed back in [`TrackInfo::id`] and
    /// [`crate::Event::Unplayable`], so it can tell which of its entries plays or failed even
    /// when two play the same file. 0 when it has none.
    pub id: u64,
}

/// Tags that replace a file's. One left out keeps the file's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

impl<P: Into<PathBuf>> From<P> for Media {
    fn from(path: P) -> Self {
        Self {
            path: path.into(),
            ..Default::default()
        }
    }
}

/// Description of a track, as known when the file is opened.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackInfo {
    /// The [`Media::id`] this track was opened for.
    pub id: u64,
    pub path: PathBuf,
    /// The part of the file played, as in [`Media`]. Position and `duration` count from
    /// `start`.
    pub start: Duration,
    pub end: Option<Duration>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
    pub sample_rate: u32,
    pub channels: usize,
    /// Short codec name, e.g. `mp3` or `flac`.
    pub codec: String,
    /// Average bitrate estimated from file size and duration, or as a station states it.
    pub bitrate_kbps: Option<u32>,
    /// For internet radio: the station's name. `title` and `artist` then follow the song
    /// being played, as far as the station says.
    pub station: Option<String>,
}

impl TrackInfo {
    /// What to open to play this track again.
    pub(crate) fn media(&self) -> Media {
        Media {
            path: self.path.clone(),
            start: self.start,
            end: self.end,
            tags: MediaTags {
                title: self.title.clone(),
                artist: self.artist.clone(),
                album: self.album.clone(),
            },
            id: self.id,
        }
    }

    /// Title to display: "Artist - Title" if tagged, otherwise the station or file name.
    pub fn display_title(&self) -> String {
        match (&self.artist, &self.title, &self.station) {
            (Some(artist), Some(title), _) => format!("{artist} - {title}"),
            (None, Some(title), _) => title.clone(),
            (_, None, Some(station)) => station.clone(),
            _ => self
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }
}

/// A block of decoded audio. Sample rate and channel count can, rarely, change mid-stream,
/// so they travel with every block.
pub struct Chunk<'a> {
    pub samples: &'a [f32],
    pub rate: u32,
    pub channels: usize,
}

pub struct Source {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    info: TrackInfo,
    /// The length and tags of the whole file, of which a part may be played.
    file_duration: Option<Duration>,
    file_tags: MediaTags,
    /// The frame of the file that the next sample returned comes from.
    position: u64,
    /// The frame to stop at, when playing a part that ends before the file does.
    end_frame: Option<u64>,
    /// Frames to discard after a seek to land on the exact position.
    skip_frames: usize,
    /// The last packet decoded, of which `buf[buf_pos..]` has not been returned yet. A packet
    /// that runs over the end of a part keeps the rest for the part that follows.
    buf: Vec<f32>,
    buf_pos: usize,
    buf_rate: u32,
    buf_channels: usize,
    /// Song titles, when this is internet radio.
    titles: Option<Titles>,
    /// Set when `info` changed while playing: a new song on the radio, or a new part of a
    /// chained Ogg file.
    info_changed: bool,
}

impl Source {
    /// Opens a file, or an `http://` or `https://` stream, and seeks to where `media` starts.
    /// Opening a stream gives up with [`Error::Interrupted`] when `interrupt` is raised.
    pub fn open(media: &Media, interrupt: &Interrupt) -> Result<Self, Error> {
        let path = media.path.as_path();
        let open_err = |msg: String| Error::Open(path.to_owned(), msg);
        let mut hint = Hint::new();
        let mut station = None;
        let mut titles = None;
        let mut file_len = None;

        let input: Box<dyn MediaSource> = match path.to_str().filter(|p| stream::is_url(p)) {
            Some(url) => {
                let (stream, info, stream_titles) = Stream::open(url, interrupt.clone())?;
                if let Some(ext) = info.extension_hint() {
                    hint.with_extension(ext);
                }
                station = Some(info);
                titles = Some(stream_titles);
                Box::new(ReadOnlySource::new(stream))
            }
            None => {
                let file = File::open(path).map_err(|e| open_err(e.to_string()))?;
                file_len = file.metadata().ok().map(|m| m.len());
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    hint.with_extension(ext);
                }
                Box::new(file)
            }
        };
        let mss = MediaSourceStream::new(input, Default::default());

        let mut format = PROBE
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| {
                if interrupt.is_raised() {
                    Error::Interrupted
                } else {
                    open_err(e.to_string())
                }
            })?;

        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| open_err("no audio track".into()))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| open_err("unknown codec parameters".into()))?;

        let decoder = CODECS
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| open_err(e.to_string()))?;

        let track_id = track.id;
        let time_base = track.time_base;
        let sample_rate = params.sample_rate.unwrap_or(0);
        let channel_count = params.channels.as_ref().map_or(0, |c| c.count());
        if !valid_rate(sample_rate) || channel_count == 0 {
            return Err(open_err(format!(
                "unplayable format ({sample_rate} Hz, {channel_count} channels)"
            )));
        }
        let duration = match (time_base, track.duration, track.num_frames) {
            (Some(tb), Some(dur), _) => tb.calc_duration(dur).map(time_to_duration),
            (_, _, Some(frames)) if sample_rate > 0 => Some(Duration::from_secs_f64(
                frames as f64 / f64::from(sample_rate),
            )),
            _ => None,
        };
        let bitrate_kbps = match (file_len, duration) {
            (Some(len), Some(d)) if d.as_secs_f64() > 0.0 => {
                Some((len as f64 * 8.0 / d.as_secs_f64() / 1000.0).round() as u32)
            }
            _ => station.as_ref().and_then(|s| s.bitrate_kbps),
        };

        let mut info = TrackInfo {
            path: path.to_owned(),
            sample_rate,
            channels: channel_count,
            codec: decoder.codec_info().short_name.to_owned(),
            bitrate_kbps,
            station: station.and_then(|s| s.name),
            ..Default::default()
        };
        if let Some(rev) = format.metadata().skip_to_latest() {
            apply_tags(&mut info, rev);
        }

        let file_tags = MediaTags {
            title: info.title.clone(),
            artist: info.artist.clone(),
            album: info.album.clone(),
        };
        let mut source = Self {
            format,
            decoder,
            track_id,
            time_base,
            info,
            file_duration: duration,
            file_tags,
            position: 0,
            end_frame: None,
            skip_frames: 0,
            buf: Vec::new(),
            buf_pos: 0,
            buf_rate: 0,
            buf_channels: 0,
            titles,
            info_changed: false,
        };
        source.play_part(media);
        if !media.start.is_zero() {
            source
                .seek(Duration::ZERO)
                .map_err(|e| open_err(e.to_string()))?;
        }
        Ok(source)
    }

    /// Plays `media`'s part of the file from here on, and describes it in `info`.
    fn play_part(&mut self, media: &Media) {
        self.end_frame = media.end.map(|end| self.frame_at(end));
        let info = &mut self.info;
        info.id = media.id;
        info.start = media.start;
        info.end = media.end;
        info.duration = media
            .end
            .or(self.file_duration)
            .map(|end| end.saturating_sub(media.start));
        let (tags, file) = (&media.tags, &self.file_tags);
        info.title = tags.title.clone().or_else(|| file.title.clone());
        info.artist = tags.artist.clone().or_else(|| file.artist.clone());
        info.album = tags.album.clone().or_else(|| file.album.clone());
    }

    /// Goes on with `next` if it is the part of the file right after the one that just ended,
    /// as the next track of a CUE sheet usually is. Nothing is opened or sought then, and not
    /// a sample is lost between the two. Returns whether it did.
    pub fn continue_with(&mut self, next: &Media) -> bool {
        // Past the end, after a seek there, would skip the start of `next`.
        let at_end = self.end_frame == Some(self.position);
        if !at_end || next.path != self.info.path || Some(next.start) != self.info.end {
            return false;
        }
        self.play_part(next);
        true
    }

    /// The frame of the file at `time`.
    fn frame_at(&self, time: Duration) -> u64 {
        (time.as_secs_f64() * f64::from(self.info.sample_rate)).round() as u64
    }

    /// Whether this is internet radio, which has no end and cannot seek.
    pub fn is_live(&self) -> bool {
        self.titles.is_some()
    }

    /// The new description, if it changed since the last call: a new song on the radio, or
    /// the next part of a chained Ogg stream.
    pub fn take_info_change(&mut self) -> Option<&TrackInfo> {
        if let Some(text) = self.titles.as_ref().and_then(Titles::take) {
            let (artist, title) = stream::split_title(&text);
            self.info.artist = artist;
            self.info.title = title;
            self.info_changed = true;
        }
        if !std::mem::take(&mut self.info_changed) {
            return None;
        }
        Some(&self.info)
    }

    /// After a chained Ogg stream starts a new part: picks up its track and tags, with a new
    /// decoder. Returns false if the new part cannot be played.
    fn restart_decoder(&mut self) -> bool {
        let Some(track) = self.format.default_track(TrackType::Audio) else {
            return false;
        };
        let Some(params) = track.codec_params.as_ref().and_then(|p| p.audio()) else {
            return false;
        };
        let Ok(decoder) = CODECS.make_audio_decoder(params, &AudioDecoderOptions::default()) else {
            return false;
        };
        self.track_id = track.id;
        self.time_base = track.time_base;
        self.decoder = decoder;
        if let Some(rev) = self.format.metadata().skip_to_latest() {
            apply_tags(&mut self.info, rev);
            self.info_changed = true;
        }
        true
    }

    pub fn info(&self) -> &TrackInfo {
        &self.info
    }

    /// The next block of audio. Returns `None` at the end of the track.
    pub fn next_chunk(&mut self) -> Result<Option<Chunk<'_>>, Error> {
        if self.frames_left() == Some(0) {
            return Ok(None);
        }
        if self.buf_pos >= self.buf.len() && !self.decode_packet()? {
            return Ok(None);
        }
        let channels = self.buf_channels;
        let mut frames = (self.buf.len() - self.buf_pos) / channels;
        if let Some(left) = self.frames_left() {
            frames = frames.min(left);
        }
        let from = self.buf_pos;
        self.buf_pos += frames * channels;
        self.position += frames as u64;
        Ok(Some(Chunk {
            samples: &self.buf[from..self.buf_pos],
            rate: self.buf_rate,
            channels,
        }))
    }

    /// Frames left of the part played, if it ends before the file does.
    fn frames_left(&self) -> Option<usize> {
        self.end_frame
            .map(|end| end.saturating_sub(self.position) as usize)
    }

    /// Decodes the next packet into `buf`. Returns `false` at the end of the file.
    fn decode_packet(&mut self) -> Result<bool, Error> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(false),
                // A chained Ogg stream, as Ogg radio stations send, starts a new part per song.
                Err(SymError::ResetRequired) => {
                    if self.restart_decoder() {
                        continue;
                    }
                    return Ok(false);
                }
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false);
                }
                Err(SymError::IoError(e))
                    if e.kind() == stream::interrupted().kind() && self.titles.is_some() =>
                {
                    return Err(Error::Interrupted);
                }
                Err(e) => return Err(Error::Decode(e.to_string())),
            };
            if packet.track_id != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(SymError::DecodeError(msg)) => {
                    log::warn!("silencing corrupt packet: {msg}");
                    // Silence in its place keeps the position, and the parts of a CUE sheet,
                    // where they are.
                    if self.silence(packet.dur.get()) && self.start_buffer() {
                        return Ok(true);
                    }
                    continue;
                }
                Err(SymError::IoError(e)) => {
                    log::warn!("skipping packet: {e}");
                    continue;
                }
                Err(e) => return Err(Error::Decode(e.to_string())),
            };

            let channels = decoded.spec().channels().count().max(1);
            if !valid_rate(decoded.spec().rate()) {
                return Err(Error::Decode(format!(
                    "unplayable sample rate {} Hz",
                    decoded.spec().rate()
                )));
            }
            self.buf_rate = decoded.spec().rate();
            self.buf_channels = channels;
            decoded.copy_to_vec_interleaved(&mut self.buf);
            if self.start_buffer() {
                return Ok(true);
            }
        }
    }

    /// Fills `buf` with `frames` of silence, in the format of the audio around it. Returns
    /// `false` for a length no packet has, from a damaged file.
    fn silence(&mut self, frames: u64) -> bool {
        if self.buf_rate == 0 {
            self.buf_rate = self.info.sample_rate;
            self.buf_channels = self.info.channels.max(1);
        }
        if frames == 0 || frames > u64::from(self.buf_rate) * 10 {
            return false;
        }
        self.buf.clear();
        self.buf.resize(frames as usize * self.buf_channels, 0.0);
        true
    }

    /// Skips what a seek asked to past the start of the new `buf`. Returns whether any of it
    /// is left to play.
    fn start_buffer(&mut self) -> bool {
        let skip = self.skip_frames.min(self.buf.len() / self.buf_channels);
        self.skip_frames -= skip;
        self.buf_pos = skip * self.buf_channels;
        self.buf_pos < self.buf.len()
    }

    /// Seeks to `position` in the part played and returns the position actually reached.
    pub fn seek(&mut self, position: Duration) -> Result<Duration, Error> {
        let target = self.info.start + position;
        let time = Time::try_from_secs_f64(target.as_secs_f64())
            .ok_or_else(|| Error::Seek("invalid position".into()))?;
        let seeked = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|e| Error::Seek(e.to_string()))?;
        self.decoder.reset();
        self.buf.clear();
        self.buf_pos = 0;

        // The format reader can only seek to the nearest packet. Decode from there and drop
        // everything before the requested position.
        self.skip_frames = 0;
        let Some(tb) = self.time_base else {
            self.position = self.frame_at(target);
            return Ok(position);
        };
        let required = time_to_duration(tb.calc_time_saturating(seeked.required_ts));
        let actual = time_to_duration(tb.calc_time_saturating(seeked.actual_ts));
        self.skip_frames = self.frame_at(required.saturating_sub(actual)) as usize;
        self.position = self.frame_at(required);
        Ok(required.saturating_sub(self.info.start))
    }
}

/// The highest sample rate played. Files claiming more (or none) are damaged or not audio,
/// and would only make the rate converter fail.
const MAX_SAMPLE_RATE: u32 = 768_000;

fn valid_rate(rate: u32) -> bool {
    (1..=MAX_SAMPLE_RATE).contains(&rate)
}

fn time_to_duration(time: Time) -> Duration {
    Duration::from_secs_f64(time.as_secs_f64().max(0.0))
}

fn apply_tags(info: &mut TrackInfo, rev: &MetadataRevision) {
    for tag in &rev.media.tags {
        match &tag.std {
            Some(StandardTag::TrackTitle(v)) => info.title = Some(v.to_string()),
            Some(StandardTag::Artist(v)) => info.artist = Some(v.to_string()),
            Some(StandardTag::Album(v)) => info.album = Some(v.to_string()),
            _ => {}
        }
    }
}
