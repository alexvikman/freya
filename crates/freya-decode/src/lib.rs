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
