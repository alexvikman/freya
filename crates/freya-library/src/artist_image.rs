//! Pictures of artists, for the library's grid of artists: one in the artist's own folder,
//! as Plex and Kodi read them; else a photo from Deezer, when the user allows it; else the
//! artist's album covers, four in a square or the first alone.

use std::path::{Path, PathBuf};
use std::time::Duration;

use image::{DynamicImage, RgbImage, imageops::FilterType};

use crate::cover::read_cover;

/// Names of an artist's picture in their folder, the one holding their albums' folders.
const ARTIST_FILES: &[&str] = &["artist", "poster", "folder"];
/// And in an album's folder, where `folder` is the album's cover.
const ALBUM_FOLDER_FILES: &[&str] = &["artist"];
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png"];

const DEEZER_SEARCH: &str = "https://api.deezer.com/search/artist";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10);
/// A search answer is at most this large, and a picture this large, to keep a broken answer
/// from filling memory.
const MAX_ANSWER_BYTES: u64 = 256 * 1024;
const MAX_PICTURE_BYTES: u64 = 4 * 1024 * 1024;

/// A picture of the artist next to their music: in their own folder, the parent of their
/// albums' folders, or in an album's folder as `artist.jpg`. `album_tracks` has a track of
/// each of their albums.
pub fn local(album_tracks: &[PathBuf]) -> Option<PathBuf> {
    let album_dirs: Vec<&Path> = album_tracks.iter().filter_map(|t| t.parent()).collect();
    let in_dir = |dir: &Path, names: &[&str]| {
        names.iter().find_map(|name| {
            IMAGE_EXTENSIONS
                .iter()
                .map(|ext| dir.join(format!("{name}.{ext}")))
                .find(|path| path.is_file())
        })
    };
    album_dirs
        .iter()
        .filter_map(|dir| dir.parent())
        .find_map(|dir| in_dir(dir, ARTIST_FILES))
        .or_else(|| {
            album_dirs
                .iter()
                .find_map(|dir| in_dir(dir, ALBUM_FOLDER_FILES))
        })
}

/// What Deezer has for an artist.
#[derive(Debug, PartialEq)]
pub enum Online {
    Picture(Vec<u8>),
    /// Deezer knows no picture of an artist of that name; asking again will not change it.
    NoPicture,
}

/// A photo of the artist `name` from Deezer's public catalogue, which needs no key. Only an
/// artist of the same name counts, not the nearest match. An error means Deezer could not
/// be asked, and may be asked again later.
pub fn deezer(name: &str) -> Result<Online, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .user_agent(concat!("Freya/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let text = agent
        .get(DEEZER_SEARCH)
        .query("q", name)
        .query("limit", "5")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(MAX_ANSWER_BYTES)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let answer: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let Some(url) = picture_url(&answer, name) else {
        return Ok(Online::NoPicture);
    };
    let bytes = agent
        .get(&url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(MAX_PICTURE_BYTES)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    Ok(Online::Picture(bytes))
}

/// The address of the largest picture of the artist named `name` in a Deezer search answer.
fn picture_url(answer: &serde_json::Value, name: &str) -> Option<String> {
    let wanted = comparable(name);
    let artist =
        answer.get("data")?.as_array()?.iter().find(|a| {
            a.get("name").and_then(|n| n.as_str()).map(comparable) == Some(wanted.clone())
        })?;
    let url = ["picture_xl", "picture_big"]
        .iter()
        .find_map(|key| artist.get(key)?.as_str())?;
    // Artists without a picture have one whose name in the address is empty.
    (!url.contains("/artist//")).then(|| url.to_owned())
}

/// A name with only its letters and digits, in lower case: "AC/DC" and "ac dc" are one.
fn comparable(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The artist's album covers as one square picture `size` wide: four in a square, or the
/// first alone when there are fewer.
pub fn collage(album_tracks: &[PathBuf], size: u32) -> Option<RgbImage> {
    let half = size / 2;
    let covers: Vec<DynamicImage> = album_tracks
        .iter()
        .filter_map(|track| read_cover(track, size))
        .filter_map(|c| image::RgbaImage::from_raw(c.width, c.height, c.rgba))
        .map(DynamicImage::ImageRgba8)
        .take(4)
        .collect();
    match covers.len() {
        0 => None,
        1..=3 => Some(
            covers[0]
                .resize_to_fill(size, size, FilterType::Triangle)
                .to_rgb8(),
        ),
        _ => {
            let mut square = RgbImage::new(size, size);
            for (i, cover) in covers.iter().enumerate() {
                let tile = cover
                    .resize_to_fill(half, half, FilterType::Triangle)
                    .to_rgb8();
                let (x, y) = ((i as u32 % 2) * half, (i as u32 / 2) * half);
                image::imageops::replace(&mut square, &tile, i64::from(x), i64::from(y));
            }
            Some(square)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("freya-artist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_the_picture_in_the_artists_folder() {
        let artist = temp_dir("local");
        let album = artist.join("Homesick");
        std::fs::create_dir_all(&album).unwrap();
        let track = album.join("01.flac");
        std::fs::write(&track, b"").unwrap();
        // An album's own cover is not the artist's.
        std::fs::write(album.join("folder.jpg"), b"x").unwrap();
        assert_eq!(local(std::slice::from_ref(&track)), None);
        std::fs::write(album.join("artist.png"), b"x").unwrap();
        assert_eq!(
            local(std::slice::from_ref(&track)),
            Some(album.join("artist.png"))
        );
        // The artist's folder comes first.
        std::fs::write(artist.join("folder.jpg"), b"x").unwrap();
        assert_eq!(local(&[track]), Some(artist.join("folder.jpg")));
    }

    #[test]
    fn takes_only_the_artist_of_that_name_from_deezer() {
        let answer = serde_json::json!({ "data": [
            { "name": "Shores of Null Tribute", "picture_xl": "https://x/artist/aaa/1000.jpg" },
            { "name": "Shores Of Null", "picture_xl": "https://x/artist/bbb/1000.jpg" },
        ]});
        assert_eq!(
            picture_url(&answer, "shores of null").as_deref(),
            Some("https://x/artist/bbb/1000.jpg")
        );
        assert_eq!(picture_url(&answer, "Opeth"), None);
        let blank = serde_json::json!({ "data": [
            { "name": "Opeth", "picture_xl": "https://x/images/artist//1000x1000.jpg" },
        ]});
        assert_eq!(picture_url(&blank, "Opeth"), None, "Deezer's empty picture");
        assert_eq!(comparable("AC/DC"), comparable("ac dc"));
    }

    #[test]
    fn makes_a_square_of_four_covers_or_takes_the_first() {
        let dir = temp_dir("collage");
        let tracks: Vec<PathBuf> = (0..4)
            .map(|i| {
                let album = dir.join(format!("album{i}"));
                std::fs::create_dir_all(&album).unwrap();
                let shade = 60 * i as u8;
                image::RgbImage::from_pixel(400, 300, image::Rgb([shade, 0, 0]))
                    .save(album.join("cover.jpg"))
                    .unwrap();
                let track = album.join("01.flac");
                std::fs::write(&track, b"").unwrap();
                track
            })
            .collect();
        let square = collage(&tracks, 300).unwrap();
        assert_eq!(square.dimensions(), (300, 300));
        // Each quarter is one cover: the bottom right is the fourth, the reddest.
        assert!(square.get_pixel(225, 225)[0] > square.get_pixel(75, 75)[0] + 100);
        let single = collage(&tracks[..2], 300).unwrap();
        assert_eq!(single.dimensions(), (300, 300));
        assert!(collage(&[], 300).is_none());
    }
}
