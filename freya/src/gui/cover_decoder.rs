//! Covers decoded on threads of their own, ahead of their being scrolled to: decoded on the
//! window's thread as each came into view, they made the first scroll through a grid stutter.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use slint::{Rgb8Pixel, SharedPixelBuffer};

/// Threads decoding at once.
const WORKERS: usize = 2;

/// A cover decoded, by its key; none when its file could not be read.
type Decoded = (String, Option<SharedPixelBuffer<Rgb8Pixel>>);

pub struct CoverDecoder {
    jobs: Option<Sender<(String, PathBuf)>>,
    done: (Sender<Decoded>, Receiver<Decoded>),
    /// Keys being decoded.
    waiting: HashSet<String>,
}

impl CoverDecoder {
    pub fn new() -> Self {
        Self {
            jobs: None,
            done: channel(),
            waiting: HashSet::new(),
        }
    }

    /// Decodes the cover `key` from `file`, unless it is being decoded already.
    pub fn decode(&mut self, key: &str, file: PathBuf) {
        if self.waiting.contains(key) {
            return;
        }
        let jobs = self.jobs.get_or_insert_with(|| {
            let (jobs, queue) = channel::<(String, PathBuf)>();
            let queue = Arc::new(Mutex::new(queue));
            for _ in 0..WORKERS {
                let (queue, done) = (Arc::clone(&queue), self.done.0.clone());
                let started = std::thread::Builder::new()
                    .name("freya-cover-decoder".into())
                    .spawn(move || {
                        // The lock goes with the statement, before the decoding.
                        while let Some((key, file)) = queue.lock().ok().and_then(|q| q.recv().ok())
                        {
                            let decoded = image::open(&file)
                                .map_err(|e| log::debug!("could not read {}: {e}", file.display()))
                                .ok()
                                .map(|image| {
                                    let rgb = image.to_rgb8();
                                    SharedPixelBuffer::clone_from_slice(
                                        rgb.as_raw(),
                                        rgb.width(),
                                        rgb.height(),
                                    )
                                });
                            if done.send((key, decoded)).is_err() {
                                break;
                            }
                        }
                    });
                if let Err(e) = started {
                    log::warn!("could not start decoding covers: {e}");
                }
            }
            jobs
        });
        if jobs.send((key.to_owned(), file)).is_ok() {
            self.waiting.insert(key.to_owned());
        }
    }

    /// Whether the cover `key` is being decoded.
    pub fn is_decoding(&self, key: &str) -> bool {
        self.waiting.contains(key)
    }

    /// The covers decoded since the last call.
    pub fn take(&mut self) -> Vec<Decoded> {
        let decoded: Vec<Decoded> = self.done.1.try_iter().collect();
        for (key, _) in &decoded {
            self.waiting.remove(key);
        }
        decoded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_covers_on_a_thread() {
        let dir = std::env::temp_dir().join(format!("freya-decoder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cover.jpg");
        image::RgbImage::from_pixel(30, 20, image::Rgb([200, 0, 0]))
            .save(&file)
            .unwrap();
        let mut decoder = CoverDecoder::new();
        decoder.decode("a", file);
        decoder.decode("gone", dir.join("gone.jpg"));
        assert!(decoder.is_decoding("a"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut decoded = Vec::new();
        while decoded.len() < 2 {
            assert!(std::time::Instant::now() < deadline);
            decoded.extend(decoder.take());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        decoded.sort_by(|a, b| a.0.cmp(&b.0));
        let picture = decoded[0].1.as_ref().unwrap();
        assert_eq!((picture.width(), picture.height()), (30, 20));
        assert!(decoded[1].1.is_none());
        assert!(!decoder.is_decoding("a"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
