//! Cover art: from a track's tags, or from an image next to it in the folder.

use std::path::Path;

use lofty::picture::PictureType;
use lofty::prelude::*;

/// File names (without extension) that commonly hold an album's cover, in order of preference.
const FOLDER_IMAGES: [&str; 5] = ["cover", "folder", "front", "album", "artwork"];
const IMAGE_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];
/// Subfolders (lowercase) that rips often keep the artwork in.
pub const ARTWORK_FOLDERS: [&str; 4] = ["cover", "covers", "artwork", "scans"];

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
    cover_from(&bytes, max_size)
}

/// The cover in the picture `bytes`, at most `max_size` wide or high.
pub fn cover_from(bytes: &[u8], max_size: u32) -> Option<Cover> {
    let image = image::load_from_memory(bytes).ok()?;
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

/// The longest side of the cover kept for an album: sharp on its page on a Retina screen.
pub const ALBUM_ART: u32 = 600;

/// An album's cover from `track`, one of its tracks, as Freya keeps it: at most
/// [`ALBUM_ART`] wide or high, as a JPEG, and a color from it for the album's page. None when
/// the album has no cover.
pub fn album_art(track: &Path) -> Option<(Vec<u8>, [u8; 3])> {
    let bytes = embedded_cover(track).or_else(|| folder_cover(track))?;
    album_art_from(&bytes)
}

/// An album's cover from `track`, one of its tracks, as large as it is up to `largest` wide
/// or high, to show whole: read from the music's file or folder.
pub fn album_art_large(track: &Path, largest: u32) -> Option<image::DynamicImage> {
    let bytes = embedded_cover(track).or_else(|| folder_cover(track))?;
    let image = image::load_from_memory(&bytes).ok()?;
    Some(if image.width() > largest || image.height() > largest {
        image.resize(largest, largest, image::imageops::FilterType::Lanczos3)
    } else {
        image
    })
}

/// An album's cover from the picture `bytes`, as Freya keeps it: at most [`ALBUM_ART`] wide
/// or high, as a JPEG, and a color from it for the album's page.
pub fn album_art_from(bytes: &[u8]) -> Option<(Vec<u8>, [u8; 3])> {
    let image = image::load_from_memory(bytes).ok()?;
    let image = if image.width() > ALBUM_ART || image.height() > ALBUM_ART {
        image.resize(ALBUM_ART, ALBUM_ART, image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    let rgb = image.to_rgb8();
    let color = page_color(
        image::DynamicImage::ImageRgb8(rgb.clone())
            .thumbnail(64, 64)
            .to_rgb8()
            .as_raw(),
    );
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 88)
        .encode_image(&rgb)
        .ok()?;
    Some((jpeg, color))
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

/// An image next to the track, or else in an artwork subfolder such as "Covers": within the
/// track's folder, after links, so one leading out of it reads nothing elsewhere.
fn folder_cover(track: &Path) -> Option<Vec<u8>> {
    let dir = track.parent()?;
    let within = dir.canonicalize().ok()?;
    image_in(dir, &within).or_else(|| {
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
            .find_map(|folder| image_in(&folder, &within))
    })
}

/// In `dir`: an image with a usual cover name, or else the folder's only image; read only when
/// it is within `within`, after links.
fn image_in(dir: &Path, within: &Path) -> Option<Vec<u8>> {
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
    let real = chosen.canonicalize().ok()?;
    if !real.starts_with(within) || !std::fs::metadata(&real).ok()?.is_file() {
        return None;
    }
    std::fs::read(real).ok()
}

/// A color for an album's page, from its cover's pixels (RGB, 3 bytes each): its most
/// prominent vivid hue, where bright, saturated pixels count most, as a window of light in a
/// dark picture rather than the dark around it; not an average, which turns blue and yellow
/// to mud. Then made dark enough for white text. A grey cover, or one with only a touch of
/// color, gives grey.
pub fn page_color(rgb: &[u8]) -> [u8; 3] {
    const HUES: usize = 24;
    let mut weights = [0.0f64; HUES];
    let mut sums = [[0.0f64; 3]; HUES];
    let mut all = 0.0;
    for pixel in rgb.as_chunks::<3>().0 {
        let [r, g, b] = pixel.map(|c| f64::from(c) / 255.0);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        // Bright pixels count most; white ones hardly.
        let presence = max.powi(3) * (1.0 - min).max(0.05);
        all += presence;
        let saturation = if max > 0.0 { (max - min) / max } else { 0.0 };
        if saturation < 0.2 || max < 0.12 {
            continue;
        }
        let (hue, ..) = hsl(r, g, b);
        let at = ((hue * HUES as f64) as usize).min(HUES - 1);
        let weight = saturation * presence;
        weights[at] += weight;
        for (sum, c) in sums[at].iter_mut().zip([r, g, b]) {
            *sum += c * weight;
        }
    }
    // A hue with its neighbours, half as much.
    let around = |at: usize| [(at + HUES - 1) % HUES, at, (at + 1) % HUES];
    let score = |at: usize| {
        let [before, at, after] = around(at);
        weights[before] / 2.0 + weights[at] + weights[after] / 2.0
    };
    let best = (0..HUES).max_by(|a, b| score(*a).total_cmp(&score(*b)));
    let picked = best.map(|best| {
        let mut sum = [0.0; 3];
        let mut weight = 0.0;
        for at in around(best) {
            weight += weights[at];
            for (s, c) in sum.iter_mut().zip(sums[at]) {
                *s += c;
            }
        }
        (sum, weight)
    });
    let Some((sum, weight)) = picked.filter(|(_, weight)| *weight > 0.0) else {
        return [83, 83, 83];
    };
    let [r, g, b] = sum.map(|c| c / weight);
    let (hue, saturation, _) = hsl(r, g, b);
    // How much of the cover is vivid: a touch of color in a grey cover leaves it grey.
    let vivid = ((weights.iter().sum::<f64>() / all - 0.05) / (0.12 - 0.05)).clamp(0.0, 1.0);
    let vivid = vivid * vivid * (3.0 - 2.0 * vivid);
    let mut rgb = from_hsl(hue, saturation.min(0.6) * vivid, 0.42);
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
const MAX_LUMINANCE: f64 = 0.34;

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
        // Grey stays grey; no pixels at all give a neutral grey.
        assert_eq!(page_color(&image(&[[100, 100, 100]; 4])), [83, 83, 83]);
        assert_eq!(page_color(&[]), [83, 83, 83]);
        // A bright window in a dark blue picture: the window's yellow, not a mix.
        let mut night = vec![[20, 30, 70]; 85];
        night.extend([[220, 220, 110]; 15]);
        let [r, g, b] = page_color(&image(&night));
        assert!(r > b + 30 && g > b + 30, "{r} {g} {b}");
        // A touch of color in a grey cover leaves it grey.
        let mut grey = vec![[150, 150, 150]; 99];
        grey.push([200, 40, 40]);
        let [r, g, b] = page_color(&image(&grey));
        assert!(r.abs_diff(g) < 12 && g.abs_diff(b) < 12, "{r} {g} {b}");
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
