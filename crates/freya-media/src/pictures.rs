//! Pictures downloaded from the internet, kept as they came: each once, named by the SHA-256
//! of what it holds, so two artists with the same picture share one file. They are kept with
//! Freya's data rather than its cache, which the system may empty, since getting them again
//! takes the internet. The library says which picture is whose; [`Pictures::keep_only`] takes
//! away those it no longer names.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// A picture newer than this is kept even when the library does not name it yet: it was
/// downloaded just now, and the library is told of it after it is written.
const YOUNG: Duration = Duration::from_secs(3600);

#[derive(Clone, Debug)]
pub struct Pictures {
    dir: PathBuf,
}

impl Pictures {
    /// The picture in `file`, at most `largest` wide or high, kept among these: its checksum.
    pub fn take_file(&self, file: &Path, largest: u32) -> Result<String, String> {
        let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
        self.take_bytes(&bytes, largest)
    }

    /// The picture in `bytes`, at most `largest` wide or high, kept among these: its checksum.
    pub fn take_bytes(&self, bytes: &[u8], largest: u32) -> Result<String, String> {
        let picture = crate::decode_picture(bytes).map_err(|e| e.to_string())?;
        let picture = if picture.width() > largest || picture.height() > largest {
            picture.resize(largest, largest, image::imageops::FilterType::Lanczos3)
        } else {
            picture
        };
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
            .encode_image(&picture.to_rgb8())
            .map_err(|e| e.to_string())?;
        self.put(&jpeg).map_err(|e| e.to_string())
    }

    /// Pictures kept in `dir`, which is made when the first one is written.
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The name of a picture holding `bytes`: their SHA-256, in hex.
    pub fn checksum(bytes: &[u8]) -> String {
        ring::digest::digest(&ring::digest::SHA256, bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Keeps `bytes`, unless they are kept already, and returns their name. Written under
    /// another name first and then renamed, so a half-written picture is never read.
    pub fn put(&self, bytes: &[u8]) -> std::io::Result<String> {
        let checksum = Self::checksum(bytes);
        let file = self.path(&checksum);
        if file.is_file() {
            return Ok(checksum);
        }
        let dir = file.parent().unwrap_or(&self.dir);
        create_private_dir(dir)?;
        // Each write its own: threads may keep the same picture at once.
        static WRITES: AtomicU64 = AtomicU64::new(0);
        let write = WRITES.fetch_add(1, Ordering::Relaxed);
        let partial = dir.join(format!("{checksum}.{}-{write}.partial", std::process::id()));
        let mut out = std::fs::File::create(&partial)?;
        out.write_all(bytes)?;
        out.sync_all()?;
        drop(out);
        if let Err(e) = std::fs::rename(&partial, &file) {
            let _ = std::fs::remove_file(&partial);
            if !file.is_file() {
                return Err(e);
            }
        }
        Ok(checksum)
    }

    /// Where the picture `checksum` is kept, kept or not: in a folder named by its first two
    /// digits, so no folder holds more than a few thousand.
    pub fn path(&self, checksum: &str) -> PathBuf {
        let folder = checksum.get(..2).unwrap_or("00");
        self.dir.join(folder).join(checksum)
    }

    /// The picture `checksum`, when it is kept and still holds what its name says.
    pub fn read(&self, checksum: &str) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.path(checksum)).ok()?;
        (Self::checksum(&bytes) == checksum).then_some(bytes)
    }

    /// Takes away the pictures not in `wanted`, but for those written in the last hour, which
    /// the library may not have been told of yet. Returns how many were taken away.
    pub fn keep_only(&self, wanted: &HashSet<String>) -> std::io::Result<usize> {
        let folders = match std::fs::read_dir(&self.dir) {
            Ok(folders) => folders,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let mut removed = 0;
        for folder in folders.flatten() {
            let Ok(files) = std::fs::read_dir(folder.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                let name = file.file_name().to_string_lossy().into_owned();
                let young = file
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|made| SystemTime::now().duration_since(made).ok())
                    .is_none_or(|age| age < YOUNG);
                if young || wanted.contains(&name) {
                    continue;
                }
                std::fs::remove_file(&path)?;
                removed += 1;
            }
            // Gone when empty; kept, and the error ignored, when not.
            let _ = std::fs::remove_dir(folder.path());
        }
        // And the small ones of those no longer wanted.
        for file in std::fs::read_dir(self.small_dir())
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = file.file_name().to_string_lossy().into_owned();
            let of = name.split('-').next().unwrap_or_default();
            if !wanted.contains(of) && std::fs::remove_file(file.path()).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The picture `checksum` at most `size` wide or high, as a JPEG: made the first time it
    /// is asked for and kept beside the pictures, for clients to show in a grid without
    /// fetching the picture whole. None when the picture is not kept.
    pub fn small(&self, checksum: &str, size: u32) -> Option<Vec<u8>> {
        let file = self.small_dir().join(format!("{checksum}-{size}.jpg"));
        if let Ok(bytes) = std::fs::read(&file) {
            return Some(bytes);
        }
        let picture = crate::decode_picture(&self.read(checksum)?).ok()?;
        let small = if picture.width() > size || picture.height() > size {
            picture.thumbnail(size, size)
        } else {
            picture
        };
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, SMALL_QUALITY)
            .encode_image(&small.to_rgb8())
            .ok()?;
        let kept = create_private_dir(&self.small_dir()).and_then(|()| {
            let partial = file.with_extension("partial");
            std::fs::write(&partial, &jpeg)?;
            std::fs::rename(&partial, &file)
        });
        if let Err(e) = kept {
            log::debug!("could not keep a small picture: {e}");
        }
        Some(jpeg)
    }

