//! Decoding Monkey's Audio frames: range-coded residuals, through the predictors, into
//! samples, checked against the CRC each frame stores.
//!
//! A frame holds up to 294912 samples per channel, almost seven seconds. The reader hands each
//! frame out in parts of [`PART_BLOCKS`], the first part with the frame's data, and the
//! decoder goes on through the frame part by part, so the work per packet stays small and
//! even. The CRC covers the whole frame, so a damaged frame is reported at its end, as the
//! reference decoder does when it streams.

use symphonia::core::audio::{
    AsGenericAudioBufferRef, Audio, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
};
use symphonia::core::codecs::CodecInfo;
use symphonia::core::codecs::audio::well_known::CODEC_ID_MONKEYS_AUDIO;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia::core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia::core::errors::{Error, Result, decode_error, unsupported_error};
use symphonia::core::packet::PacketRef;

use super::predictor::Predictor;

const CODECS: &[SupportedAudioCodec] = &[SupportedAudioCodec {
    id: CODEC_ID_MONKEYS_AUDIO,
    info: CodecInfo {
        short_name: "ape",
        long_name: "Monkey's Audio",
        profiles: &[],
    },
}];

/// Samples per channel in a packet, but for the last of a frame.
pub const PART_BLOCKS: u32 = 4096;

/// Files from this version on code residuals with an adaptive pivot instead of Rice-like `k`.
const PIVOT_VERSION: u16 = 3990;

/// Frame flags that replace coded data.
const SILENT_LEFT: u32 = 1;
const SILENT_RIGHT: u32 = 2;
const PSEUDO_STEREO: u32 = 4;

/// The parameters a frame decoder needs from the file header, stored in the codec
/// parameters' extra data: version and compression level, both little-endian `u16`.
pub fn extra_data(version: u16, compression: u16) -> Box<[u8]> {
    let [v0, v1] = version.to_le_bytes();
    let [c0, c1] = compression.to_le_bytes();
    Box::new([v0, v1, c0, c1])
}

/// The data of a frame's first packet: its length in samples per channel, little-endian,
/// then its bytes in the order the decoder reads them.
pub fn first_part(frame_blocks: u32, bytes: impl Iterator<Item = u8>) -> Box<[u8]> {
    frame_blocks
        .to_le_bytes()
        .into_iter()
        .chain(bytes)
        .collect()
}

pub struct ApeDecoder {
    params: AudioCodecParameters,
    version: u16,
    bits: u32,
    predictors: Vec<Predictor>,
    /// Set for good once a 24-bit frame only checks out with an early encoder's rounding.
    interim: bool,
    /// The frame being decoded, until its last part.
    frame: Option<Frame>,
    coder: RangeDecoder,
    buf: AudioBuffer<i32>,
}

/// What is left to decode of a frame, and what it should check out to.
struct Frame {
    content: Content,
    stored_crc: u32,
    crc: Crc32,
    blocks_left: usize,
    residuals: [Residual; 2],
    /// The last mid sample, which the difference channel is predicted from.
    last_x: i32,
}

enum Content {
    /// All channels silent: nothing is coded.
    Silent,
    /// One channel coded, played on all.
    Single,
    Stereo,
    /// Damaged: the rest of the frame is silence.
    Failed,
}

