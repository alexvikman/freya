//! The audio callback side: pulls samples from the ring buffer into the device.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use rtrb::{Consumer, Producer};

use crate::Error;
use crate::eq::Equalizer;
use crate::state::Shared;

/// Frames of scratch space allocated up front; enough for any common device buffer.
const SCRATCH_FRAMES: usize = 16_384;

/// Everything the audio callback needs. Lock-free, and allocation-free unless a device asks
/// for an unusually large buffer.
pub(crate) struct Renderer {
    consumer: Consumer<f32>,
    shared: Arc<Shared>,
    channels: usize,
    /// Gains applied at the end of the previous buffer, to ramp towards new targets.
    gain: (f32, f32),
    eq: Equalizer,
    eq_version: Option<u64>,
    scratch: Vec<f32>,
    /// Played samples, mixed to mono, for visualization.
    visualization: Producer<f32>,
    mono: Vec<f32>,
}

impl Renderer {
    pub fn new(
        consumer: Consumer<f32>,
        shared: Arc<Shared>,
        rate: u32,
        channels: usize,
        visualization: Producer<f32>,
    ) -> Self {
        let gain = shared.gains();
        Self {
            consumer,
            shared,
            channels,
            gain,
            eq: Equalizer::new(rate, channels),
            eq_version: None,
            scratch: vec![0.0; SCRATCH_FRAMES * channels],
            visualization,
            mono: vec![0.0; SCRATCH_FRAMES],
        }
    }

    pub fn render<T: SizedSample + FromSample<f32>>(&mut self, out: &mut [T]) {
        if self.shared.flush.load(Ordering::Acquire) {
            let n = self.consumer.slots();
            if let Ok(chunk) = self.consumer.read_chunk(n) {
                chunk.commit_all();
            }
            self.shared.flush.store(false, Ordering::Release);
        }

        let version = self.shared.eq.version();
        if self.eq_version != Some(version) {
            self.eq.configure(&self.shared.eq.get());
            self.eq_version = Some(version);
        }

        let ch = self.channels;
        let wanted = out.len() / ch;
        let frames = if self.shared.paused.load(Ordering::Relaxed) {
            0
        } else {
            wanted.min(self.consumer.slots() / ch)
        };
        if !self.shared.paused.load(Ordering::Relaxed) && frames < wanted {
            self.shared
                .starved
                .fetch_add((wanted - frames) as u64, Ordering::Relaxed);
        }

        if frames > 0 {
            let n = frames * ch;
            if self.scratch.len() < n {
                self.scratch.resize(n, 0.0);
            }
            let samples = &mut self.scratch[..n];
            let chunk = self.consumer.read_chunk(n).expect("slots checked above");
            let (a, b) = chunk.as_slices();
            samples[..a.len()].copy_from_slice(a);
            samples[a.len()..].copy_from_slice(b);
            chunk.commit_all();

            self.eq.process(samples);

            if self.shared.visualize.load(Ordering::Relaxed) {
                if self.mono.len() < frames {
                    self.mono.resize(frames, 0.0);
                }
                for (m, frame) in self.mono.iter_mut().zip(samples.chunks_exact(ch)) {
                    *m = frame.iter().sum::<f32>() / ch as f32;
                }
                // When the UI falls behind, the rest is dropped; it only wants recent audio.
                let _ = self.visualization.push_partial_slice(&self.mono[..frames]);
            }

            let (from, to) = (self.gain, self.shared.gains());
            let step = 1.0 / frames as f32;
            for (i, (dst, &src)) in out.iter_mut().zip(samples.iter()).enumerate() {
                let t = (i / ch) as f32 * step;
                let gain = match i % ch {
                    0 => from.0 + (to.0 - from.0) * t,
                    1 => from.1 + (to.1 - from.1) * t,
                    _ => from.0.max(from.1) + (to.0.max(to.1) - from.0.max(from.1)) * t,
                };
                *dst = T::from_sample(src * gain);
            }
            self.gain = to;
            self.shared
                .consumed
                .fetch_add(frames as u64, Ordering::AcqRel);
        }

        out[frames * ch..].fill(T::EQUILIBRIUM);
    }
}

