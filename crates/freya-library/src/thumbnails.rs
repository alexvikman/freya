//! Small album covers for grids. Each is made once, on a thread of its own, and kept as a file,
//! so showing thousands of albums only ever decodes the covers on screen. That an album has no
//! cover is only remembered until Freya quits, so a cover added later is found.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::cover::read_cover;

/// The longest side of a thumbnail, in pixels: sharp in a 150-point grid tile on a Retina
/// screen, about 30 KB as a JPEG.
pub const SIZE: u32 = 300;
const JPEG_QUALITY: u8 = 85;

/// What is known about one album's thumbnail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Thumbnail {
    /// Made; the file to show.
    Ready(PathBuf),
    /// The album has no cover, as far as this run of Freya knows.
    None,
    /// Being made; [`Thumbnails::poll`] says when.
    Pending,
}

pub struct Thumbnails {
    dir: PathBuf,
    /// Albums asked for, with what is known about them.
    known: HashMap<String, Thumbnail>,
    /// Started with the first request.
    jobs: Option<Sender<(String, PathBuf)>>,
    /// Albums whose covers were looked for, and whether a thumbnail was made.
    finished_tx: Sender<(String, bool)>,
    finished: Receiver<(String, bool)>,
}

impl Thumbnails {
    /// Thumbnails kept in `dir`, which is created when the first one is made.
    pub fn new(dir: PathBuf) -> Self {
        let (finished_tx, finished) = channel();
        Self {
            dir,
            known: HashMap::new(),
            jobs: None,
            finished_tx,
            finished,
        }
    }

    /// A name for an album's thumbnail that stays the same between runs: 16 hex digits.
    pub fn key(artist: Option<&str>, album: Option<&str>) -> String {
        // FNV-1a: tiny and stable, unlike the standard library's hasher.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let text = format!("{}\0{}", artist.unwrap_or(""), album.unwrap_or(""));
        for byte in text.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{hash:016x}")
    }

    /// The thumbnail for `key`. When it is not made yet, it is made from the cover of `track`,
    /// one of the album's tracks.
    pub fn get(&mut self, key: &str, track: &Path) -> Thumbnail {
        if let Some(known) = self.known.get(key) {
            return known.clone();
        }
        let file = self.file(key);
        let state = if file.exists() && !outdated(&file, track) {
            Thumbnail::Ready(file)
        } else {
            self.request(key, track);
            Thumbnail::Pending
        };
        self.known.insert(key.to_owned(), state.clone());
        state
    }

    /// Takes in thumbnails made since the last call. Returns true when there were any.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((key, made)) = self.finished.try_recv() {
            if made {
                // Found as a file on the next `get`.
                self.known.remove(&key);
            } else {
                self.known.insert(key, Thumbnail::None);
            }
            any = true;
        }
        any
    }

    fn file(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.jpg"))
    }

    fn request(&mut self, key: &str, track: &Path) {
        let jobs = self.jobs.get_or_insert_with(|| {
            let (jobs, queue) = channel::<(String, PathBuf)>();
            let dir = self.dir.clone();
            let finished = self.finished_tx.clone();
            let started = std::thread::Builder::new()
                .name("freya-covers".into())
                .spawn(move || {
                    for (key, track) in queue {
                        let made = match make(&dir, &key, &track) {
                            Ok(made) => made,
                            Err(e) => {
                                log::debug!("no thumbnail for {}: {e}", track.display());
                                false
                            }
                        };
                        if finished.send((key, made)).is_err() {
                            break;
                        }
                    }
                });
            if let Err(e) = started {
                log::warn!("could not start making covers: {e}");
            }
            jobs
        });
        let _ = jobs.send((key.to_owned(), track.to_owned()));
    }
}

/// Whether the thumbnail `file` is older than where its cover may come from: the track, with
/// its embedded picture, its folder, where adding a cover image changes the folder, and the
/// images in it.
fn outdated(file: &Path, track: &Path) -> bool {
    let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let Some(made) = modified(file) else {
        return true;
    };
    let newer = |path: &Path| modified(path).is_some_and(|changed| changed > made);
    let Some(folder) = track.parent() else {
        return newer(track);
    };
    let images = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            ["jpg", "jpeg", "png", "webp", "gif", "bmp"]
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        });
    newer(track) || newer(folder) || images.into_iter().any(|entry| newer(&entry.path()))
}

