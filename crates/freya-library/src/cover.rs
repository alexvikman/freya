//! Cover art: from a track's tags, or from an image next to it in the folder.

use std::path::Path;

use lofty::picture::PictureType;
use lofty::prelude::*;

/// File names (without extension) that commonly hold an album's cover, in order of preference.
const FOLDER_IMAGES: [&str; 5] = ["cover", "folder", "front", "album", "artwork"];
const IMAGE_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];
/// Subfolders (lowercase) that rips often keep the artwork in.
const ARTWORK_FOLDERS: [&str; 4] = ["cover", "covers", "artwork", "scans"];

/// A decoded cover image, 4 bytes (RGBA) per pixel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cover {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Cover {
    /// Writes the cover as a PNG file, e.g. for the system's Now Playing display.
    pub fn save_png(&self, path: &Path) -> Result<(), String> {
        let image = image::RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
            .ok_or("the cover's size does not match its pixels")?;
        image.save(path).map_err(|e| e.to_string())
    }
}

/// Finds and decodes the cover for `track`, scaled down to fit in `max_size` × `max_size`.
pub fn read_cover(track: &Path, max_size: u32) -> Option<Cover> {
    let bytes = embedded_cover(track).or_else(|| folder_cover(track))?;
    let image = image::load_from_memory(&bytes).ok()?;
    let image = if image.width() > max_size || image.height() > max_size {
        image.thumbnail(max_size, max_size)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    Some(Cover {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// The front cover from the file's tags, or else any picture in them.
fn embedded_cover(track: &Path) -> Option<Vec<u8>> {
    let file = lofty::read_from_path(track).ok()?;
    let pictures: Vec<_> = file.tags().iter().flat_map(|t| t.pictures()).collect();
    pictures
        .iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| pictures.first())
        .map(|p| p.data().to_vec())
}

/// An image next to the track, or else in an artwork subfolder such as "Covers".
fn folder_cover(track: &Path) -> Option<Vec<u8>> {
    let dir = track.parent()?;
    image_in(dir).or_else(|| {
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        ARTWORK_FOLDERS.contains(&name.to_ascii_lowercase().as_str())
                    })
            })
            .find_map(|folder| image_in(&folder))
    })
}

/// In `dir`: an image with a usual cover name, or else the folder's only image.
fn image_in(dir: &Path) -> Option<Vec<u8>> {
    let images: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        })
        .collect();
    let rank = |path: &Path| {
        let stem = path.file_stem()?.to_str()?.to_ascii_lowercase();
        FOLDER_IMAGES.iter().position(|name| *name == stem)
    };
    let mut named: Vec<(usize, &std::path::PathBuf)> = images
        .iter()
        .filter_map(|path| Some((rank(path)?, path)))
        .collect();
    named.sort();
    let chosen = match (named.first(), images.as_slice()) {
        (Some((_, path)), _) => *path,
        (None, [only]) => only,
        _ => return None,
    };
    std::fs::read(chosen).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_folder_image_and_scales_it_down() {
        let dir = std::env::temp_dir().join(format!("freya-cover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = image::RgbImage::from_pixel(800, 400, image::Rgb([200, 100, 50]));
        img.save(dir.join("Folder.JPG")).unwrap();
        let track = dir.join("song.mp3");
        std::fs::write(&track, b"not really audio").unwrap();

        let cover = read_cover(&track, 256).expect("cover from folder image");
        assert_eq!((cover.width, cover.height), (256, 128));
        assert_eq!(cover.rgba.len(), 256 * 128 * 4);
    }

    #[test]
    fn takes_a_folders_only_image_but_guesses_no_further() {
        let dir = std::env::temp_dir().join(format!("freya-cover-only-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let track = dir.join("song.flac");
        std::fs::write(&track, b"not really audio").unwrap();
        let img = image::RgbImage::from_pixel(40, 40, image::Rgb([1, 2, 3]));
        img.save(dir.join("Some Album.flac.png")).unwrap();
        assert!(
            read_cover(&track, 256).is_some(),
            "the only image is the cover"
        );

        img.save(dir.join("back.png")).unwrap();
        assert_eq!(
            read_cover(&track, 256),
            None,
            "two images, neither named as a cover"
        );
    }

    #[test]
    fn looks_in_artwork_subfolders() {
        let dir = std::env::temp_dir().join(format!("freya-cover-sub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Covers")).unwrap();
        let track = dir.join("disc.flac");
        std::fs::write(&track, b"not really audio").unwrap();
        image::RgbImage::from_pixel(40, 40, image::Rgb([9, 8, 7]))
            .save(dir.join("Covers").join("Front.jpg"))
            .unwrap();
        image::RgbImage::from_pixel(40, 40, image::Rgb([0, 0, 0]))
            .save(dir.join("Covers").join("Back.jpg"))
            .unwrap();
        assert!(read_cover(&track, 256).is_some());
    }

    #[test]
    fn reads_embedded_front_cover() {
        let dir = std::env::temp_dir().join(format!("freya-cover-tag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let track = dir.join("tagged.mp3");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../freya-audio/tests/fixtures/sine.mp3");
        std::fs::copy(&fixture, &track).unwrap();

        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(64, 64, image::Rgb([10, 200, 30]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let mut file = lofty::read_from_path(&track).unwrap();
        let tag = file.primary_tag_mut().unwrap();
        tag.push_picture(
            lofty::picture::Picture::unchecked(png.into_inner())
                .pic_type(PictureType::CoverFront)
                .build(),
        );
        tag.save_to_path(&track, lofty::config::WriteOptions::default())
            .unwrap();

        let cover = read_cover(&track, 256).expect("embedded cover");
        assert_eq!((cover.width, cover.height), (64, 64));
        assert_eq!(&cover.rgba[..4], &[10, 200, 30, 255]);
    }

    #[test]
    fn no_cover_is_none() {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../freya-audio/tests/fixtures/sine.flac");
        assert_eq!(read_cover(&fixture, 256), None);
    }
}
