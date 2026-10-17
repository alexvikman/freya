//! Adapting decoded audio to the output device: channel mapping and resampling.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};

/// Maps interleaved samples from `src_ch` to `dst_ch` channels and appends them to `out`.
pub fn map_channels(input: &[f32], src_ch: usize, dst_ch: usize, out: &mut Vec<f32>) {
    if src_ch == dst_ch {
        out.extend_from_slice(input);
        return;
    }
    out.reserve(input.len() / src_ch * dst_ch);
    for frame in input.chunks_exact(src_ch) {
        match (src_ch, dst_ch) {
            // Mono to many: the same signal everywhere.
            (1, _) => out.extend(std::iter::repeat_n(frame[0], dst_ch)),
            // Down to mono: average of all channels.
            (_, 1) => out.push(frame.iter().sum::<f32>() / src_ch as f32),
            // More source channels than output (e.g. 5.1 to stereo): the first channels are
            // front left/right in all common layouts. The rest is mixed into all outputs at a
            // lower level.
            (_, _) if src_ch > dst_ch => {
                let extra: f32 = frame[dst_ch..].iter().sum::<f32>() / src_ch as f32;
                out.extend(frame[..dst_ch].iter().map(|s| s * 0.7 + extra * 0.3));
            }
            // Fewer source channels than output: pad with silence.
            _ => {
                out.extend_from_slice(frame);
                out.extend(std::iter::repeat_n(0.0, dst_ch - src_ch));
            }
        }
    }
}

const CHUNK_FRAMES: usize = 1024;

/// Resamples interleaved audio at a fixed ratio between two rates.
///
/// Compensates for the resampler's delay so output starts exactly at the first input sample,
/// and pads with silence in `finish` so the output length is exact.
pub struct RateConverter {
    resampler: Fft<f32>,
    channels: usize,
    ratio: f64,
    pending: Vec<f32>,
    scratch: Vec<f32>,
    delay_left: usize,
    frames_in: u64,
    frames_out: u64,
}

impl RateConverter {
    pub fn new(from: u32, to: u32, channels: usize) -> Self {
        let resampler = Fft::<f32>::new(
            from as usize,
            to as usize,
            CHUNK_FRAMES,
            channels,
            FixedSync::Input,
        )
        .expect("valid resampler parameters");
        let scratch = vec![0.0; resampler.output_frames_max() * channels];
        Self {
            delay_left: resampler.output_delay(),
            ratio: f64::from(to) / f64::from(from),
            resampler,
            channels,
            pending: Vec::new(),
            scratch,
            frames_in: 0,
            frames_out: 0,
        }
    }

    /// Forgets all buffered audio, e.g. after a seek.
    pub fn reset(&mut self) {
        self.resampler.reset();
        self.pending.clear();
        self.delay_left = self.resampler.output_delay();
        self.frames_in = 0;
        self.frames_out = 0;
    }

    /// Accepts interleaved samples and appends resampled audio to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.pending.extend_from_slice(input);
        self.frames_in += (input.len() / self.channels) as u64;
        let need = self.resampler.input_frames_next();
        let mut offset = 0;
        while self.pending.len() / self.channels - offset >= need {
            self.run_chunk(offset, None, out);
            offset += need;
        }
        self.pending.drain(..offset * self.channels);
    }

    /// Drains the resampler at the end of a stream.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        let target = (self.frames_in as f64 * self.ratio).round() as u64;
        while self.frames_out < target {
            let frames = self.pending.len() / self.channels;
            self.run_chunk(0, Some(frames), out);
            self.pending.clear();
        }
        // The last block may have produced a few frames too many (silence).
        let excess = self.frames_out.saturating_sub(target) as usize * self.channels;
        out.truncate(out.len().saturating_sub(excess));
        self.reset();
    }

    fn run_chunk(&mut self, offset_frames: usize, partial: Option<usize>, out: &mut Vec<f32>) {
        let need = self.resampler.input_frames_next();
        // For a partial final block the buffer must still hold a full block.
        if self.pending.len() < (offset_frames + need) * self.channels {
            self.pending
                .resize((offset_frames + need) * self.channels, 0.0);
        }
        let frames_avail = self.pending.len() / self.channels;
        let input = InterleavedSlice::new(&self.pending, self.channels, frames_avail)
            .expect("buffer has the right size");
        let scratch_frames = self.scratch.len() / self.channels;
        let mut output =
            InterleavedSlice::new_mut(&mut self.scratch, self.channels, scratch_frames)
                .expect("buffer has the right size");

        let mut indexing = Indexing::new().input_offset(offset_frames);
        if let Some(len) = partial {
            indexing = indexing.partial_len(len);
        }
        let (_, produced) = self
            .resampler
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .expect("resampling with correctly sized buffers");

        let skip = self.delay_left.min(produced);
        self.delay_left -= skip;
        self.frames_out += (produced - skip) as u64;
        out.extend_from_slice(&self.scratch[skip * self.channels..produced * self.channels]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_to_stereo_duplicates() {
        let mut out = Vec::new();
        map_channels(&[0.1, 0.2], 1, 2, &mut out);
        assert_eq!(out, [0.1, 0.1, 0.2, 0.2]);
    }

    #[test]
    fn stereo_to_mono_averages() {
        let mut out = Vec::new();
        map_channels(&[0.2, 0.4], 2, 1, &mut out);
        assert!((out[0] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn resampled_length_matches_ratio() {
        let mut conv = RateConverter::new(44_100, 48_000, 2);
        let input = vec![0.25f32; 44_100 * 2];
        let mut out = Vec::new();
        // Feed uneven blocks, like a decoder would.
        for chunk in input.chunks(1152 * 2) {
            conv.process(chunk, &mut out);
        }
        conv.finish(&mut out);
        assert_eq!(out.len(), 48_000 * 2);
        // A constant signal should stay constant once the initial transient has passed.
        let mid = &out[20_000..20_010];
        assert!(mid.iter().all(|s| (s - 0.25).abs() < 0.01), "{mid:?}");
    }
}
