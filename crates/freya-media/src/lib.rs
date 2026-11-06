//! Music files and what is in and around them, for Freya and Freya Server alike: finding
//! playable files, their tags and CUE sheets, playlist entries and files, covers, and pictures
//! of artists, found online and kept with their thumbnails.

pub mod artist_image;
pub mod cover;
pub mod cue;
pub mod entry;
pub mod files;
pub mod pictures;
pub mod playlist_file;
pub mod tags;
pub mod thumbnails;

pub use entry::Entry;
pub use tags::{TagInfo, TagLoader, read_tags};

/// A picture in `bytes`, decoded within bounds: no wider or higher than 12,000 pixels, nor
/// needing more than 256 MB. Pictures come from files others may have written, from servers and
/// from the internet: one small as a file and huge decoded cannot take the memory. Only a few
/// are decoded at once, whoever asks: many asked for together, each as large as the bounds
/// allow, cannot take it either.
pub fn decode_picture(bytes: &[u8]) -> image::ImageResult<image::DynamicImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    reader.limits(picture_limits());
    let _decoding = DECODES.enter();
    reader.decode()
}

/// How many pictures are decoded at once at most, in all: each may take 256 MB.
const DECODES_AT_ONCE: usize = 6;

static DECODES: Gate = Gate::new(DECODES_AT_ONCE);

/// Lets so many through at once; the rest wait their turn.
struct Gate {
    free: std::sync::Mutex<usize>,
    changed: std::sync::Condvar,
}

impl Gate {
    const fn new(at_once: usize) -> Self {
        Self {
            free: std::sync::Mutex::new(at_once),
            changed: std::sync::Condvar::new(),
        }
    }

    /// Waits for a place, held until what is returned is dropped.
    fn enter(&self) -> Through<'_> {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        while *free == 0 {
            free = self.changed.wait(free).unwrap_or_else(|e| e.into_inner());
        }
        *free -= 1;
        Through(self)
    }
}

/// A place through a [`Gate`], given back when dropped.
struct Through<'a>(&'a Gate);

impl Drop for Through<'_> {
    fn drop(&mut self) {
        *self.0.free.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.0.changed.notify_one();
    }
}

/// How wide and high the picture in `bytes` is, read from its head alone: an error for what is
/// not a picture Freya decodes, or one beyond the bounds [`decode_picture`] decodes within. So
/// bytes can be checked to be a picture before they are kept, without decoding them.
pub fn picture_dimensions(bytes: &[u8]) -> image::ImageResult<(u32, u32)> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let limits = picture_limits();
    reader.limits(limits.clone());
    let (width, height) = reader.into_dimensions()?;
    limits.check_dimensions(width, height)?;
    Ok((width, height))
}

/// The bounds a picture is decoded within.
fn picture_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pictures_within_bounds_have_dimensions() {
        let mut png = Vec::new();
        image::RgbImage::from_pixel(30, 20, image::Rgb([1, 2, 3]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(picture_dimensions(&png).unwrap(), (30, 20));
        assert!(picture_dimensions(b"not a picture").is_err());
        assert!(picture_dimensions(b"").is_err());
        // A head that says it is huge, read without decoding it.
        let mut huge = png.clone();
        // PNG's IHDR: width and height are the 4 bytes each at 16 and 20.
        huge[16..20].copy_from_slice(&30_000u32.to_be_bytes());
        assert!(picture_dimensions(&huge).is_err());
    }

    #[test]
    fn a_gate_lets_so_many_through_at_once() {
        let gate = Gate::new(2);
        let first = gate.enter();
        let second = gate.enter();
        assert_eq!(*gate.free.lock().unwrap(), 0);
        let (tell, told) = std::sync::mpsc::channel();
        let waiting = std::thread::scope(|threads| {
            threads.spawn(|| {
                let _third = gate.enter();
                tell.send(()).unwrap();
            });
            // Held up until a place is given back.
            let early = told.recv_timeout(std::time::Duration::from_millis(200));
            drop(first);
            let late = told.recv_timeout(std::time::Duration::from_secs(5));
            (early.is_err(), late.is_ok())
        });
        assert_eq!(waiting, (true, true));
        drop(second);
        assert_eq!(*gate.free.lock().unwrap(), 2);
    }
}