impl ApeDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_MONKEYS_AUDIO {
            return unsupported_error("ape: invalid codec");
        }
        let Some(&[v0, v1, c0, c1]) = params.extra_data.as_deref() else {
            return unsupported_error("ape: missing extra data");
        };
        let (version, compression) = (u16::from_le_bytes([v0, v1]), u16::from_le_bytes([c0, c1]));
        let (Some(rate), Some(channels), Some(bits)) = (
            params.sample_rate,
            params.channels.clone(),
            params.bits_per_sample,
        ) else {
            return unsupported_error("ape: incomplete codec parameters");
        };
        let predictors = (0..channels.count())
            .map(|_| Predictor::new(compression, version))
            .collect();
        Ok(Self {
            params: params.clone(),
            version,
            bits,
            predictors,
            interim: false,
            frame: None,
            coder: RangeDecoder::default(),
            buf: AudioBuffer::new(AudioSpec::new(rate, channels), PART_BLOCKS as usize),
        })
    }

    /// Reads a frame's header from its first packet and gets ready to decode it.
    fn start_frame(&mut self, data: &[u8]) -> Result<()> {
        let Some((blocks, bytes)) = data.split_first_chunk() else {
            return decode_error("ape: frame too short");
        };
        self.coder.data.clear();
        self.coder.data.extend_from_slice(bytes);
        self.coder.pos = 0;
        let stored = self.coder.read_u32();
        let (stored_crc, flags) = if stored & 0x8000_0000 != 0 {
            (stored & 0x7FFF_FFFF, self.coder.read_u32())
        } else {
            (stored, 0)
        };

        let mono = self.predictors.len() == 1;
        let content = if mono && flags & SILENT_LEFT != 0
            || flags & (SILENT_LEFT | SILENT_RIGHT) == SILENT_LEFT | SILENT_RIGHT
        {
            Content::Silent
        } else if mono || flags & PSEUDO_STEREO != 0 {
            Content::Single
        } else {
            Content::Stereo
        };
        if !matches!(content, Content::Silent) {
            self.coder.start();
            self.predictors.iter_mut().for_each(Predictor::reset);
        }
        self.frame = Some(Frame {
            content,
            stored_crc,
            crc: Crc32::new(),
            blocks_left: u32::from_le_bytes(*blocks) as usize,
            residuals: [Residual::new(self.version), Residual::new(self.version)],
            last_x: 0,
        });
        Ok(())
    }

    /// Decodes the next `blocks` samples per channel of the frame into `buf`.
    fn decode_part(&mut self, blocks: usize) -> Result<()> {
        let Some(frame) = &mut self.frame else {
            return decode_error("ape: packet outside a frame");
        };
        if blocks > frame.blocks_left {
            return decode_error("ape: packet runs past its frame");
        }
        if self.buf.capacity() < blocks {
            self.buf.grow_capacity(blocks);
        }
        self.buf.clear();
        self.buf.render_uninit(Some(blocks));

        let decoded = decode_samples(
            frame,
            &mut self.coder,
            &mut self.predictors,
            &mut self.buf,
            blocks,
            self.interim,
        );
        if let Err(e) = decoded {
            frame.content = Content::Failed;
            return Err(e);
        }
        let failed = matches!(frame.content, Content::Failed);
        if !failed {
            update_crc(&mut frame.crc, &self.buf, self.bits);
        }
        scale(&mut self.buf, self.bits);
        frame.blocks_left -= blocks;
        if frame.blocks_left > 0 || failed {
            return Ok(());
        }

        let frame = self.frame.take().expect("frame");
        if frame.crc.finish() >> 1 == frame.stored_crc {
            return Ok(());
        }
        // A few early 24-bit encoders kept more precision in the predictor. This frame was
        // decoded without it, but the rest of the file will be right.
        if self.bits == 24 && !self.interim {
            log::warn!("ape: CRC mismatch, decoding on with the rounding of early encoders");
            self.interim = true;
        }
        decode_error("ape: frame CRC mismatch")
    }
}

/// Decodes `blocks` samples per channel of `frame` into `buf`, as 32-bit values that still
/// need to be brought to the bit depth's range.
fn decode_samples(
    frame: &mut Frame,
    coder: &mut RangeDecoder,
    predictors: &mut [Predictor],
    buf: &mut AudioBuffer<i32>,
    blocks: usize,
    interim: bool,
) -> Result<()> {
    match frame.content {
        Content::Silent | Content::Failed => {
            buf.iter_planes_mut().for_each(|plane| plane.fill(0));
        }
        Content::Single => {
            let residual = &mut frame.residuals[0];
            for i in 0..blocks {
                let x = predictors[0].decompress(residual.decode(coder)?, 0, interim);
                for plane in buf.iter_planes_mut() {
                    plane[i] = x;
                }
            }
        }
        Content::Stereo => {
            let [x_predictor, y_predictor] = predictors else {
                return decode_error("ape: stereo frame in a mono file");
            };
            let [x_residual, y_residual] = &mut frame.residuals;
            let (left, right) = buf.plane_pair_mut(0, 1).expect("stereo buffer");
            for i in 0..blocks {
                let y = y_residual.decode(coder)?;
                let x = x_residual.decode(coder)?;
                let y = y_predictor.decompress(y, frame.last_x, interim);
                let x = x_predictor.decompress(x, y, interim);
                frame.last_x = x;
                // X is the mid channel, Y the difference.
                left[i] = x.wrapping_sub(y / 2);
                right[i] = left[i].wrapping_add(y);
            }
        }
    }
    Ok(())
}

