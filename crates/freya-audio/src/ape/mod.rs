//! Monkey's Audio (`.ape`), which Symphonia does not decode: a format reader and a decoder that
//! plug into it.
//!
//! Decodes files from version 3.95 (2002) on, mono or stereo, 8, 16 or 24 bits: what the
//! encoders of the last twenty years write. The format is defined by Matthew T. Ashland's
//! reference encoder (BSD-licensed since 2023); this follows it as the MIT-licensed
//! `ape-decoder` and `oxideav-ape` crates describe it, neither of which fits here: the first
//! cannot decode files before 3.99, the second wants the whole file in memory. Every frame
//! stores a CRC of its samples, which the decoder checks, so a mistake shows at once instead
//! of as noise.

mod decoder;
mod header;
mod predictor;
mod reader;
#[cfg(test)]
mod tests;

pub use decoder::ApeDecoder;
pub use reader::ApeReader;
