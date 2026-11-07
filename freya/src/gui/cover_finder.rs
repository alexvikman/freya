//! Covers to choose from when the user changes an album's: the one it shows now, then each of
//! its editions' front covers in the Cover Art Archive and fanart.tv's covers of it, found on
//! a thread of its own. Each found is kept among the pictures at once, so choosing it needs no
//! second download; those not chosen are taken away later, as no album has them. The same
//! picture found twice, as editions often share one, is shown once.

use std::collections::{HashSet, VecDeque};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use freya_media::artist_image;
use freya_protocol::LibraryApi;
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// At most this many from each source: an album can have dozens of editions.
const MAX_EACH: usize = 12;
/// The longest side of a preview, in pixels: sharp in a 160-point tile on a Retina screen.
const PREVIEW: u32 = 360;
/// Pictures downloaded at once.
const WORKERS: usize = 4;

/// A cover found to choose from.
pub struct FoundCover {
    /// The picture, kept among the library's, by its SHA-256.
    pub checksum: String,
    /// Where it is from: "Shown now", "Cover Art Archive · 2007 · US", "fanart.tv".
    pub source: String,
    pub width: u32,
    pub height: u32,
    pub preview: SharedPixelBuffer<Rgba8Pixel>,
    /// A color from it for the album's page.
    pub color: [u8; 3],
    /// Chosen as soon as it is found: one the user gave.
    pub choose: bool,
    /// Where it goes among those found: they come in any order.
    pub order: usize,
}

/// What to look for.
pub struct CoverRequest {
    pub title: String,
    pub artist: String,
    /// The album's release group in MusicBrainz, when it was matched to one; else it is
    /// searched for by the title and artist.
    pub group: Option<String>,
    pub fanart_key: Option<String>,
    /// Covers already kept, with where they came from.
    pub kept: Vec<(String, String)>,
    /// Whether the internet is asked: the user asked for covers to choose from, so it is,
    /// but for in tests.
    pub online: bool,
}

enum Message {
    Found(u64, FoundCover),
    Done(u64),
    Failed(u64, String),
}

pub struct CoverFinder {
    pictures: Option<Arc<dyn LibraryApi>>,
    /// Counted up for each search, so a thread still running for an earlier one is ignored.
    generation: u64,
    searching: bool,
    results: (Sender<Message>, Receiver<Message>),
}

impl CoverFinder {
    pub fn new(pictures: Option<Arc<dyn LibraryApi>>) -> Self {
        Self {
            pictures,
            generation: 0,
            searching: false,
            results: channel(),
        }
    }

    /// Starts looking for covers, forgetting an earlier search.
    pub fn find(&mut self, request: CoverRequest) {
        self.generation += 1;
        let Some(pictures) = self.pictures.clone() else {
            return;
        };
        self.searching = true;
        let (generation, results) = (self.generation, self.results.0.clone());
        let started = std::thread::Builder::new()
            .name("freya-cover-finder".into())
            .spawn(move || {
                look(&request, pictures.as_ref(), &|found| {
                    let _ = results.send(Message::Found(generation, found));
                });
                let _ = results.send(Message::Done(generation));
            });
        if let Err(e) = started {
            log::warn!("could not look for covers: {e}");
            self.searching = false;
        }
    }

    /// Downloads the picture at `url` as a cover to choose, chosen when it comes.
    pub fn download(&mut self, url: String) {
        let Some(pictures) = self.pictures.clone() else {
            return;
        };
        let (generation, results) = (self.generation, self.results.0.clone());
        let started = std::thread::Builder::new()
            .name("freya-cover-download".into())
            .spawn(move || {
                let message = artist_image::download(&url)
                    .and_then(|bytes| keep(pictures.as_ref(), "Web", &bytes))
                    .map(|found| FoundCover {
                        choose: true,
                        order: usize::MAX,
                        ..found
                    });
                let _ = results.send(match message {
                    Ok(found) => Message::Found(generation, found),
                    Err(e) => {
                        Message::Failed(generation, format!("Could not use that address: {e}"))
                    }
                });
            });
        if let Err(e) = started {
            log::warn!("could not download a cover: {e}");
        }
    }