/// Adds the samples to the frame's CRC, as the source WAV file stored them.
fn update_crc(crc: &mut Crc32, buf: &AudioBuffer<i32>, bits: u32) {
    let planes: Vec<&[i32]> = buf.iter_planes().collect();
    for i in 0..buf.frames() {
        for plane in &planes {
            let sample = plane[i];
            match bits {
                // 8-bit WAV is unsigned.
                8 => crc.update(&[(sample as u8).wrapping_add(128)]),
                16 => crc.update(&(sample as i16).to_le_bytes()),
                _ => crc.update(&sample.to_le_bytes()[..3]),
            }
        }
    }
}

/// Brings the samples up to 32 bits. Bits beyond the bit depth, which the stored samples did
/// not have either, are shifted out.
fn scale(buf: &mut AudioBuffer<i32>, bits: u32) {
    let shift = 32 - bits;
    buf.apply(|sample| sample << shift);
}

impl AudioDecoder for ApeDecoder {
    fn reset(&mut self) {
        // After a seek, the reader starts at the beginning of a frame.
        self.frame = None;
    }

    fn codec_info(&self) -> &CodecInfo {
        &CODECS[0].info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        let mut decoded = Ok(());
        if !packet.data.is_empty() {
            decoded = self.start_frame(packet.data);
        }
        if decoded.is_ok() {
            decoded = self.decode_part(packet.dur.get() as usize);
        }
        if let Err(e) = decoded {
            self.buf.clear();
            return Err(e);
        }
        Ok(self.buf.as_generic_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        Default::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for ApeDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(Self::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        CODECS
    }
}

const BOTTOM_VALUE: u32 = 1 << 23;

/// The range decoder the residuals are coded with, over the bytes of one frame. Past their
/// end, it reads zeros.
#[derive(Default)]
struct RangeDecoder {
    data: Vec<u8>,
    pos: usize,
    low: u32,
    range: u32,
    buffer: u32,
}

impl RangeDecoder {
    fn read_u8(&mut self) -> u32 {
        let byte = self.data.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        u32::from(byte)
    }

    fn read_u32(&mut self) -> u32 {
        (0..4).fold(0, |value, _| value << 8 | self.read_u8())
    }

    /// Starts decoding after the frame's header.
    fn start(&mut self) {
        // The first byte is not part of the coded data.
        self.read_u8();
        self.buffer = self.read_u8();
        self.low = self.buffer >> 1;
        self.range = 1 << 7;
    }

    fn corrupt() -> Error {
        Error::DecodeError("ape: corrupt range coder state")
    }

    fn normalize(&mut self) -> Result<()> {
        while self.range <= BOTTOM_VALUE {
            if self.range == 0 {
                return Err(Self::corrupt());
            }
            self.buffer = self.buffer << 8 | self.read_u8();
            self.low = self.low << 8 | (self.buffer >> 1) & 0xFF;
            self.range <<= 8;
        }
        Ok(())
    }

    /// A value from a uniform distribution of `n` values.
    fn decode_below(&mut self, n: u32) -> Result<u32> {
        self.normalize()?;
        self.range /= n;
        if self.range == 0 {
            return Err(Self::corrupt());
        }
        let value = self.low / self.range;
        self.low -= value * self.range;
        Ok(value)
    }

    fn decode_bits(&mut self, bits: u32) -> Result<u32> {
        self.decode_below(1 << bits)
    }

    /// A symbol of the model whose cumulative frequencies, out of 65536, are `counts`.
    fn decode_symbol(&mut self, counts: &[u32; 65]) -> Result<u32> {
        self.normalize()?;
        self.range >>= 16;
        let frequency = self.low / self.range;
        if frequency >= 1 << 16 {
            return Err(Self::corrupt());
        }
        let symbol = counts.partition_point(|&count| count <= frequency) - 1;
        self.low -= self.range * counts[symbol];
        self.range *= counts[symbol + 1] - counts[symbol];
        Ok(symbol as u32)
    }
}

/// Cumulative frequencies of the overflow symbol, before 3.99 and after.
const COUNTS_3970: [u32; 65] = [
    0, 14824, 28224, 39348, 47855, 53994, 58171, 60926, 62682, 63786, 64463, 64878, 65126, 65276,
    65365, 65419, 65450, 65469, 65480, 65487, 65491, 65493, 65494, 65495, 65496, 65497, 65498,
    65499, 65500, 65501, 65502, 65503, 65504, 65505, 65506, 65507, 65508, 65509, 65510, 65511,
    65512, 65513, 65514, 65515, 65516, 65517, 65518, 65519, 65520, 65521, 65522, 65523, 65524,
    65525, 65526, 65527, 65528, 65529, 65530, 65531, 65532, 65533, 65534, 65535, 65536,
];
const COUNTS_3990: [u32; 65] = [
    0, 19578, 36160, 48417, 56323, 60899, 63265, 64435, 64971, 65232, 65351, 65416, 65447, 65466,
    65476, 65482, 65485, 65488, 65490, 65491, 65492, 65493, 65494, 65495, 65496, 65497, 65498,
    65499, 65500, 65501, 65502, 65503, 65504, 65505, 65506, 65507, 65508, 65509, 65510, 65511,
    65512, 65513, 65514, 65515, 65516, 65517, 65518, 65519, 65520, 65521, 65522, 65523, 65524,
    65525, 65526, 65527, 65528, 65529, 65530, 65531, 65532, 65533, 65534, 65535, 65536,
];
/// The overflow symbol that says a wider value follows.
const ESCAPE: u32 = 63;

/// The adaptive state of one channel's residuals: `k_sum` follows the average magnitude,
/// `k` its number of bits.
struct Residual {
    old: bool,
    /// Before 3.91, wide values were not split in two.
    split_wide: bool,
    k: u32,
    k_sum: u32,
}

impl Residual {
    fn new(version: u16) -> Self {
        Self {
            old: version < PIVOT_VERSION,
            split_wide: version >= 3910,
            k: 10,
            k_sum: 1 << 14,
        }
    }

    fn decode(&mut self, coder: &mut RangeDecoder) -> Result<i32> {
        let value = if self.old {
            self.decode_old(coder)?
        } else {
            self.decode_pivot(coder)?
        };
        self.k_sum = self
            .k_sum
            .wrapping_add(value.div_ceil(2))
            .wrapping_sub(self.k_sum.wrapping_add(16) >> 5);
        // k follows k_sum between 2^(k+4) and 2^(k+5).
        if self.k > 0 && self.k_sum < 1 << (self.k + 4) {
            self.k -= 1;
        } else if self.k < 27 && self.k_sum >= 1 << (self.k + 5) {
            self.k += 1;
        }
        // Odd values are positive, even ones zero or negative.
        Ok(if value & 1 != 0 {
            (value >> 1) as i32 + 1
        } else {
            -((value >> 1) as i32)
        })
    }

    /// Files from 3.99 on: the value divided by a pivot that follows the average magnitude.
    fn decode_pivot(&self, coder: &mut RangeDecoder) -> Result<u32> {
        let mut pivot = (self.k_sum / 32).max(1);
        let overflow = loop {
            let symbol = coder.decode_symbol(&COUNTS_3990)?;
            if symbol != ESCAPE {
                break symbol;
            }
            let overflow = coder.decode_bits(16)? << 16 | coder.decode_bits(16)?;
            // An escaped 1 says the pivot was too small and a fixed one is used instead.
            if overflow != 1 {
                break overflow;
            }
            pivot = 1 << 15;
        };
        let base = if pivot < 1 << 16 {
            coder.decode_below(pivot)?
        } else {
            // The range coder divides at most 16 bits at a time.
            let split = 1 << (32 - pivot.leading_zeros() - 16);
            let high = coder.decode_below(pivot / split + 1)?;
            let low = coder.decode_below(split)?;
            high.wrapping_mul(split).wrapping_add(low)
        };
        Ok(base.wrapping_add(overflow.wrapping_mul(pivot)))
    }

    /// Before 3.99: an overflow count, then `k - 1` low bits.
    fn decode_old(&self, coder: &mut RangeDecoder) -> Result<u32> {
        let symbol = coder.decode_symbol(&COUNTS_3970)?;
        let (overflow, bits) = if symbol == ESCAPE {
            (0, coder.decode_bits(5)?)
        } else {
            (symbol, self.k.saturating_sub(1))
        };
        let low = if bits <= 16 || !self.split_wide {
            coder.decode_bits(bits)?
        } else {
            coder.decode_bits(16)? | coder.decode_bits(bits - 16)? << 16
        };
        Ok(low.wrapping_add(overflow.wrapping_shl(bits)))
    }
}

/// CRC-32 as in zip and PNG, computed a byte at a time.
struct Crc32(u32);

const CRC_TABLE: [u32; 256] = {
    let mut table = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ crc >> 1
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

impl Crc32 {
    fn new() -> Self {
        Self(!0)
    }

    fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = CRC_TABLE[((self.0 ^ u32::from(byte)) & 0xFF) as usize] ^ self.0 >> 8;
        }
    }

    fn finish(&self) -> u32 {
        !self.0
    }
}
