//! Radio stations' logos, as their directory names them: fetched on a few threads, as streams
//! are asked (only beyond this computer and its network, for a server elsewhere), made small,
//! and kept in Freya's cache, by their address's SHA-256. A logo that cannot be had is not
//! asked again while Freya runs.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

/// Logos are kept this wide and high, at most: twice what the radio view shows of them.
const SIZE: u32 = 192;
/// A logo is at most this large…
const MOST_BYTES: u64 = 1024 * 1024;
/// …and decoded within this many pixels a side, and this much memory.
const MOST_PIXELS: u32 = 2048;
const MOST_MEMORY: u64 = 64 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);
/// How many logos are fetched at once.
const WORKERS: usize = 4;

/// A logo made small, as the threads hand it over: its width, height and RGBA pixels.
type Small = (u32, u32, Vec<u8>);
/// A logo's address, and it made small if it could be had.
type Fetched = (String, Option<Small>);

pub struct Logos {
    /// Those fetched, by their address.
    images: HashMap<String, Image>,
    /// Those asked for, or that could not be had.
    asked: HashSet<String>,
    queue: Arc<Mutex<VecDeque<(String, bool)>>>,
    working: Arc<AtomicUsize>,
    done: (Sender<Fetched>, Receiver<Fetched>),
    cache: Option<PathBuf>,
    /// Goes up with every logo fetched, so the rows showing it are made again.
    version: u64,
}

impl Logos {
    pub fn new(cache: Option<PathBuf>) -> Self {
        Self {
            images: HashMap::new(),
            asked: HashSet::new(),
            queue: Arc::default(),
            working: Arc::default(),
            done: channel(),
            cache: cache.map(|dir| dir.join("radio-logos")),
            version: 0,
        }
    }

    /// The logo at `url`, once it is fetched; asked for now if it was not yet. A server
    /// elsewhere's logos are asked `far_only`.
    pub fn get(&mut self, url: &str, far_only: bool) -> Option<Image> {
        if let Some(image) = self.images.get(url) {
            return Some(image.clone());
        }
        if self.asked.insert(url.to_owned()) {
            self.queue
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back((url.to_owned(), far_only));
            if self.working.load(Ordering::Relaxed) < WORKERS {
                self.work();
            }
        }
        None
    }

    /// A thread that fetches the logos asked for until none are left.
    fn work(&self) {
        let queue = Arc::clone(&self.queue);
        let working = Arc::clone(&self.working);
        let done = self.done.0.clone();
        let cache = self.cache.clone();
        working.fetch_add(1, Ordering::Relaxed);
        let started = std::thread::Builder::new()
            .name("freya-logos".into())
            .spawn(move || {
                loop {
                    let next = queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
                    let Some((url, far_only)) = next else {
                        break;
                    };
                    let small = fetch(&url, far_only, cache.as_deref());
                    if done.send((url, small)).is_err() {
                        break;
                    }
                }
                working.fetch_sub(1, Ordering::Relaxed);
            });
        if started.is_err() {
            self.working.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Takes the logos fetched. Returns whether any were.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((url, small)) = self.done.1.try_recv() {
            if let Some((width, height, pixels)) = small {
                let buffer =
                    SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&pixels, width, height);
                self.images.insert(url, Image::from_rgba8(buffer));
                any = true;
            }
        }
        if any {
            self.version += 1;
        }
        any
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

/// The logo at `url`, made small: from the cache, or fetched and kept there.
fn fetch(url: &str, far_only: bool, cache: Option<&std::path::Path>) -> Option<Small> {
    let kept = cache.map(|dir| dir.join(format!("{}.png", hex_sha256(url))));
    if let Some(kept) = &kept
        && let Ok(bytes) = std::fs::read(kept)
        && let Some(small) = decode(&bytes)
    {
        return Some(small);
    }
    let mut answer = freya_audio::web_agent(far_only, Some(TIMEOUT))
        .get(url)
        .call()
        .ok()?;
    let bytes = answer
        .body_mut()
        .with_config()
        .limit(MOST_BYTES)
        .read_to_vec()
        .ok()?;
    let small = decode(&bytes)?;
    if let Some(kept) = &kept {
        keep(kept, &small);
    }
    Some(small)
}

/// `bytes` as a picture no larger than [`SIZE`] a side, if they are one Freya decodes.
fn decode(bytes: &[u8]) -> Option<Small> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MOST_PIXELS);
    limits.max_image_height = Some(MOST_PIXELS);
    limits.max_alloc = Some(MOST_MEMORY);
    reader.limits(limits);
    let picture = reader.decode().ok()?;
    let small = if picture.width() > SIZE || picture.height() > SIZE {
        picture.thumbnail(SIZE, SIZE)
    } else {
        picture
    }
    .to_rgba8();
    let (width, height) = small.dimensions();
    (width > 0 && height > 0).then(|| (width, height, small.into_raw()))
}

