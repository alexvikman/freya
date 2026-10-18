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

/// A color for an album's page, from its cover's pixels (RGB, 3 bytes each): the cover's
/// colors averaged, the vivid ones counting most so a red cover on grey gives red, then made
/// dark enough for white text and not garish. A grey cover gives grey.
pub fn page_color(rgb: &[u8]) -> [u8; 3] {
    let mut sum = [0.0f64; 3];
    let mut weight = 0.0;
    for pixel in rgb.as_chunks::<3>().0 {
        let [r, g, b] = pixel.map(|c| f64::from(c) / 255.0);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        // Vivid pixels count up to nine times as much as grey ones; near-black and near-white
        // ones, as in a border or a background, hardly at all.
        let vivid = 1.0 + 8.0 * (max - min);
        let w = vivid * (max * (1.0 - min)).max(0.02);
        sum[0] += r * w;
        sum[1] += g * w;
        sum[2] += b * w;
        weight += w;
    }
    if weight == 0.0 {
        return [83, 83, 83];
    }
    let [r, g, b] = sum.map(|c| c / weight);
    let (hue, saturation, _) = hsl(r, g, b);
    let mut rgb = from_hsl(hue, saturation.min(0.55), 0.32);
    // Yellows and greens look lighter than blues at the same lightness: darker still, until
    // white text reads on them as on the others.
    let [r, g, b] = rgb;
    let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if luminance > MAX_LUMINANCE {
        rgb = rgb.map(|c| c * MAX_LUMINANCE / luminance);
    }
    rgb.map(|c| (c * 255.0).round() as u8)
}

/// The most a page color's luminance may be, from 0 to 1.
const MAX_LUMINANCE: f64 = 0.38;

/// Hue (0 to 1), saturation and lightness of an RGB color with channels from 0 to 1.
fn hsl(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let lightness = (max + min) / 2.0;
    let delta = max - min;
    if delta < 1e-9 {
        return (0.0, 0.0, lightness);
    }
    let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs());
    let hue = if max == r {
        ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    } / 6.0;
    (hue, saturation, lightness)
}

fn from_hsl(hue: f64, saturation: f64, lightness: f64) -> [f64; 3] {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let h = hue * 6.0;
    let x = chroma * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = lightness - chroma / 2.0;
    [r + m, g + m, b + m]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_colors_follow_the_cover_dark_enough_for_white_text() {
        let image = |pixels: &[[u8; 3]]| -> Vec<u8> { pixels.concat() };
        // Mostly grey with a red figure: red wins.
        let mut pixels = vec![[128, 128, 128]; 90];
        pixels.extend([[220, 30, 30]; 10]);
        let [r, g, b] = page_color(&image(&pixels));
        assert!(r > g + 40 && r > b + 40, "{r} {g} {b}");
        // Dark enough for white text, whatever the cover.
        for color in [[250, 240, 120], [20, 40, 200], [255, 255, 255], [0, 0, 0]] {
            let [r, g, b] = page_color(&image(&[color; 4]));
            let luminance = 0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b);
            assert!(luminance < 110.0, "{color:?} gave {r} {g} {b}");
        }
        // Grey stays grey; no pixels at all give Spotify's grey.
        let [r, g, b] = page_color(&image(&[[100, 100, 100]; 4]));
        assert!(r == g && g == b);
        assert_eq!(page_color(&[]), [83, 83, 83]);
    }

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