/// The device format the engine should produce.
pub(crate) struct DeviceFormat {
    pub sample_rate: u32,
    pub channels: usize,
}

pub(crate) struct Output {
    device: cpal::Device,
    config: cpal::SupportedStreamConfig,
}

impl Output {
    pub fn open_default() -> Result<Self, Error> {
        let (device, config) = default_device()?;
        log::info!("output: {device} {config:?}");
        Ok(Self { device, config })
    }

    /// The default output device, in `format`: for a new device when the one the engine plays
    /// for is gone, as the engine keeps its rate and channels.
    pub fn open_default_in(format: &DeviceFormat) -> Result<Self, Error> {
        let (device, config) = default_device()?;
        let channels = u16::try_from(format.channels).unwrap_or(0);
        let config = if config.sample_rate() == format.sample_rate && config.channels() == channels
        {
            Some(config)
        } else {
            let ranges = device
                .supported_output_configs()
                .map_err(|e| Error::Output(e.to_string()))?;
            matching_config(ranges, format.sample_rate, channels)
        }
        .ok_or_else(|| {
            Error::Output(format!(
                "{device} cannot play {} Hz in {} channels",
                format.sample_rate, format.channels
            ))
        })?;
        log::info!("output: {device} {config:?}");
        Ok(Self { device, config })
    }

    pub fn format(&self) -> DeviceFormat {
        DeviceFormat {
            sample_rate: self.config.sample_rate(),
            channels: usize::from(self.config.channels()),
        }
    }

    /// Starts a stream that `renderer` plays into. When the stream is dropped, or cannot be
    /// started, `handoff` gets the renderer back.
    pub fn start(self, renderer: Renderer, handoff: &Handoff) -> Result<cpal::Stream, Error> {
        let format = self.config.sample_format();
        let preferred = buffer_size(self.config.buffer_size());
        let config: cpal::StreamConfig = self.config.into();
        let shared = Arc::clone(&renderer.shared);
        let mut renderer = Some(renderer);
        // Some devices list the preferred size and then refuse it at the rate they run at.
        let sizes = [preferred, cpal::BufferSize::Default];
        let mut result = Err(Error::Output("no buffer size".into()));
        for (i, size) in sizes.iter().enumerate() {
            if i > 0 && *size == preferred {
                break;
            }
            let Some(renderer) = renderer.take().or_else(|| handoff.take()) else {
                break;
            };
            let config = cpal::StreamConfig {
                buffer_size: *size,
                ..config
            };
            let back = Handback {
                renderer: Some(renderer),
                back: handoff.sender.clone(),
            };
            result = match format {
                SampleFormat::F32 => build::<f32>(&self.device, config, back, &shared),
                SampleFormat::F64 => build::<f64>(&self.device, config, back, &shared),
                SampleFormat::I16 => build::<i16>(&self.device, config, back, &shared),
                SampleFormat::I32 => build::<i32>(&self.device, config, back, &shared),
                SampleFormat::U16 => build::<u16>(&self.device, config, back, &shared),
                SampleFormat::U8 => build::<u8>(&self.device, config, back, &shared),
                other => return Err(Error::Output(format!("unsupported sample format {other}"))),
            };
            match &result {
                Ok(_) => break,
                Err(e) => log::warn!("output with buffer {size:?}: {e}"),
            }
        }
        let stream = result?;
        stream.play().map_err(|e| Error::Output(e.to_string()))?;
        Ok(stream)
    }
}

fn default_device() -> Result<(cpal::Device, cpal::SupportedStreamConfig), Error> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| Error::Output("no output device".into()))?;
    let config = device
        .default_output_config()
        .map_err(|e| Error::Output(e.to_string()))?;
    Ok((device, config))
}

/// A configuration of `ranges` at `rate` with `channels`, in 32-bit float where offered.
fn matching_config(
    ranges: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
    rate: u32,
    channels: u16,
) -> Option<cpal::SupportedStreamConfig> {
    let mut found: Vec<_> = ranges
        .filter(|r| r.channels() == channels)
        .filter_map(|r| r.try_with_sample_rate(rate))
        .collect();
    found.sort_by_key(|c| c.sample_format() != SampleFormat::F32);
    found.into_iter().next()
}

