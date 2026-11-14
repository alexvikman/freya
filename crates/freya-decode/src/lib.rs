//! Reading music files as samples: the formats and decoders Freya knows, Symphonia's with
//! Opus from libopus and Monkey's Audio of our own, and adapting what they give to another
//! number of channels or another rate.

mod ape;
mod convert;

use std::sync::LazyLock;

use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::formats::probe::Probe;

pub use crate::ape::{ApeDecoder, ApeReader};
pub use crate::convert::{RateConverter, map_channels};

/// The sample rates and channels of audio Freya decodes. A file may claim anything, and what
/// is set aside to decode and convert it follows what it claims: at a rate of a few hertz
/// the rate converter takes a gigabyte, as do tens of thousands of channels in the decoder.
/// No music is below 4 kHz (the lowest a codec has is 7,350 Hz), above 768 kHz, or in more
/// than 32 channels.
pub const LOWEST_RATE: u32 = 4_000;
pub const HIGHEST_RATE: u32 = 768_000;
pub const MOST_CHANNELS: usize = 32;

/// Whether audio at `rate` in `channels` is decoded: see [`LOWEST_RATE`].
pub fn decodes(rate: u32, channels: usize) -> bool {
    (LOWEST_RATE..=HIGHEST_RATE).contains(&rate) && (1..=MOST_CHANNELS).contains(&channels)
}

/// Symphonia's decoders, plus Opus from libopus and Monkey's Audio of our own, which Symphonia
/// does not have.
pub static CODECS: LazyLock<CodecRegistry> = LazyLock::new(|| {
    let mut codecs = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut codecs);
    #[cfg(feature = "opus")]
    codecs.register_audio_decoder::<symphonia_adapter_libopus::OpusDecoder>();
    codecs.register_audio_decoder::<ApeDecoder>();
    codecs
});

/// Symphonia's formats and tag readers, plus Monkey's Audio.
pub static PROBE: LazyLock<Probe> = LazyLock::new(|| {
    let mut probe = Probe::default();
    symphonia::default::register_enabled_formats(&mut probe);
    probe.register_format::<ApeReader<'_>>();
    probe
});

#[cfg(test)]
mod tests {
    use super::decodes;

    #[test]
    fn only_rates_and_channels_music_has_are_decoded() {
        assert!(decodes(7_350, 1), "the lowest rate a codec has");
        assert!(decodes(44_100, 2));
        assert!(decodes(768_000, 8));
        // A few hertz, which the rate converter would take a gigabyte for.
        assert!(!decodes(1, 2));
        assert!(!decodes(3_999, 2));
        assert!(!decodes(1_000_000, 2));
        assert!(!decodes(44_100, 0));
        assert!(!decodes(44_100, 65_535));
    }
}
