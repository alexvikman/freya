//! Reading a Monkey's Audio file frame by frame.

use std::io::{Read, Seek, SeekFrom};

use symphonia::core::audio::{Channels, Position};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioCodecParameters;
use symphonia::core::codecs::audio::well_known::CODEC_ID_MONKEYS_AUDIO;
use symphonia::core::common::FourCc;
use symphonia::core::errors::{Error, Result, SeekErrorKind, seek_error};
use symphonia::core::formats::probe::{
    ProbeDataMatchSpec, ProbeFormatData, ProbeableFormat, Score, Scoreable,
};
use symphonia::core::formats::{
    FormatId, FormatInfo, FormatOptions, FormatReader, MediaInfo, SeekMode, SeekTo, SeekedTo, Track,
};
use symphonia::core::io::{MediaSource, MediaSourceStream, ReadBytes, ScopedStream};
use symphonia::core::meta::{Metadata, MetadataLog};
use symphonia::core::packet::Packet;
use symphonia::core::units::{Duration, TimeBase, Timestamp};

use super::decoder;
use super::header::Header;

const FORMAT_INFO: FormatInfo = FormatInfo {
    format: FormatId::new(FourCc::new(*b"MAC ")),
    short_name: "ape",
    long_name: "Monkey's Audio",
};

/// Bytes read past a frame's end. The range decoder looks a few bytes ahead, into the next
/// frame, which it then ignores.
const LOOKAHEAD: u64 = 4;
/// Larger frames are refused, so a damaged seek table cannot ask for gigabytes. Even
/// uncompressed, the largest frames encoders write are a few megabytes.
const MAX_FRAME_BYTES: u64 = 64 << 20;

pub struct ApeReader<'s> {
    reader: MediaSourceStream<'s>,
    header: Header,
    /// Where the header starts in the stream; frame offsets count from here.
    base: u64,
    /// Where the last frame ends, counted like the frame offsets.
    data_end: u64,
    tracks: Vec<Track>,
    media_info: MediaInfo,
    metadata: MetadataLog,
    /// The next packet: a frame, and where in it, in samples per channel.
    next_frame: u32,
    next_block: u32,
}

impl<'s> ApeReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let base = mss.pos();
        let header = Header::read(&mut mss)?;
        let data_end = match (header.data_end, mss.byte_len()) {
            (Some(end), _) => end,
            (None, Some(len)) => len.saturating_sub(base),
            (None, None) => u64::MAX,
        };

        let channels = match header.channels {
            1 => Channels::Positioned(Position::FRONT_CENTER),
            _ => Channels::Positioned(Position::FRONT_LEFT | Position::FRONT_RIGHT),
        };
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_MONKEYS_AUDIO)
            .with_sample_rate(header.sample_rate)
            .with_bits_per_sample(u32::from(header.bits_per_sample))
            .with_channels(channels)
            .with_extra_data(decoder::extra_data(header.version, header.compression));
        let total = header.total_blocks();
        let mut track = Track::new(0);
        track
            .with_codec_params(CodecParameters::Audio(params))
            .with_time_base(TimeBase::try_new(1, header.sample_rate).expect("non-zero rate"))
            .with_num_frames(total)
            .with_duration(Duration::new(total));

        Ok(Self {
            reader: mss,
            header,
            base,
            data_end,
            media_info: MediaInfo::from_track(&track),
            tracks: vec![track],
            // Tags at the end of the file are read by the probe and passed on here.
            metadata: opts.external_data.metadata.unwrap_or_default(),
            next_frame: 0,
            next_block: 0,
        })
    }

    /// Reads frame `index` into the data of its first packet.
    fn read_frame(&mut self, index: u32) -> Result<Box<[u8]>> {
        let offsets = &self.header.frame_offsets;
        let start = offsets[index as usize];
        let end = offsets
            .get(index as usize + 1)
            .copied()
            .unwrap_or(self.data_end);
        let invalid = Error::DecodeError("ape: invalid seek table");
        let (Some(frame_bytes), Some(from_first)) =
            (end.checked_sub(start), start.checked_sub(offsets[0]))
        else {
            return Err(invalid);
        };
        if frame_bytes > MAX_FRAME_BYTES {
            return Err(invalid);
        }
        let len = (frame_bytes + LOOKAHEAD) as usize;

        // The frames form one stream of 32-bit little-endian words, read from the most
        // significant byte down, starting at the first frame. A frame starts at any byte.
        let skip = (from_first % 4) as usize;
        let mut words = vec![0; (skip + len).next_multiple_of(4)];
        self.reader
            .seek(SeekFrom::Start(self.base + start - skip as u64))?;
        let mut filled = 0;
        while filled < words.len() {
            match self.reader.read(&mut words[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        if filled < skip + len - LOOKAHEAD as usize {
            return Err(Error::IoError(std::io::ErrorKind::UnexpectedEof.into()));
        }
        let bytes = (skip..skip + len).map(|i| words[i ^ 3]);
        Ok(decoder::first_part(self.header.frame_blocks(index), bytes))
    }
}

impl Scoreable for ApeReader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(255))
    }
}

impl ProbeableFormat<'_> for ApeReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(ApeReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[ProbeFormatData {
            spec: ProbeDataMatchSpec {
                extensions: &["ape"],
                mime_types: &["audio/ape", "audio/x-ape"],
                markers: &[b"MAC "],
            },
            info: FORMAT_INFO,
        }]
    }
}

impl FormatReader for ApeReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &FORMAT_INFO
    }

    fn media_info(&self) -> &MediaInfo {
        &self.media_info
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        let required = match to {
            SeekTo::Timestamp { ts, .. } => ts,
            SeekTo::Time { time, .. } => self.tracks[0]
                .time_base
                .and_then(|tb| tb.calc_timestamp(time))
                .ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?,
        };
        if required.is_negative() || required.get() as u64 > self.header.total_blocks() {
            return seek_error(SeekErrorKind::OutOfRange);
        }
        // Frames are independent: start from the one that holds the position.
        let blocks_per_frame = u64::from(self.header.blocks_per_frame);
        let frame =
            (required.get() as u64 / blocks_per_frame).min(u64::from(self.header.total_frames - 1));
        self.next_frame = frame as u32;
        self.next_block = 0;
        Ok(SeekedTo {
            track_id: 0,
            actual_ts: Timestamp::new((frame * blocks_per_frame) as i64),
            required_ts: required,
        })
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// The next part of a frame; see [`decoder::PART_BLOCKS`].
    fn next_packet(&mut self) -> Result<Option<Packet>> {
        let (index, offset) = (self.next_frame, self.next_block);
        if index >= self.header.total_frames {
            return Ok(None);
        }
        let data = if offset == 0 {
            self.read_frame(index)?
        } else {
            Box::default()
        };
        let frame_blocks = self.header.frame_blocks(index);
        let blocks = decoder::PART_BLOCKS.min(frame_blocks - offset);
        self.next_block += blocks;
        if self.next_block == frame_blocks {
            self.next_frame += 1;
            self.next_block = 0;
        }
        let ts = u64::from(index) * u64::from(self.header.blocks_per_frame) + u64::from(offset);
        Ok(Some(Packet::new(
            0,
            Timestamp::new(ts as i64),
            Duration::new(u64::from(blocks)),
            data,
        )))
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}
