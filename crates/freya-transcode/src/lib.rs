//! A music file made smaller for a slow network: decoded, and encoded again as Opus in Ogg,
//! at a constant bitrate. Every packet is as long as the next and every page holds as many,
//! so how long the whole file is is known before it is encoded: a server can answer for any
//! part of it, by its length, while it is being made. The same file encodes to the same bytes.

mod ogg;
mod opus;

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use freya_decode::{CODECS, PROBE, RateConverter, map_channels};

/// How small a track is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Quality {
    /// 160 kbit/s.
    High,
    /// 96 kbit/s.
    Normal,
}

impl Quality {
    pub const ALL: [Quality; 2] = [Quality::High, Quality::Normal];

    /// Its name, as a client asks for it.
    pub fn name(self) -> &'static str {
        match self {
            Quality::High => "high",
            Quality::Normal => "normal",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|quality| quality.name() == name)
    }

    /// The bytes a second it plays.
    pub fn bytes_per_second(self) -> u32 {
        self.packet() as u32 * opus::RATE / opus::FRAME as u32
    }

    /// The bytes of each packet, of 20 ms.
    fn packet(self) -> usize {
        match self {
            Quality::High => 400,
            Quality::Normal => 240,
        }
    }
}

/// A file only made smaller when it plays this much more a second than the smaller one would:
/// much less, and it would sound worse for little.
pub const SMALLER_BY: f64 = 1.5;

/// The longest file made smaller; a longer one is a recording of a day, not music to play.
const LONGEST_SECONDS: f64 = 12.0 * 60.0 * 60.0;

/// What encoding writes to say who did, in its tags.
const VENDOR: &[u8] = b"Freya";
/// The stream's serial number: any, the same every time.
const SERIAL: u32 = 0x4672_6579;

/// Why a file could not be made smaller.
#[derive(Debug)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}

impl From<SymError> for Error {
    fn from(e: SymError) -> Self {
        Self(e.to_string())
    }
}

impl From<String> for Error {
    fn from(e: String) -> Self {
        Self(e)
    }
}

/// A file being decoded.
struct Decoding {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track: u32,
    rate: u32,
    channels: usize,
    /// Its frames, as the file says.
    frames: Option<u64>,
}

