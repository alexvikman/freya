//! libopus's encoder, at a constant bitrate: every packet as long as the next.

#![allow(unsafe_code, reason = "libopus's encoder, through its C interface")]

use std::ffi::c_int;
use std::ptr::NonNull;

use opusic_sys::{
    OPUS_APPLICATION_AUDIO, OPUS_GET_LOOKAHEAD_REQUEST, OPUS_OK, OPUS_SET_BITRATE_REQUEST,
    OPUS_SET_COMPLEXITY_REQUEST, OPUS_SET_SIGNAL_REQUEST, OPUS_SET_VBR_REQUEST, OPUS_SIGNAL_MUSIC,
    OpusEncoder, opus_encode_float, opus_encoder_create, opus_encoder_ctl, opus_encoder_destroy,
    opus_packet_pad,
};

/// The rate Opus is encoded at.
pub const RATE: u32 = 48_000;
/// A packet's samples a channel: 20 ms.
pub const FRAME: usize = 960;
/// How hard the encoder works, of 10: somewhat faster than the hardest, for little.
const COMPLEXITY: c_int = 8;

pub struct Encoder {
    state: NonNull<OpusEncoder>,
    channels: usize,
    packet: usize,
}

// SAFETY: the encoder's state is its own, used by one thread at a time through `&mut self`.
unsafe impl Send for Encoder {}

impl Encoder {
    /// An encoder of `channels`, one or two, whose packets are `packet` bytes each.
    pub fn new(channels: usize, packet: usize) -> Result<Self, String> {
        let mut error = 0;
        // SAFETY: valid arguments, and room for the error.
        let state = unsafe {
            opus_encoder_create(
                RATE as i32,
                channels as c_int,
                OPUS_APPLICATION_AUDIO,
                &mut error,
            )
        };
        let state = NonNull::new(state)
            .filter(|_| error == OPUS_OK)
            .ok_or_else(|| format!("libopus could not make an encoder ({error})"))?;
        let encoder = Self {
            state,
            channels,
            packet,
        };
        let bitrate = (packet * 8 * RATE as usize / FRAME) as c_int;
        encoder.set(OPUS_SET_BITRATE_REQUEST, bitrate)?;
        encoder.set(OPUS_SET_VBR_REQUEST, 0)?;
        encoder.set(OPUS_SET_COMPLEXITY_REQUEST, COMPLEXITY)?;
        encoder.set(OPUS_SET_SIGNAL_REQUEST, OPUS_SIGNAL_MUSIC)?;
        Ok(encoder)
    }

    fn set(&self, request: c_int, value: c_int) -> Result<(), String> {
        // SAFETY: a request that takes one int.
        let done = unsafe { opus_encoder_ctl(self.state.as_ptr(), request, value) };
        if done == OPUS_OK {
            Ok(())
        } else {
            Err(format!(
                "libopus refused setting {request} to {value} ({done})"
            ))
        }
    }

    /// The samples at the start a decoder skips: the encoder's lookahead.
    pub fn lookahead(&self) -> Result<u16, String> {
        let mut samples: i32 = 0;
        // SAFETY: a request that writes one int, where it points.
        let done = unsafe {
            opus_encoder_ctl(
                self.state.as_ptr(),
                OPUS_GET_LOOKAHEAD_REQUEST,
                &mut samples as *mut i32,
            )
        };
        if done != OPUS_OK {
            return Err(format!("libopus did not tell its lookahead ({done})"));
        }
        u16::try_from(samples).map_err(|_| format!("a lookahead of {samples}"))
    }

    /// One packet of the [`FRAME`] interleaved samples of `pcm`, padded to its full length.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, String> {
        assert_eq!(pcm.len(), FRAME * self.channels);
        let mut packet = vec![0u8; self.packet];
        // SAFETY: a frame of samples, and room for a packet.
        let written = unsafe {
            opus_encode_float(
                self.state.as_ptr(),
                pcm.as_ptr(),
                FRAME as c_int,
                packet.as_mut_ptr(),
                self.packet as i32,
            )
        };
        if written <= 0 {
            return Err(format!("libopus could not encode ({written})"));
        }
        if (written as usize) < self.packet {
            // SAFETY: a packet of `written` bytes, in room for `self.packet`.
            let done = unsafe { opus_packet_pad(packet.as_mut_ptr(), written, self.packet as i32) };
            if done != OPUS_OK {
                return Err(format!("libopus could not pad a packet ({done})"));
            }
        }
        Ok(packet)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: made by opus_encoder_create, destroyed once.
        unsafe { opus_encoder_destroy(self.state.as_ptr()) };
    }
}
