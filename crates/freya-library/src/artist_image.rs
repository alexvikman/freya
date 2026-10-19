//! Pictures of artists, for the library's grid of artists: one in the artist's own folder,
//! under the names media players commonly use; else a photo from Deezer, when the user allows it; else the
//! artist's album covers, four in a square or the first alone.
//!
//! And wide ones across the top of an artist's page: in their folder, else from fanart.tv
//! with the user's own key, found by the artist's MusicBrainz id.

use std::path::{Path, PathBuf};
use std::time::Duration;

use image::{DynamicImage, RgbImage, imageops::FilterType};

/// Names of an artist's picture in their folder, the one holding their albums' folders.
const ARTIST_FILES: &[&str] = &["artist", "poster", "folder"];
/// And in an album's folder, where `folder` is the album's cover.
const ALBUM_FOLDER_FILES: &[&str] = &["artist"];
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png"];
/// Names of a wide picture in the artist's folder, as media players commonly use them.
const BACKDROP_FILES: &[&str] = &["fanart", "backdrop", "background"];

const DEEZER_SEARCH: &str = "https://api.deezer.com/search/artist";
/// In the address of Deezer's picture for an artist without one.
const NO_PICTURE: &str = "d41d8cd98f00b204e9800998ecf8427e";
const MUSICBRAINZ_SEARCH: &str = "https://musicbrainz.org/ws/2/artist/";
const MUSICBRAINZ_ALBUMS: &str = "https://musicbrainz.org/ws/2/release-group/";
const FANART: &str = "https://webservice.fanart.tv/v3/music";
/// MusicBrainz asks for at most one request a second, and for a way to reach the program's
/// makers in its user agent.
pub const MUSICBRAINZ_INTERVAL: Duration = Duration::from_millis(1100);
const USER_AGENT: &str = concat!(
    "Freya/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/alexvikman/freya )"
);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10);
/// A search answer is at most this large, and a picture this large, to keep a broken answer
/// from filling memory.
const MAX_ANSWER_BYTES: u64 = 256 * 1024;
/// fanart.tv's wide pictures can be 3840 pixels wide.
const MAX_PICTURE_BYTES: u64 = 16 * 1024 * 1024;

/// The folders holding the artist's albums' folders, each once: an artist's albums are
/// mostly in one, and looking in a folder on a network drive takes a while.
fn artist_dirs(album_tracks: &[PathBuf]) -> Vec<&Path> {
    let mut dirs: Vec<&Path> = Vec::new();
    for dir in album_tracks.iter().filter_map(|t| t.parent()?.parent()) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// The pictures a walk through the music's folders found, to look among by name, in any case,
/// as the file system does on macOS and Windows: without looking in the folders again.
pub struct FolderImages<'a> {
    by_name: std::collections::HashMap<String, &'a Path>,
}

impl<'a> FolderImages<'a> {
    pub fn new(images: &'a [PathBuf]) -> Self {
        Self {
            by_name: images
                .iter()
                .map(|image| (image.to_string_lossy().to_lowercase(), image.as_path()))
                .collect(),
        }
    }

    /// The first of `names`, with any of the image extensions, in `dir`.
    fn in_dir(&self, dir: &Path, names: &[&str]) -> Option<PathBuf> {
        names.iter().find_map(|name| {
            IMAGE_EXTENSIONS.iter().find_map(|ext| {
                let path = dir.join(format!("{name}.{ext}"));
                let found = self.by_name.get(&path.to_string_lossy().to_lowercase())?;
                Some(found.to_path_buf())
            })
        })
    }

    /// As [`local`]: a picture of the artist next to their music.
    pub fn portrait(&self, album_tracks: &[PathBuf]) -> Option<PathBuf> {
        artist_dirs(album_tracks)
            .into_iter()
            .find_map(|dir| self.in_dir(dir, ARTIST_FILES))
            .or_else(|| {
                album_tracks
                    .iter()
                    .filter_map(|t| t.parent())
                    .find_map(|dir| self.in_dir(dir, ALBUM_FOLDER_FILES))
            })
    }

    /// As [`local_backdrop`]: a wide picture of the artist in their own folder.
    pub fn backdrop(&self, album_tracks: &[PathBuf]) -> Option<PathBuf> {
        artist_dirs(album_tracks)
            .into_iter()
            .find_map(|dir| self.in_dir(dir, BACKDROP_FILES))
    }
}

