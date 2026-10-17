//! The audio callback side: pulls samples from the ring buffer into the device.

use std::sync::Arc;
use std::sync::atomic::Ordering;

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
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| Error::Output("no output device".into()))?;
        let config = device
            .default_output_config()
            .map_err(|e| Error::Output(e.to_string()))?;
        log::info!("output: {device} {config:?}");
        Ok(Self { device, config })
    }

    pub fn format(&self) -> DeviceFormat {
        DeviceFormat {
            sample_rate: self.config.sample_rate(),
            channels: usize::from(self.config.channels()),
        }
    }

    pub fn start(self, renderer: Renderer) -> Result<cpal::Stream, Error> {
        let format = self.config.sample_format();
        let buffer_size = buffer_size(self.config.buffer_size());
        let mut config: cpal::StreamConfig = self.config.into();
        config.buffer_size = buffer_size;
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&self.device, config, renderer),
            SampleFormat::F64 => build::<f64>(&self.device, config, renderer),
            SampleFormat::I16 => build::<i16>(&self.device, config, renderer),
            SampleFormat::I32 => build::<i32>(&self.device, config, renderer),
            SampleFormat::U16 => build::<u16>(&self.device, config, renderer),
            SampleFormat::U8 => build::<u8>(&self.device, config, renderer),
            other => return Err(Error::Output(format!("unsupported sample format {other}"))),
        }?;
        stream.play().map_err(|e| Error::Output(e.to_string()))?;
        Ok(stream)
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
    mut renderer: Renderer,
) -> Result<cpal::Stream, Error> {
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| renderer.render(data),
            |err| log::error!("audio stream: {err}"),
            None,
        )
        .map_err(|e| Error::Output(e.to_string()))
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
}
