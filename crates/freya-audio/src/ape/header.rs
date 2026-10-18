//! The header of a Monkey's Audio file: the stream's format and where each frame starts.

use std::io::{Read, Seek, SeekFrom};

use symphonia::core::errors::{Result, decode_error, unsupported_error};

/// Before 3980, the header was a fixed 32 bytes; since then a descriptor of its own size comes
/// first, then a header of its own size.
const DESCRIPTOR_VERSION: u16 = 3980;
/// The oldest version decoded. Earlier files (from before 2002) use other predictors.
pub const MIN_VERSION: u16 = 3950;

const FLAG_8_BIT: u16 = 1 << 0;
const FLAG_PEAK_LEVEL: u16 = 1 << 2;
const FLAG_24_BIT: u16 = 1 << 3;
const FLAG_SEEK_ELEMENTS: u16 = 1 << 4;
const FLAG_CREATE_WAV_HEADER: u16 = 1 << 5;
const FLAG_FLOATING_POINT: u16 = 1 << 12;

/// Frames longer than this are refused, so a damaged header cannot ask for gigabytes.
const MAX_BLOCKS_PER_FRAME: u32 = 10_000_000;

#[derive(Clone, Debug)]
pub struct Header {
    pub version: u16,
    /// 1000 (fast) to 5000 (insane): how many prediction filters were used.
    pub compression: u16,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub sample_rate: u32,
    pub blocks_per_frame: u32,
    pub final_frame_blocks: u32,
    pub total_frames: u32,
    /// Where each frame starts, counted from the start of the header.
    pub frame_offsets: Vec<u64>,
    /// Where the last frame ends, counted from the start of the header, if the header says.
    pub data_end: Option<u64>,
}

impl Header {
    /// Reads the header that `reader` is positioned at, the `MAC ` marker.
    pub fn read<R: Read + Seek>(reader: &mut R) -> Result<Self> {
        let start = reader.stream_position()?;
        let mut magic = [0; 4];
        reader.read_exact(&mut magic)?;
        if &magic != b"MAC " {
            return unsupported_error("ape: missing MAC marker");
        }
        let version = read_u16(reader)?;
        if version < MIN_VERSION {
            return unsupported_error("ape: files older than version 3.95 are not supported");
        }
        let header = if version >= DESCRIPTOR_VERSION {
            Self::read_current(reader, start, version)?
        } else {
            Self::read_old(reader, version)?
        };
        header.validate()?;
        Ok(header)
    }

    /// Version 3.98 and later: a descriptor, a header, the seek table, then the stored WAV
    /// header.
    fn read_current<R: Read + Seek>(reader: &mut R, start: u64, version: u16) -> Result<Self> {
        let _padding = read_u16(reader)?;
        let descriptor_bytes = read_u32(reader)?;
        let header_bytes = read_u32(reader)?;
        let seek_table_bytes = read_u32(reader)?;
        let wav_header_bytes = read_u32(reader)?;
        let frame_data_bytes = u64::from(read_u32(reader)?) | u64::from(read_u32(reader)?) << 32;

        reader.seek(SeekFrom::Start(start + u64::from(descriptor_bytes)))?;
        let compression = read_u16(reader)?;
        let flags = read_u16(reader)?;
        let blocks_per_frame = read_u32(reader)?;
        let final_frame_blocks = read_u32(reader)?;
        let total_frames = read_u32(reader)?;
        let bits_per_sample = read_u16(reader)?;
        let channels = read_u16(reader)?;
        let sample_rate = read_u32(reader)?;
        if flags & FLAG_FLOATING_POINT != 0 {
            return unsupported_error("ape: floating-point samples are not supported");
        }

        let seek_table = u64::from(descriptor_bytes) + u64::from(header_bytes);
        reader.seek(SeekFrom::Start(start + seek_table))?;
        let frame_offsets = read_seek_table(reader, seek_table_bytes / 4, total_frames)?;
        let frames_start = seek_table + u64::from(seek_table_bytes) + u64::from(wav_header_bytes);
        let Some(data_end) = frames_start.checked_add(frame_data_bytes) else {
            return decode_error("ape: frame data larger than any file");
        };

        Ok(Self {
            version,
            compression,
            channels,
            bits_per_sample,
            sample_rate,
            blocks_per_frame,
            final_frame_blocks,
            total_frames,
            frame_offsets,
            data_end: Some(data_end),
        })
    }