impl Decoding {
    fn open(file: File, extension: Option<&str>) -> Result<Self, Error> {
        let mut hint = Hint::new();
        if let Some(extension) = extension {
            hint.with_extension(extension);
        }
        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let format = PROBE.probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| Error("no audio in it".into()))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|params| params.audio())
            .ok_or_else(|| Error("no codec it tells".into()))?;
        let decoder = CODECS.make_audio_decoder(params, &AudioDecoderOptions::default())?;
        let rate = params.sample_rate.unwrap_or(0);
        let channels = params.channels.as_ref().map_or(0, |c| c.count());
        if !(1..=768_000).contains(&rate) || channels == 0 {
            return Err(Error(format!("{rate} Hz, {channels} channels")));
        }
        let frames = match (track.time_base, track.duration, track.num_frames) {
            (Some(base), Some(duration), _) => base
                .calc_duration(duration)
                .map(|time| (time.as_secs_f64().max(0.0) * f64::from(rate)).round() as u64),
            (_, _, Some(frames)) => Some(frames),
            _ => None,
        };
        let track = track.id;
        Ok(Self {
            format,
            decoder,
            track,
            rate,
            channels,
            frames,
        })
    }

    /// The next samples, interleaved, in `samples`; false at the end.
    fn next(&mut self, samples: &mut Vec<f32>) -> Result<bool, Error> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(false),
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false);
                }
                // A chained Ogg stream's next part: the first is the file's music.
                Err(SymError::ResetRequired) => return Ok(false),
                Err(e) => return Err(e.into()),
            };
            if packet.track_id != self.track {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    if decoded.spec().rate() != self.rate {
                        return Err(Error("its rate changes".into()));
                    }
                    let channels = decoded.spec().channels().count().max(1);
                    let mut read = Vec::new();
                    decoded.copy_to_vec_interleaved(&mut read);
                    samples.clear();
                    map_channels(&read, channels, self.channels, samples);
                    return Ok(true);
                }
                // A damaged packet, as silence of its length, keeps what follows in its place.
                Err(SymError::DecodeError(why)) => {
                    log::debug!("silencing a damaged packet: {why}");
                    let frames = packet.dur.get();
                    if frames == 0 || frames > u64::from(self.rate) * 10 {
                        continue;
                    }
                    samples.clear();
                    samples.resize(frames as usize * self.channels, 0.0);
                    return Ok(true);
                }
                Err(SymError::IoError(e)) => {
                    log::debug!("skipping a packet: {e}");
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// A file to be made smaller: how, and how long it will be.
pub struct Plan {
    decoding: Decoding,
    encoder: opus::Encoder,
    quality: Quality,
    /// The channels encoded: one, or two.
    channels: usize,
    /// The music's samples a channel, at Opus's rate.
    samples: u64,
    pre_skip: u16,
    packets: u64,
    length: u64,
}

impl Plan {
    /// How `file`, of `extension`, is made smaller at `quality`; none when it would not be
    /// [`SMALLER_BY`] smaller, or is longer than a file made smaller is.
    pub fn new(
        file: File,
        extension: Option<&str>,
        quality: Quality,
    ) -> Result<Option<Self>, Error> {
        let length = file.metadata()?.len();
        let mut again = file.try_clone()?;
        let mut decoding = Decoding::open(file, extension)?;
        let frames = match decoding.frames {
            Some(frames) => frames,
            None => {
                // Not told: counted, and read again from the start.
                let mut frames = 0u64;
                let mut samples = Vec::new();
                while decoding.next(&mut samples)? {
                    frames += (samples.len() / decoding.channels) as u64;
                }
                again.seek(SeekFrom::Start(0))?;
                decoding = Decoding::open(again, extension)?;
                frames
            }
        };
        let seconds = frames as f64 / f64::from(decoding.rate);
        if seconds <= 0.0 || seconds > LONGEST_SECONDS {
            return Ok(None);
        }
        if length as f64 / seconds <= f64::from(quality.bytes_per_second()) * SMALLER_BY {
            return Ok(None);
        }
        Ok(Some(Self::of(decoding, quality, frames)?))
    }

    fn of(mut decoding: Decoding, quality: Quality, frames: u64) -> Result<Self, Error> {
        let channels = decoding.channels.min(2);
        // What it decodes, already in the channels encoded.
        decoding.channels = channels;
        let encoder = opus::Encoder::new(channels, quality.packet())?;
        let pre_skip = encoder.lookahead()?;
        let samples =
            (frames as f64 * f64::from(opus::RATE) / f64::from(decoding.rate)).round() as u64;
        let packets = (u64::from(pre_skip) + samples)
            .div_ceil(opus::FRAME as u64)
            .max(1);
        let length = ogg::single_length(head(channels, pre_skip, decoding.rate).len())
            + ogg::single_length(tags().len())
            + ogg::pages_length(packets, quality.packet());
        Ok(Self {
            decoding,
            encoder,
            quality,
            channels,
            samples,
            pre_skip,
            packets,
            length,
        })
    }

    /// The bytes the file will be.
    pub fn length(&self) -> u64 {
        self.length
    }

    pub fn quality(&self) -> Quality {
        self.quality
    }

    /// Encodes the file into `out`, a page at a time: [`Plan::length`] bytes.
    pub fn encode(mut self, out: impl Write) -> Result<(), Error> {
        let mut ogg = ogg::Writer::new(out, SERIAL);
        let head = head(self.channels, self.pre_skip, self.decoding.rate);
        ogg.page(&[&head], ogg::FIRST, 0)?;
        ogg.page(&[&tags()], 0, 0)?;

        let frame = opus::FRAME * self.channels;
        let mut resampler = (self.decoding.rate != opus::RATE)
            .then(|| RateConverter::new(self.decoding.rate, opus::RATE, self.channels));
        let mut decoded = Vec::new();
        let mut pending: Vec<f32> = Vec::new();
        let mut page: Vec<Vec<u8>> = Vec::with_capacity(ogg::PACKETS_PER_PAGE as usize);
        let mut done = 0u64;
        let mut ended = false;
        while done < self.packets {
            if pending.len() < frame && !ended {
                if self.decoding.next(&mut decoded)? {
                    match &mut resampler {
                        Some(resampler) => resampler.process(&decoded, &mut pending),
                        None => pending.extend_from_slice(&decoded),
                    }
                } else {
                    if let Some(resampler) = &mut resampler {
                        resampler.finish(&mut pending);
                    }
                    ended = true;
                }
                continue;
            }
            // After the music, silence, to the length reckoned.
            if pending.len() < frame {
                pending.resize(frame, 0.0);
            }
            page.push(self.encoder.encode(&pending[..frame])?);
            pending.drain(..frame);
            done += 1;
            let last = done == self.packets;
            if page.len() as u64 == ogg::PACKETS_PER_PAGE || last {
                let (flags, granule) = if last {
                    (ogg::LAST, u64::from(self.pre_skip) + self.samples)
                } else {
                    (0, done * opus::FRAME as u64)
                };
                let packets: Vec<&[u8]> = page.iter().map(Vec::as_slice).collect();
                ogg.page(&packets, flags, granule)?;
                page.clear();
            }
        }
        ogg.into_inner().flush()?;
        Ok(())
    }
}

/// The identification header (RFC 7845).
fn head(channels: usize, pre_skip: u16, rate: u32) -> Vec<u8> {
    let mut head = b"OpusHead".to_vec();
    head.push(1);
    head.push(channels as u8);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&rate.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes());
    head.push(0);
    head
}

/// The comment header: who encoded it, and nothing else; a client knows the track's tags.
fn tags() -> Vec<u8> {
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
    tags.extend_from_slice(VENDOR);
    tags.extend_from_slice(&0u32.to_le_bytes());
    tags
}

#[cfg(test)]
mod tests;