/// Busy services answer 503 now and then, MusicBrainz often: each wait, the request is made
/// again.
const BUSY_WAITS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];

/// Makes the request `call` makes, again after a wait while the service is busy.
fn call_patiently(
    call: impl Fn() -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    for wait in BUSY_WAITS {
        match call() {
            Err(ureq::Error::StatusCode(503)) => std::thread::sleep(wait),
            answer => return answer,
        }
    }
    call()
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .user_agent(USER_AGENT)
        .build()
        .into()
}

/// What Deezer or fanart.tv has for an artist.
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
    match deezer_picture_urls(name)?.first() {
        Some(url) => Ok(Online::Picture(download(url)?)),
        None => Ok(Online::NoPicture),
    }
}

/// The addresses of Deezer's pictures of artists named `name`: several share a name now and
/// then. Best matches first.
pub fn deezer_picture_urls(name: &str) -> Result<Vec<String>, String> {
    let text = agent()
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
    Ok(picture_urls(&answer, name))
}

/// The front cover of the release group `group` from the Cover Art Archive, as large as it
/// keeps it up to 1200 pixels; none when it has none.
pub fn cover_art(group: &str) -> Result<Option<Vec<u8>>, String> {
    let url = format!("https://coverartarchive.org/release-group/{group}/front-1200");
    match agent().get(&url).call() {
        Ok(mut answer) => answer
            .body_mut()
            .with_config()
            .limit(MAX_PICTURE_BYTES)
            .read_to_vec()
            .map(Some)
            .map_err(|e| e.to_string()),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Downloads a picture at `url`.
pub fn download(url: &str) -> Result<Vec<u8>, String> {
    agent()
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(MAX_PICTURE_BYTES)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

/// MusicBrainz's id of the artist named `name`: only an artist of that very name counts, the
/// best match of them. None when MusicBrainz knows no such artist. Callers wait
/// [`MUSICBRAINZ_INTERVAL`] between calls.
pub fn musicbrainz_id(name: &str) -> Result<Option<String>, String> {
    // Quoted, as Lucene reads a query, so a name is not taken for its syntax.
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    let answer = musicbrainz_search(&format!("artist:\"{escaped}\""))?;
    Ok(artist_id(&answer, name))
}

/// An artist MusicBrainz knows, with what tells them from others of the same name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArtistMatch {
    pub mbid: String,
    pub name: String,
    /// MusicBrainz's words to tell them apart: "Norwegian jazz/metal/rock group".
    pub disambiguation: String,
    /// "Group", "Person".
    pub kind: String,
    /// Where they are from: "Norway".
    pub area: String,
    /// When they began, and ended, as MusicBrainz has it: "1999", "1999-05".
    pub begin: String,
    pub end: String,
}

/// The artists MusicBrainz finds for the words in `query`, the best matches first.
pub fn musicbrainz_artists(query: &str) -> Result<Vec<ArtistMatch>, String> {
    // Every character Lucene would read as syntax is escaped.
    let escaped: String = query
        .chars()
        .flat_map(|c| {
            let special = "+-&|!(){}[]^\"~*?:\\/".contains(c);
            special
                .then_some('\\')
                .into_iter()
                .chain(std::iter::once(c))
        })
        .collect();
    Ok(artist_matches(&musicbrainz_search(escaped.trim())?))
}

/// An album MusicBrainz knows (a release group), with the artist who made it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlbumCandidate {
    pub release_group: String,
    pub title: String,
    /// "Album", "EP", "Single".
    pub kind: String,
    /// When it first came out: "2010-01-18".
    pub date: String,
    pub artist: ArtistMatch,
}

/// The albums MusicBrainz finds titled `title`, by an artist named `artist` when it is not
/// empty, the best matches first.
pub fn musicbrainz_albums(title: &str, artist: &str) -> Result<Vec<AlbumCandidate>, String> {
    let quoted = |text: &str| {
        let escaped = text.trim().replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    };
    let mut query = format!("releasegroup:{}", quoted(title));
    if !artist.trim().is_empty() {
        query = format!("{query} AND artist:{}", quoted(artist));
    }
    Ok(album_candidates(&musicbrainz_query(
        MUSICBRAINZ_ALBUMS,
        &query,
    )?))
}

fn album_candidates(answer: &serde_json::Value) -> Vec<AlbumCandidate> {
    let text = |value: Option<&serde_json::Value>| {
        value
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    answer
        .get("release-groups")
        .and_then(|g| g.as_array())
        .into_iter()
        .flatten()
        .filter_map(|group| {
            // The first credited artist: the one the album is filed under.
            let credit = group.get("artist-credit")?.as_array()?.first()?;
            let artist = credit.get("artist")?;
            Some(AlbumCandidate {
                release_group: group.get("id")?.as_str()?.to_owned(),
                title: text(group.get("title")),
                kind: text(group.get("primary-type")),
                date: text(group.get("first-release-date")),
                artist: ArtistMatch {
                    mbid: artist.get("id")?.as_str()?.to_owned(),
                    name: text(artist.get("name")),
                    disambiguation: text(artist.get("disambiguation")),
                    ..ArtistMatch::default()
                },
            })
        })
        .collect()
}

/// The JSON answer at `address` with the `query` given, asked again while the service is busy.
pub(crate) fn get_json(address: &str, query: &[(&str, &str)]) -> Result<serde_json::Value, String> {
    let agent = agent();
    let text = call_patiently(|| {
        let mut request = agent.get(address);
        for (name, value) in query {
            request = request.query(*name, *value);
        }
        request.call()
    })
    .map_err(|e| e.to_string())?
    .body_mut()
    .with_config()
    .limit(MAX_ANSWER_BYTES)
    .read_to_string()
    .map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn musicbrainz_search(query: &str) -> Result<serde_json::Value, String> {
    musicbrainz_query(MUSICBRAINZ_SEARCH, query)
}

fn musicbrainz_query(address: &str, query: &str) -> Result<serde_json::Value, String> {
    let agent = agent();
    let text = call_patiently(|| {
        agent
            .get(address)
            .query("query", query)
            .query("fmt", "json")
            .query("limit", "10")
            .call()
    })
    .map_err(|e| e.to_string())?
    .body_mut()
    .with_config()
    .limit(MAX_ANSWER_BYTES)
    .read_to_string()
    .map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn artist_matches(answer: &serde_json::Value) -> Vec<ArtistMatch> {
    let text = |value: Option<&serde_json::Value>| {
        value
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    answer
        .get("artists")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|artist| {
            let life = artist.get("life-span");
            Some(ArtistMatch {
                mbid: artist.get("id")?.as_str()?.to_owned(),
                name: text(artist.get("name")),
                disambiguation: text(artist.get("disambiguation")),
                kind: text(artist.get("type")),
                area: text(artist.get("area").and_then(|a| a.get("name"))),
                begin: text(life.and_then(|l| l.get("begin"))),
                end: text(life.and_then(|l| l.get("end"))),
            })
        })
        .collect()
}

/// The id of the best matching artist named `name` in a MusicBrainz search answer, which
/// lists the best matches first.
fn artist_id(answer: &serde_json::Value, name: &str) -> Option<String> {
    let wanted = comparable(name);
    answer
        .get("artists")?
        .as_array()?
        .iter()
        .find(|a| a.get("name").and_then(|n| n.as_str()).map(comparable) == Some(wanted.clone()))?
        .get("id")?
        .as_str()
        .map(str::to_owned)
}

/// The addresses of every picture fanart.tv has of the artist with the MusicBrainz id
/// `mbid`: portraits, and wide ones, 4K first. Best liked first.
#[derive(Debug, Default, PartialEq)]
pub struct FanartUrls {
    pub portraits: Vec<String>,
    pub backdrops: Vec<String>,
}

pub fn fanart_urls(mbid: &str, key: &str) -> Result<FanartUrls, String> {
    Ok(fanart_answer(mbid, key)?
        .map(|answer| urls_of(&answer))
        .unwrap_or_default())
}

fn urls_of(answer: &serde_json::Value) -> FanartUrls {
    let list = |kinds: &[&str]| -> Vec<String> {
        kinds
            .iter()
            .filter_map(|kind| answer.get(kind))
            .flat_map(by_likes)
            .collect()
    };
    FanartUrls {
        portraits: list(&["artistthumb"]),
        backdrops: list(&["artist4kbackground", "artistbackground"]),
    }
}

/// What fanart.tv has of the artist `mbid`; none when it knows nothing of them.
fn fanart_answer(mbid: &str, key: &str) -> Result<Option<serde_json::Value>, String> {
    let agent = agent();
    let response = call_patiently(|| {
        agent
            .get(format!("{FANART}/{mbid}"))
            .query("client_key", key)
            .call()
    });
    let text = match response {
        Ok(mut response) => response
            .body_mut()
            .with_config()
            .limit(MAX_ANSWER_BYTES)
            .read_to_string()
            .map_err(|e| e.to_string())?,
        // fanart.tv has nothing of this artist.
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(ureq::Error::StatusCode(401 | 403)) => {
            return Err("fanart.tv did not take the API key".to_owned());
        }
        Err(e) => return Err(e.to_string()),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// The addresses in a list of fanart.tv pictures, best liked first; those liked as much as
/// fanart.tv lists them, as its users ranked them.
fn by_likes(pictures: &serde_json::Value) -> Vec<String> {
    let likes = |picture: &serde_json::Value| {
        let likes = picture.get("likes");
        likes
            .and_then(serde_json::Value::as_u64)
            .or_else(|| likes?.as_str()?.parse().ok())
            .unwrap_or(0)
    };
    let mut pictures: Vec<(u64, String)> = pictures
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((likes(p), p.get("url")?.as_str()?.to_owned())))
        .collect();
    // Stable: equals keep their order.
    pictures.sort_by_key(|(likes, _)| std::cmp::Reverse(*likes));
    pictures.into_iter().map(|(_, url)| url).collect()
}

/// The addresses of the largest pictures of artists named `name` in a Deezer search answer.
fn picture_urls(answer: &serde_json::Value, name: &str) -> Vec<String> {
    let wanted = comparable(name);
    answer
        .get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter(|a| a.get("name").and_then(|n| n.as_str()).map(comparable) == Some(wanted.clone()))
        .filter_map(|artist| {
            ["picture_xl", "picture_big"]
                .iter()
                .find_map(|key| artist.get(key)?.as_str())
        })
        // Artists without a picture have one whose name in the address is empty, or the
        // MD5 of nothing: a grey figure.
        .filter(|url| !url.contains("/artist//") && !url.contains(NO_PICTURE))
        .map(str::to_owned)
        .collect()
}

/// A name with only its letters and digits, in lower case: "AC/DC" and "ac dc" are one.
fn comparable(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// `covers` as one square picture `size` wide: the first four in a square, or the first alone
/// when there are fewer.
pub fn collage_of(covers: Vec<DynamicImage>, size: u32) -> Option<RgbImage> {
    let half = size / 2;
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

    #[test]
    fn takes_only_the_artist_of_that_name_from_deezer() {
        let answer = serde_json::json!({ "data": [
            { "name": "Shores of Null Tribute", "picture_xl": "https://x/artist/aaa/1000.jpg" },
            { "name": "Shores Of Null", "picture_xl": "https://x/artist/bbb/1000.jpg" },
        ]});
        assert_eq!(
            picture_urls(&answer, "shores of null"),
            ["https://x/artist/bbb/1000.jpg"]
        );
        assert!(picture_urls(&answer, "Opeth").is_empty());
        let blank = serde_json::json!({ "data": [
            { "name": "Opeth", "picture_xl": "https://x/images/artist//1000x1000.jpg" },
        ]});
        assert!(
            picture_urls(&blank, "Opeth").is_empty(),
            "Deezer's empty picture"
        );
        let figure = serde_json::json!({ "data": [
            { "name": "Shining",
              "picture_xl": "https://x/images/artist/d41d8cd98f00b204e9800998ecf8427e/1000x1000.jpg" },
            { "name": "Shining", "picture_xl": "https://x/images/artist/abae37bc/1000x1000.jpg" },
        ]});
        assert_eq!(
            picture_urls(&figure, "Shining"),
            ["https://x/images/artist/abae37bc/1000x1000.jpg"],
            "Deezer's grey figure"
        );
        assert_eq!(comparable("AC/DC"), comparable("ac dc"));
    }

    #[test]
    fn finds_artists_pictures_among_those_a_walk_found() {
        let tracks = [PathBuf::from("/m/Opeth/Damnation/01.flac")];
        let images = [
            PathBuf::from("/m/Opeth/Damnation/folder.jpg"),
            PathBuf::from("/m/Opeth/Artist.JPG"),
            PathBuf::from("/m/Opeth/fanart.png"),
        ];
        let found = FolderImages::new(&images);
        assert_eq!(found.portrait(&tracks), Some(images[1].clone()));
        assert_eq!(found.backdrop(&tracks), Some(images[2].clone()));
        let none = FolderImages::new(&images[..1]);
        assert_eq!(
            none.portrait(&tracks),
            None,
            "an album's own cover is not the artist's"
        );
    }

    #[test]
    fn takes_only_the_artist_of_that_name_from_musicbrainz() {
        let answer = serde_json::json!({ "artists": [
            { "id": "aaa", "name": "Anna von Hausswolff Tribute", "score": 100 },
            { "id": "bbb", "name": "Anna Von Hausswolff", "score": 98 },
            { "id": "ccc", "name": "Anna von Hausswolff", "score": 60 },
        ]});
        assert_eq!(
            artist_id(&answer, "Anna von Hausswolff").as_deref(),
            Some("bbb")
        );
        assert_eq!(artist_id(&answer, "Opeth"), None);
    }

    #[test]
    fn reads_albums_with_the_artist_who_made_them() {
        let answer = serde_json::json!({ "release-groups": [
            { "id": "rg", "title": "Blackjazz", "primary-type": "Album",
              "first-release-date": "2010-01-18",
              "artist-credit": [{ "name": "Shining", "artist": {
                  "id": "no", "name": "Shining",
                  "disambiguation": "Norwegian jazz/metal/rock group" } }] },
            { "id": "no-credit", "title": "Lost" },
        ]});
        let albums = album_candidates(&answer);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].artist.mbid, "no");
        assert_eq!(albums[0].date, "2010-01-18");
        assert_eq!(
            albums[0].artist.disambiguation,
            "Norwegian jazz/metal/rock group"
        );
    }

    #[test]
    fn tells_artists_of_one_name_apart() {
        let answer = serde_json::json!({ "artists": [
            { "id": "se", "name": "Shining", "type": "Group",
              "disambiguation": "Swedish doom / depressive black metal band",
              "area": { "name": "Sweden" }, "life-span": { "begin": "1996", "ended": null } },
            { "id": "no", "name": "Shining", "area": { "name": "Norway" } },
            { "name": "Without an id" },
        ]});
        let matches = artist_matches(&answer);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].area, "Sweden");
        assert_eq!(matches[0].begin, "1996");
        assert_eq!(matches[1].disambiguation, "");
        assert_eq!(matches[1].mbid, "no");
    }

    #[test]
    fn lists_fanart_tvs_pictures_best_liked_first() {
        let answer = serde_json::json!({
            "artistthumb": [{ "url": "https://x/thumb.jpg", "likes": "9" }],
            "artistbackground": [
                { "url": "https://x/a.jpg", "likes": "1" },
                { "url": "https://x/b.jpg", "likes": "4" },
                { "url": "https://x/c.jpg", "likes": "4" },
            ],
        });
        assert_eq!(urls_of(&answer).backdrops[0], "https://x/b.jpg");
        let mut with_4k = answer.clone();
        with_4k["artist4kbackground"] =
            serde_json::json!([{ "url": "https://x/4k.jpg", "likes": "0" }]);
        let urls = urls_of(&with_4k);
        assert_eq!(urls.portraits, ["https://x/thumb.jpg"]);
        assert_eq!(
            urls.backdrops,
            [
                "https://x/4k.jpg",
                "https://x/b.jpg",
                "https://x/c.jpg",
                "https://x/a.jpg"
            ]
        );
        assert!(
            urls_of(&serde_json::json!({ "artistthumb": [] }))
                .backdrops
                .is_empty()
        );
    }

    #[test]
    fn makes_a_square_of_four_covers_or_takes_the_first() {
        let covers: Vec<DynamicImage> = (0..4)
            .map(|i| {
                let shade = 60 * i as u8;
                DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                    400,
                    300,
                    image::Rgb([shade, 0, 0]),
                ))
            })
            .collect();
        let square = collage_of(covers.clone(), 300).unwrap();
        assert_eq!(square.dimensions(), (300, 300));
        // Each quarter is one cover: the bottom right is the fourth, the reddest.
        assert!(square.get_pixel(225, 225)[0] > square.get_pixel(75, 75)[0] + 100);
        let single = collage_of(covers[..2].to_vec(), 300).unwrap();
        assert_eq!(single.dimensions(), (300, 300));
        assert!(collage_of(Vec::new(), 300).is_none());
    }
}