    /// Before version 3.98: a fixed header, optional fields that flags announce, the stored WAV
    /// header, then the seek table.
    fn read_old<R: Read + Seek>(reader: &mut R, version: u16) -> Result<Self> {
        let compression = read_u16(reader)?;
        let flags = read_u16(reader)?;
        let channels = read_u16(reader)?;
        let sample_rate = read_u32(reader)?;
        let wav_header_bytes = read_u32(reader)?;
        let _terminating_bytes = read_u32(reader)?;
        let total_frames = read_u32(reader)?;
        let final_frame_blocks = read_u32(reader)?;
        if flags & FLAG_PEAK_LEVEL != 0 {
            read_u32(reader)?;
        }
        let seek_entries = if flags & FLAG_SEEK_ELEMENTS != 0 {
            read_u32(reader)?
        } else {
            total_frames
        };
        if flags & FLAG_CREATE_WAV_HEADER == 0 {
            reader.seek(SeekFrom::Current(i64::from(wav_header_bytes)))?;
        }
        let frame_offsets = read_seek_table(reader, seek_entries, total_frames)?;

        let bits_per_sample = if flags & FLAG_8_BIT != 0 {
            8
        } else if flags & FLAG_24_BIT != 0 {
            24
        } else {
            16
        };
        Ok(Self {
            version,
            compression,
            channels,
            bits_per_sample,
            sample_rate,
            // Every version from 3.95 on, which is all that is decoded.
            blocks_per_frame: 73_728 * 4,
            final_frame_blocks,
            total_frames,
            frame_offsets,
            data_end: None,
        })
    }

    fn validate(&self) -> Result<()> {
        if !(1..=2).contains(&self.channels) {
            return unsupported_error("ape: only mono and stereo are supported");
        }
        if ![8, 16, 24].contains(&self.bits_per_sample) {
            return unsupported_error("ape: only 8, 16 and 24 bits per sample are supported");
        }
        if !(1000..=5000).contains(&self.compression) || !self.compression.is_multiple_of(1000) {
            return decode_error("ape: unknown compression level");
        }
        if self.sample_rate == 0 || self.total_frames == 0 {
            return decode_error("ape: empty or unfinished file");
        }
        if self.blocks_per_frame == 0
            || self.blocks_per_frame > MAX_BLOCKS_PER_FRAME
            || self.final_frame_blocks > self.blocks_per_frame
        {
            return decode_error("ape: invalid frame size");
        }
        Ok(())
    }

    /// Samples per channel in the whole stream.
    pub fn total_blocks(&self) -> u64 {
        u64::from(self.total_frames - 1) * u64::from(self.blocks_per_frame)
            + u64::from(self.final_frame_blocks)
    }

    /// Samples per channel in frame `index`.
    pub fn frame_blocks(&self, index: u32) -> u32 {
        if index + 1 == self.total_frames {
            self.final_frame_blocks
        } else {
            self.blocks_per_frame
        }
    }
}

/// Reads `entries` frame offsets, of which the first `total_frames` are used. The offsets are
/// 32 bits each and wrap around in files over 4 GB.
fn read_seek_table<R: Read>(reader: &mut R, entries: u32, total_frames: u32) -> Result<Vec<u64>> {
    if entries < total_frames {
        return decode_error("ape: seek table shorter than the number of frames");
    }
    // Grown as read, so a damaged count fails at the end of the file, not in an allocation.
    let mut offsets = Vec::with_capacity(total_frames.min(1 << 16) as usize);
    let mut wrap = 0;
    let mut previous = 0;
    for _ in 0..total_frames {
        let offset = read_u32(reader)?;
        if offset < previous {
            wrap += 1 << 32;
        }
        previous = offset;
        offsets.push(wrap + u64::from(offset));
    }
    Ok(offsets)
}

fn read_u16<R: Read>(reader: &mut R) -> Result<u16> {
    let mut bytes = [0; 2];
    reader.read_exact(&mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32<R: Read>(reader: &mut R) -> Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}