/// Where a renderer comes back to when the stream that played it is gone, so the next
/// stream can take it over, with the ring buffer the engine fills.
pub(crate) struct Handoff {
    sender: crossbeam_channel::Sender<Renderer>,
    receiver: crossbeam_channel::Receiver<Renderer>,
}

impl Handoff {
    pub fn new() -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        Self { sender, receiver }
    }

    /// The renderer of a stream just dropped. Backends drop the callback with the stream, or
    /// soon after on their own thread.
    pub fn take(&self) -> Option<Renderer> {
        self.receiver.recv_timeout(Duration::from_secs(1)).ok()
    }
}

/// Owns the renderer in the audio callback, and sends it back when the callback is dropped.
struct Handback {
    renderer: Option<Renderer>,
    back: crossbeam_channel::Sender<Renderer>,
}

impl Drop for Handback {
    fn drop(&mut self) {
        if let Some(renderer) = self.renderer.take() {
            let _ = self.back.try_send(renderer);
        }
    }
}

/// Frames per audio callback, where the device allows it: 21 ms at 48 kHz. Devices often
/// default to 512, which wakes the audio thread about 94 times a second; this halves that,
/// and still hands the visualization new samples for every frame it draws.
const BUFFER_FRAMES: u32 = 1024;

fn buffer_size(supported: &cpal::SupportedBufferSize) -> cpal::BufferSize {
    match supported {
        cpal::SupportedBufferSize::Range { min, max } if (*min..=*max).contains(&BUFFER_FRAMES) => {
            cpal::BufferSize::Fixed(BUFFER_FRAMES)
        }
        _ => cpal::BufferSize::Default,
    }
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut back: Handback,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, Error> {
    let shared = Arc::clone(shared);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                if let Some(renderer) = &mut back.renderer {
                    renderer.render(data);
                }
            },
            move |err| {
                log::error!("audio stream: {err}");
                if is_lost(err.kind()) {
                    shared.device_lost.store(true, Ordering::Release);
                }
            },
            None,
        )
        .map_err(|e| Error::Output(e.to_string()))
}