/// Keeps a logo made small in the cache, through a new file renamed over the old.
fn keep(path: &std::path::Path, (width, height, pixels): &Small) {
    let Some(picture) = image::RgbaImage::from_raw(*width, *height, pixels.clone()) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let partial = path.with_extension("partial");
    let written = picture
        .save_with_format(&partial, image::ImageFormat::Png)
        .map_err(|e| e.to_string())
        .and_then(|()| std::fs::rename(&partial, path).map_err(|e| e.to_string()));
    if let Err(e) = written {
        log::debug!("could not keep a radio logo: {e}");
    }
}

fn hex_sha256(text: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(width, height, image::Rgba([200, 30, 30, 255]))
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn logos_are_made_small_and_only_pictures_are_taken() {
        let (width, height, pixels) = decode(&png(600, 300)).unwrap();
        assert_eq!((width, height), (SIZE, SIZE / 2));
        assert_eq!(pixels.len(), (width * height * 4) as usize);
        assert_eq!(
            decode(&png(40, 40)).unwrap().0,
            40,
            "a small one stays as it is"
        );
        assert!(decode(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_none());
        assert!(decode(b"<!DOCTYPE html>").is_none());
        assert!(decode(&png(4000, 10)).is_none(), "wider than it may be");
    }

    #[test]
    fn logos_are_fetched_once() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/logo.png", listener.local_addr().unwrap());
        let body = png(300, 100);
        let asked = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&asked);
        std::thread::spawn(move || {
            for mut connection in listener.incoming().flatten() {
                let _ = connection.read(&mut [0; 4096]);
                counted.fetch_add(1, Ordering::Relaxed);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = connection.write_all(head.as_bytes());
                let _ = connection.write_all(&body);
            }
        });
        let mut logos = Logos::new(None);
        assert!(logos.get(&url, false).is_none());
        let started = std::time::Instant::now();
        while !logos.poll() {
            assert!(started.elapsed() < Duration::from_secs(10), "no logo");
            std::thread::sleep(Duration::from_millis(10));
        }
        let logo = logos.get(&url, false).unwrap();
        assert_eq!((logo.size().width, logo.size().height), (SIZE, SIZE / 3));
        assert_eq!(asked.load(Ordering::Relaxed), 1);
        // Not asked of this computer when only far addresses may be.
        let near = url.replace("logo.png", "other.png");
        assert!(logos.get(&near, true).is_none());
        std::thread::sleep(Duration::from_millis(300));
        logos.poll();
        assert!(logos.get(&near, true).is_none());
        assert_eq!(asked.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_logo_is_kept_and_read_back() {
        let dir = std::env::temp_dir().join(format!("freya-logos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(format!(
            "{}.png",
            hex_sha256("https://example.org/logo.png")
        ));
        let small = decode(&png(300, 300)).unwrap();
        keep(&path, &small);
        assert_eq!(decode(&std::fs::read(&path).unwrap()), Some(small));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