    /// Keeps the picture in `bytes`, the user's own, as a cover to choose.
    pub fn add(&self, bytes: &[u8]) -> Result<FoundCover, String> {
        let pictures = self.pictures.as_ref().ok_or("no place for pictures")?;
        keep(pictures.as_ref(), "Your picture", bytes).map(|found| FoundCover {
            choose: true,
            order: usize::MAX,
            ..found
        })
    }

    /// Stops listening to the search going on.
    pub fn stop(&mut self) {
        self.generation += 1;
        self.searching = false;
    }

    pub fn searching(&self) -> bool {
        self.searching
    }

    /// Covers found since the last call, and what went wrong, if anything.
    pub fn poll(&mut self) -> (Vec<FoundCover>, Option<String>) {
        let mut found = Vec::new();
        let mut failed = None;
        while let Ok(message) = self.results.1.try_recv() {
            match message {
                Message::Found(generation, f) if generation == self.generation => found.push(f),
                Message::Done(generation) if generation == self.generation => {
                    self.searching = false;
                }
                Message::Failed(generation, e) if generation == self.generation => {
                    failed = Some(e);
                }
                _ => {}
            }
        }
        (found, failed)
    }
}

/// Looks for covers as `request` says, telling `send` of each: those kept first, then what the
/// internet has, downloaded a few at a time.
fn look(request: &CoverRequest, pictures: &dyn LibraryApi, send: &(dyn Fn(FoundCover) + Sync)) {
    let started = Instant::now();
    let mut order = 0;
    let mut next = || {
        order += 1;
        order
    };
    let seen = Mutex::new(HashSet::new());
    // Each picture once, the first found of it.
    let send_new = |found: FoundCover| {
        if seen
            .lock()
            .is_ok_and(|mut seen| seen.insert(found.checksum.clone()))
        {
            send(found);
        }
    };
    for (checksum, source) in &request.kept {
        let shown = pictures
            .picture(checksum)
            .ok()
            .flatten()
            .ok_or_else(|| "gone".to_owned())
            .and_then(|bytes| described(&bytes));
        if let Ok((width, height, preview, color)) = shown {
            send_new(FoundCover {
                checksum: checksum.clone(),
                source: source.clone(),
                width,
                height,
                preview,
                color,
                choose: false,
                order: next(),
            });
        }
    }
    if !request.online {
        return;
    }
    let group = match &request.group {
        Some(group) => Some(group.clone()),
        None => {
            let found = artist_image::musicbrainz_albums(&request.title, &request.artist)
                .map(|albums| same_album(albums, &request.title, &request.artist));
            std::thread::sleep(artist_image::MUSICBRAINZ_INTERVAL);
            found.unwrap_or_else(|e| {
                log::warn!("could not ask MusicBrainz for {}: {e}", request.title);
                None
            })
        }
    };
    let Some(group) = group else {
        log::info!("no album in MusicBrainz is {}", request.title);
        return;
    };
    let mut urls: Vec<(String, String)> = Vec::new();
    match artist_image::release_covers(&group, MAX_EACH) {
        Ok(covers) => urls.extend(covers.into_iter().map(|(url, label)| {
            let source = if label.is_empty() {
                "Cover Art Archive".to_owned()
            } else {
                format!("Cover Art Archive · {label}")
            };
            (url, source)
        })),
        Err(e) => log::warn!(
            "could not ask MusicBrainz for {}'s editions: {e}",
            request.title
        ),
    }
    if let Some(key) = &request.fanart_key {
        match artist_image::fanart_album_covers(&group, key) {
            Ok(found) => urls.extend(
                found
                    .into_iter()
                    .take(MAX_EACH)
                    .map(|url| (url, "fanart.tv".to_owned())),
            ),
            Err(e) => log::warn!("could not ask fanart.tv for {}: {e}", request.title),
        }
    }
    let queue: VecDeque<(usize, String, String)> = urls
        .into_iter()
        .map(|(url, source)| (next(), url, source))
        .collect();
    let total = queue.len();
    let queue = Mutex::new(queue);
    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(total) {
            scope.spawn(|| {
                // The lock goes with the closure, before the download.
                while let Some((order, url, source)) =
                    queue.lock().ok().and_then(|mut q| q.pop_front())
                {
                    match artist_image::download(&url)
                        .and_then(|bytes| keep(pictures, &source, &bytes))
                    {
                        Ok(found) => send_new(FoundCover { order, ..found }),
                        Err(e) => log::debug!("no cover from {url}: {e}"),
                    }
                }
            });
        }
    });
    log::info!(
        "looked through {total} covers of {} on the internet in {:.1} s",
        request.title,
        started.elapsed().as_secs_f64()
    );
}