/// Whether a stream that reported `kind` has stopped for good and must be built again.
fn is_lost(kind: cpal::ErrorKind) -> bool {
    matches!(
        kind,
        cpal::ErrorKind::DeviceNotAvailable
            | cpal::ErrorKind::StreamInvalidated
            | cpal::ErrorKind::HostUnavailable
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtrb::RingBuffer;

    #[test]
    fn applies_balance_and_counts_frames() {
        let (mut producer, consumer) = RingBuffer::new(64);
        let shared = Arc::new(Shared::new(48_000));
        shared.set_balance(-1.0); // fully left
        let mut renderer = Renderer::new(
            consumer,
            Arc::clone(&shared),
            48_000,
            2,
            RingBuffer::new(64).0,
        );
        producer.push_entire_slice(&[0.5; 8]).unwrap();

        let mut out = [1.0f32; 12];
        renderer.render(&mut out);
        assert_eq!(&out[..8], &[0.5, 0.0, 0.5, 0.0, 0.5, 0.0, 0.5, 0.0]);
        // Underrun is filled with silence and not counted as played.
        assert_eq!(&out[8..], &[0.0; 4]);
        assert_eq!(shared.consumed.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn equalizer_settings_reach_the_callback() {
        let (mut producer, consumer) = RingBuffer::new(64);
        let shared = Arc::new(Shared::new(48_000));
        shared.eq.set(&crate::EqSettings {
            enabled: true,
            preamp_db: -20.0 * 2f32.log10(), // half amplitude
            ..Default::default()
        });
        let mut renderer = Renderer::new(
            consumer,
            Arc::clone(&shared),
            48_000,
            2,
            RingBuffer::new(64).0,
        );
        producer.push_entire_slice(&[0.5; 4]).unwrap();
        let mut out = [0.0f32; 4];
        renderer.render(&mut out);
        assert!(out.iter().all(|s| (s - 0.25).abs() < 1e-4), "{out:?}");
    }

    #[test]
    fn visualization_gets_mono_samples_only_when_enabled() {
        let (mut producer, consumer) = RingBuffer::new(64);
        let (vis_producer, mut vis) = RingBuffer::new(64);
        let shared = Arc::new(Shared::new(48_000));
        let mut renderer = Renderer::new(consumer, Arc::clone(&shared), 48_000, 2, vis_producer);
        producer.push_entire_slice(&[0.2, 0.4, 0.6, 0.8]).unwrap();
        renderer.render(&mut [0.0f32; 4]);
        assert_eq!(vis.slots(), 0);

        shared.visualize.store(true, Ordering::Relaxed);
        producer.push_entire_slice(&[0.2, 0.4, 0.6, 0.8]).unwrap();
        renderer.render(&mut [0.0f32; 4]);
        let mut mono = [0.0f32; 2];
        vis.pop_entire_slice(&mut mono).unwrap();
        assert!(
            (mono[0] - 0.3).abs() < 1e-6 && (mono[1] - 0.7).abs() < 1e-6,
            "{mono:?}"
        );
    }

    #[test]
    fn paused_outputs_silence_without_consuming() {
        let (mut producer, consumer) = RingBuffer::new(64);
        let shared = Arc::new(Shared::new(48_000));
        shared.paused.store(true, Ordering::Relaxed);
        let mut renderer = Renderer::new(
            consumer,
            Arc::clone(&shared),
            48_000,
            2,
            RingBuffer::new(64).0,
        );
        producer.push_entire_slice(&[0.5; 8]).unwrap();

        let mut out = [1.0f32; 8];
        renderer.render(&mut out);
        assert_eq!(out, [0.0; 8]);
        assert_eq!(producer.slots(), 64 - 8);
    }

    #[test]
    fn a_dropped_callback_hands_its_renderer_back() {
        // A stream built again after its device is lost goes on with the same ring buffer.
        let (mut producer, consumer) = RingBuffer::new(64);
        let shared = Arc::new(Shared::new(48_000));
        let renderer = Renderer::new(consumer, shared, 48_000, 2, RingBuffer::new(64).0);
        let handoff = Handoff::new();
        drop(Handback {
            renderer: Some(renderer),
            back: handoff.sender.clone(),
        });
        producer.push_entire_slice(&[0.5; 4]).unwrap();
        let mut renderer = handoff.take().expect("renderer back");
        let mut out = [0.0f32; 4];
        renderer.render(&mut out);
        assert_eq!(out, [0.5; 4]);
    }

    #[test]
    fn a_new_device_is_asked_for_the_engines_format() {
        use cpal::{SupportedBufferSize, SupportedStreamConfigRange};
        let range = |channels, min, max, format| {
            SupportedStreamConfigRange::new(
                channels,
                min,
                max,
                SupportedBufferSize::Unknown,
                format,
            )
        };
        let ranges = [
            range(2, 44_100, 44_100, SampleFormat::F32),
            range(2, 8_000, 192_000, SampleFormat::I16),
            range(2, 8_000, 96_000, SampleFormat::F32),
            range(6, 8_000, 192_000, SampleFormat::F32),
        ];
        let config = matching_config(ranges.into_iter(), 48_000, 2).unwrap();
        assert_eq!(
            (
                config.sample_rate(),
                config.channels(),
                config.sample_format()
            ),
            (48_000, 2, SampleFormat::F32)
        );
        // Only 16-bit goes this high.
        let config = matching_config(ranges.into_iter(), 192_000, 2).unwrap();
        assert_eq!(config.sample_format(), SampleFormat::I16);
        assert!(matching_config(ranges.into_iter(), 48_000, 1).is_none());
    }

    #[test]
    fn only_a_gone_device_rebuilds_the_stream() {
        assert!(is_lost(cpal::ErrorKind::DeviceNotAvailable));
        assert!(is_lost(cpal::ErrorKind::StreamInvalidated));
        // Rerouted by the backend, or a glitch: the stream plays on.
        assert!(!is_lost(cpal::ErrorKind::DeviceChanged));
        assert!(!is_lost(cpal::ErrorKind::Xrun));
    }
}
