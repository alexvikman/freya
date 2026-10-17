//! An output without a device: pulls samples on its own thread, like an audio device would.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::output::Renderer;

const BUFFER_FRAMES: usize = 512;

pub(crate) struct NullOutput {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl NullOutput {
    /// With `realtime`, samples are pulled at that sample rate; without it, as fast as
    /// possible (for tests).
    pub fn start(mut renderer: Renderer, channels: usize, realtime: Option<u32>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let pause =
            realtime.map(|rate| Duration::from_secs_f64(BUFFER_FRAMES as f64 / f64::from(rate)));
        let thread = std::thread::Builder::new()
            .name("freya-null-output".into())
            .spawn(move || {
                let mut buf = vec![0.0f32; BUFFER_FRAMES * channels];
                while !flag.load(Ordering::Relaxed) {
                    renderer.render(&mut buf);
                    match pause {
                        Some(pause) => std::thread::sleep(pause),
                        None => std::thread::yield_now(),
                    }
                }
            })
            .expect("spawn null output thread");
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for NullOutput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