/// The album found that is the one asked for: of that title, by that artist, as their words
/// read without case or punctuation.
fn same_album(
    candidates: Vec<freya_protocol::AlbumCandidate>,
    title: &str,
    artist: &str,
) -> Option<String> {
    let words = |text: &str| -> String {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let (title, artist) = (words(title), words(artist));
    candidates
        .into_iter()
        .find(|c| {
            words(&c.title) == title && (artist.is_empty() || words(&c.artist.name) == artist)
        })
        .map(|c| c.release_group)
}

/// Keeps `bytes` among the library's pictures, as a cover from `source` to choose.
fn keep(pictures: &dyn LibraryApi, source: &str, bytes: &[u8]) -> Result<FoundCover, String> {
    let (width, height, preview, color) = described(bytes)?;
    let checksum = pictures.put_picture(bytes).map_err(|e| e.to_string())?;
    Ok(FoundCover {
        checksum,
        source: source.to_owned(),
        width,
        height,
        preview,
        color,
        choose: false,
        order: 0,
    })
}

/// The picture in `bytes`: its size, a small copy of it to show, and a color from it for an
/// album's page.
fn described(bytes: &[u8]) -> Result<(u32, u32, SharedPixelBuffer<Rgba8Pixel>, [u8; 3]), String> {
    let picture = crate::gui::decode_picture(bytes).map_err(|e| e.to_string())?;
    let (width, height) = (picture.width(), picture.height());
    let small = picture.thumbnail(PREVIEW, PREVIEW);
    let color = freya_media::cover::page_color(small.thumbnail(64, 64).to_rgb8().as_raw());
    let rgba = small.to_rgba8();
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
    );
    Ok((width, height, buffer, color))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(group: &str, title: &str, artist: &str) -> freya_protocol::AlbumCandidate {
        freya_protocol::AlbumCandidate {
            release_group: group.to_owned(),
            title: title.to_owned(),
            kind: "Album".to_owned(),
            secondary: Vec::new(),
            date: String::new(),
            artist: freya_protocol::ArtistMatch {
                name: artist.to_owned(),
                ..Default::default()
            },
        }
    }

    #[test]
    fn the_album_searched_for_is_the_one_of_its_title_and_artist() {
        let found = vec![
            candidate("a", "Pale Folklore (Live)", "Agalloch"),
            candidate("b", "Pale Folklore", "Someone Else"),
            candidate("c", "Pale folklore", "AGALLOCH"),
        ];
        assert_eq!(
            same_album(found.clone(), "Pale Folklore", "Agalloch").as_deref(),
            Some("c")
        );
        assert_eq!(same_album(found, "The Mantle", "Agalloch"), None);
    }

    #[test]
    fn a_cover_is_kept_with_its_size_and_color() {
        let dir = std::env::temp_dir().join(format!("freya-covers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(800, 800, image::Rgb([180, 60, 20]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        let library = freya_library::Library::open_in_memory().unwrap();
        let pictures = freya_media::pictures::Pictures::new(dir.join("pictures"));
        let finder = CoverFinder::new(Some(Arc::new(freya_server::LocalLibrary::new(
            library,
            Some(pictures),
        ))));
        let found = finder.add(&bytes).unwrap();
        assert_eq!((found.width, found.height), (800, 800));
        assert!(found.choose);
        assert_eq!(found.source, "Your picture");
        assert!(freya_protocol::is_checksum(&found.checksum));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