/// Makes the thumbnail for `key` from `track`'s cover. Returns false when there is no cover,
/// and then removes one made before.
fn make(dir: &Path, key: &str, track: &Path) -> Result<bool, String> {
    let Some(cover) = read_cover(track, SIZE) else {
        let _ = std::fs::remove_file(dir.join(format!("{key}.jpg")));
        return Ok(false);
    };
    create_private_dir(dir).map_err(|e| e.to_string())?;
    let rgba = image::RgbaImage::from_raw(cover.width, cover.height, cover.rgba)
        .ok_or("the cover's size does not match its pixels")?;
    let rgb = image::DynamicImage::ImageRgba8(rgba).to_rgb8();
    // Written under another name and then renamed, so a half-written file is never shown.
    let partial = dir.join(format!("{key}.partial"));
    let file = std::fs::File::create(&partial).map_err(|e| e.to_string())?;
    image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), JPEG_QUALITY)
        .encode_image(&rgb)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&partial, dir.join(format!("{key}.jpg"))).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Creates `dir`, readable only by the user where the system allows: covers say what music
/// they have.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("freya-thumbs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wait(thumbnails: &mut Thumbnails) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !thumbnails.poll() {
            assert!(Instant::now() < deadline, "no thumbnail was made");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn keys_are_stable_and_tell_albums_apart() {
        let key = Thumbnails::key(Some("Björk"), Some("Post"));
        assert_eq!(key.len(), 16);
        assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(key, Thumbnails::key(Some("Björk"), Some("Post")));
        assert_ne!(
            Thumbnails::key(Some("Björk"), Some("Post")),
            Thumbnails::key(Some("Björk"), Some("Debut"))
        );
        // The separator keeps "ab" + "c" apart from "a" + "bc".
        assert_ne!(
            Thumbnails::key(Some("ab"), Some("c")),
            Thumbnails::key(Some("a"), Some("bc"))
        );
    }

    #[test]
    fn makes_thumbnails_once_and_looks_again_for_missing_covers_next_run() {
        let music = temp_dir("music");
        let image = image::RgbImage::from_pixel(1200, 1200, image::Rgb([200, 100, 50]));
        image.save(music.join("cover.jpg")).unwrap();
        let track = music.join("song.mp3");
        std::fs::write(&track, b"not really audio").unwrap();
        let bare = temp_dir("bare").join("song.mp3");
        std::fs::write(&bare, b"not really audio").unwrap();

        let cache = temp_dir("cache").join("covers");
        let mut thumbnails = Thumbnails::new(cache.clone());
        assert_eq!(thumbnails.get("a", &track), Thumbnail::Pending);
        assert_eq!(
            thumbnails.get("a", &track),
            Thumbnail::Pending,
            "asked once"
        );
        wait(&mut thumbnails);
        let Thumbnail::Ready(file) = thumbnails.get("a", &track) else {
            panic!("the thumbnail is made");
        };
        let made = image::open(&file).unwrap();
        assert_eq!((made.width(), made.height()), (SIZE, SIZE));

        assert_eq!(thumbnails.get("b", &bare), Thumbnail::Pending);
        wait(&mut thumbnails);
        assert_eq!(thumbnails.get("b", &bare), Thumbnail::None);
        assert_eq!(thumbnails.known.len(), 2);

        // A new run finds the thumbnail made, and looks again for the missing cover.
        let mut again = Thumbnails::new(cache.clone());
        assert_eq!(again.get("a", &track), Thumbnail::Ready(file.clone()));
        assert_eq!(again.get("b", &bare), Thumbnail::Pending);

        // A cover replaced after the thumbnail was made: the next run makes it again. It kept
        // the old one for good.
        std::thread::sleep(Duration::from_millis(20));
        let image = image::RgbImage::from_pixel(600, 300, image::Rgb([0, 0, 255]));
        image.save(music.join("cover.jpg")).unwrap();
        let mut later = Thumbnails::new(cache);
        assert_eq!(later.get("a", &track), Thumbnail::Pending);
        wait(&mut later);
        let Thumbnail::Ready(file) = later.get("a", &track) else {
            panic!("the thumbnail is made again");
        };
        let made = image::open(&file).unwrap();
        assert_eq!((made.width(), made.height()), (SIZE, SIZE / 2));
    }
}