    /// Where the small pictures are kept: beside the pictures, not among them.
    fn small_dir(&self) -> PathBuf {
        let mut name = self.dir.file_name().unwrap_or_default().to_owned();
        name.push("-small");
        self.dir.with_file_name(name)
    }
}

/// How well a small picture is kept.
const SMALL_QUALITY: u8 = 85;

/// Creates `dir`, readable only by the user where the system allows: pictures say what music
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
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("freya-pictures-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_small_picture_is_made_once_and_goes_with_its_picture() {
        let dir = temp_dir("small");
        let pictures = Pictures::new(dir.join("pictures"));
        let mut png = Vec::new();
        image::RgbImage::from_pixel(900, 600, image::Rgb([200, 0, 0]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let kept = pictures.put(&png).unwrap();
        let small = pictures.small(&kept, 300).unwrap();
        let shown = image::load_from_memory(&small).unwrap();
        assert_eq!((shown.width(), shown.height()), (300, 200));
        // Kept beside the pictures, not among them, and read as it was made.
        let file = dir.join("pictures-small").join(format!("{kept}-300.jpg"));
        assert_eq!(std::fs::read(&file).unwrap(), small);
        assert_eq!(pictures.small(&kept, 300).unwrap(), small);
        assert_eq!(pictures.small(&"0".repeat(64), 300), None);
        // A picture no longer wanted takes its small one with it.
        pictures.keep_only(&HashSet::new()).unwrap();
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_each_picture_once_by_what_it_holds() {
        let pictures = Pictures::new(temp_dir("once"));
        let a = pictures.put(b"one picture").unwrap();
        assert_eq!(a.len(), 64);
        assert_eq!(a, Pictures::checksum(b"one picture"));
        assert_eq!(pictures.put(b"one picture").unwrap(), a);
        let b = pictures.put(b"another").unwrap();
        assert_ne!(a, b);
        assert_eq!(pictures.read(&a).as_deref(), Some(&b"one picture"[..]));
        assert!(pictures.path(&a).starts_with(pictures.dir().join(&a[..2])));

        // A picture that no longer holds what its name says is not read.
        std::fs::write(pictures.path(&b), b"damaged").unwrap();
        assert_eq!(pictures.read(&b), None);
        assert_eq!(pictures.read("0000"), None);
        let _ = std::fs::remove_dir_all(pictures.dir());
    }

    #[test]
    fn keeps_one_picture_written_by_many_threads_at_once() {
        let pictures = Pictures::new(temp_dir("threads"));
        let checksums: Vec<String> = std::thread::scope(|threads| {
            let handles: Vec<_> = (0..8)
                .map(|_| threads.spawn(|| pictures.put(&[7u8; 100_000]).unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(checksums.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(pictures.read(&checksums[0]).unwrap().len(), 100_000);
        let _ = std::fs::remove_dir_all(pictures.dir());
    }

    #[test]
    fn takes_away_pictures_no_one_names_but_new_ones() {
        let pictures = Pictures::new(temp_dir("keep"));
        assert_eq!(pictures.keep_only(&HashSet::new()).unwrap(), 0, "none yet");
        let named = pictures.put(b"named").unwrap();
        let unnamed = pictures.put(b"unnamed").unwrap();
        let wanted = HashSet::from([named.clone()]);

        // Just written: kept, as the library may not know of it yet.
        assert_eq!(pictures.keep_only(&wanted).unwrap(), 0);

        let old = SystemTime::now() - 2 * YOUNG;
        for checksum in [&named, &unnamed] {
            std::fs::File::options()
                .write(true)
                .open(pictures.path(checksum))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        assert_eq!(pictures.keep_only(&wanted).unwrap(), 1);
        assert!(pictures.path(&named).exists());
        assert!(!pictures.path(&unnamed).exists());
        let _ = std::fs::remove_dir_all(pictures.dir());
    }
}
