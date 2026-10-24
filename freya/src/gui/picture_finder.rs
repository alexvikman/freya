//! Pictures to choose from when the user changes an artist: those Freya keeps of them, the
//! one from their folder among them, and every one Deezer and fanart.tv have of them, found
//! on a thread of its own. Each found is kept among the downloaded pictures at once, so choosing it needs no
//! second download; those not chosen are taken away later, as no artist has them.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use freya_media::artist_image;
use freya_protocol::LibraryApi;
use freya_protocol::PictureKind;
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// At most this many of each kind from each source: fanart.tv has dozens of some artists.
const MAX_EACH: usize = 12;
/// The longest side of a preview, in pixels: sharp in a 160-point tile on a Retina screen.
const PREVIEW: u32 = 360;
/// Pictures downloaded at once.
const WORKERS: usize = 4;

/// The pictures downloaded while Freya runs, by their address, so looking again reads them
/// from the disk.
type Known = Arc<Mutex<HashMap<String, String>>>;

/// What choosing a picture means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// One kept among the downloaded pictures, by its SHA-256.
    Kept(String),
    /// None: their album covers, or no wide picture.
    Nothing,
}

/// A picture found to choose from.
pub struct Found {
    pub kind: PictureKind,
    pub pick: Pick,
    /// Where it is from: "Folder", "Deezer", "fanart.tv", "Chosen before".
    pub source: String,
    pub width: u32,
    pub height: u32,
    pub preview: SharedPixelBuffer<Rgba8Pixel>,
    /// Chosen as soon as it is found: one the user gave by its address.
    pub choose: bool,
    /// Where it goes among those found: they come in any order.
    pub order: usize,
}

/// What to look for.
pub struct Request {
    pub name: String,
    /// Whether Deezer may be asked.
    pub deezer: bool,
    pub mbid: Option<String>,
    /// Whether MusicBrainz may be searched for the id, when there is none.
    pub search: bool,
    pub fanart_key: Option<String>,
    /// Pictures of theirs already kept, with where they came from.
    pub kept: Vec<(PictureKind, String, String)>,
    /// Their albums' covers among Freya's pictures, for a collage of them.
    pub covers: Vec<String>,
}

enum Message {
    Found(u64, Found),
    Done(u64),
    Failed(u64, String),
}

pub struct PictureFinder {
    pictures: Option<Arc<dyn LibraryApi>>,
    known: Known,
    /// Counted up for each search, so a thread still running for an earlier one is ignored.
    generation: u64,
    searching: bool,
    results: (Sender<Message>, Receiver<Message>),
}

impl PictureFinder {
    pub fn new(pictures: Option<Arc<dyn LibraryApi>>) -> Self {
        Self {
            pictures,
            known: Known::default(),
            generation: 0,
            searching: false,
            results: channel(),
        }
    }

    /// Starts looking for pictures, forgetting an earlier search.
    pub fn find(&mut self, request: Request) {
        self.generation += 1;
        let Some(pictures) = self.pictures.clone() else {
            return;
        };
        self.searching = true;
        let (generation, results) = (self.generation, self.results.0.clone());
        let known = Arc::clone(&self.known);
        let started = std::thread::Builder::new()
            .name("freya-picture-finder".into())
            .spawn(move || {
                look(&request, pictures.as_ref(), &known, &|found| {
                    let _ = results.send(Message::Found(generation, found));
                });
                let _ = results.send(Message::Done(generation));
            });
        if let Err(e) = started {
            log::warn!("could not look for pictures: {e}");
            self.searching = false;
        }
    }

    /// Downloads the picture at `url` as one to choose, chosen when it comes.
    pub fn download(&mut self, kind: PictureKind, url: String) {
        let Some(pictures) = self.pictures.clone() else {
            return;
        };
        let (generation, results) = (self.generation, self.results.0.clone());
        let started = std::thread::Builder::new()
            .name("freya-picture-download".into())
            .spawn(move || {
                let message = artist_image::download(&url)
                    .and_then(|bytes| keep(pictures.as_ref(), kind, "Web", &bytes))
                    .map(|found| Found {
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
            log::warn!("could not download a picture: {e}");
        }
    }

    /// Keeps the picture in `bytes`, the user's own, as one to choose.
    pub fn add(&self, kind: PictureKind, bytes: &[u8]) -> Result<Found, String> {
        let pictures = self.pictures.as_ref().ok_or("no place for pictures")?;
        keep(pictures.as_ref(), kind, "Your picture", bytes).map(|found| Found {
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

    /// Pictures found since the last call, and what went wrong, if anything.
    pub fn poll(&mut self) -> (Vec<Found>, Option<String>) {
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

/// Looks for pictures as `request` says, telling `send` of each: what Freya has first, then
/// what the internet has, downloaded a few at a time, square ones first.
fn look(
    request: &Request,
    pictures: &dyn LibraryApi,
    known: &Known,
    send: &(dyn Fn(Found) + Sync),
) {
    let started = Instant::now();
    let mut order = 0;
    let mut next = || {
        order += 1;
        order
    };
    // What none means for a square picture: their album covers, when they have any.
    let covers = request
        .covers
        .iter()
        .filter_map(|checksum| crate::gui::decode_picture(&pictures.picture(checksum).ok()??).ok())
        .take(4)
        .collect();
    if let Some(collage) = artist_image::collage_of(covers, PREVIEW) {
        let rgba = image::DynamicImage::ImageRgb8(collage).to_rgba8();
        send(Found {
            kind: PictureKind::Portrait,
            pick: Pick::Nothing,
            source: "Album covers".to_owned(),
            width: 0,
            height: 0,
            preview: SharedPixelBuffer::clone_from_slice(
                rgba.as_raw(),
                rgba.width(),
                rgba.height(),
            ),
            choose: false,
            order: 0,
        });
    }
    for (kind, checksum, source) in &request.kept {
        let shown = pictures
            .picture(checksum)
            .ok()
            .flatten()
            .ok_or_else(|| "gone".to_owned())
            .and_then(|bytes| preview(&bytes));
        if let Ok((width, height, preview)) = shown {
            send(Found {
                kind: *kind,
                pick: Pick::Kept(checksum.clone()),
                source: source.clone(),
                width,
                height,
                preview,
                choose: false,
                order: next(),
            });
        }
    }
    let mut squares: Vec<(PictureKind, &str, String)> = Vec::new();
    let mut wides = Vec::new();
    if request.deezer {
        match artist_image::deezer_picture_urls(&request.name) {
            Ok(urls) => squares.extend(
                urls.into_iter()
                    .take(MAX_EACH)
                    .map(|url| (PictureKind::Portrait, "Deezer", url)),
            ),
            Err(e) => log::warn!("could not ask Deezer for pictures of {}: {e}", request.name),
        }
    }
    let mbid = match (&request.fanart_key, &request.mbid) {
        (None, _) => None,
        (Some(_), Some(mbid)) => Some(mbid.clone()),
        (Some(_), None) if request.search => artist_image::musicbrainz_id(&request.name)
            .unwrap_or_else(|e| {
                log::warn!("could not ask MusicBrainz for {}: {e}", request.name);
                None
            }),
        (Some(_), None) => None,
    };
    if let (Some(key), Some(mbid)) = (&request.fanart_key, mbid) {
        match artist_image::fanart_urls(&mbid, key) {
            Ok(urls) => {
                squares.extend(
                    urls.portraits
                        .into_iter()
                        .take(MAX_EACH)
                        .map(|url| (PictureKind::Portrait, "fanart.tv", url)),
                );
                wides.extend(
                    urls.backdrops
                        .into_iter()
                        .take(MAX_EACH)
                        .map(|url| (PictureKind::Backdrop, "fanart.tv", url)),
                );
            }
            Err(e) => log::warn!(
                "could not ask fanart.tv for pictures of {}: {e}",
                request.name
            ),
        }
    }
    let queue: VecDeque<(usize, PictureKind, &str, String)> = squares
        .into_iter()
        .chain(wides)
        .map(|(kind, source, url)| (next(), kind, source, url))
        .collect();
    let total = queue.len();
    let queue = Mutex::new(queue);
    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(total) {
            scope.spawn(|| {
                // The lock goes with the closure, before the download.
                while let Some((order, kind, source, url)) =
                    queue.lock().ok().and_then(|mut q| q.pop_front())
                {
                    match fetch(&url, kind, source, pictures, known) {
                        Ok(found) => send(Found { order, ..found }),
                        Err(e) => log::debug!("no picture from {url}: {e}"),
                    }
                }
            });
        }
    });
    log::info!(
        "looked through {total} pictures of {} on the internet in {:.1} s",
        request.name,
        started.elapsed().as_secs_f64()
    );
}

/// The picture at `url`: downloaded, unless it was in this run already.
fn fetch(
    url: &str,
    kind: PictureKind,
    source: &str,
    pictures: &dyn LibraryApi,
    known: &Known,
) -> Result<Found, String> {
    let kept = known
        .lock()
        .ok()
        .and_then(|known| known.get(url).cloned())
        .and_then(|checksum| pictures.picture(&checksum).ok().flatten());
    let bytes = match kept {
        Some(bytes) => bytes,
        None => artist_image::download(url)?,
    };
    let found = keep(pictures, kind, source, &bytes)?;
    if let (Pick::Kept(checksum), Ok(mut known)) = (&found.pick, known.lock()) {
        known.insert(url.to_owned(), checksum.clone());
    }
    Ok(found)
}

/// Keeps `bytes` among the downloaded pictures, as one of `kind` to choose.
fn keep(
    pictures: &dyn LibraryApi,
    kind: PictureKind,
    source: &str,
    bytes: &[u8],
) -> Result<Found, String> {
    let (width, height, preview) = preview(bytes)?;
    let checksum = pictures.put_picture(bytes).map_err(|e| e.to_string())?;
    Ok(Found {
        kind,
        pick: Pick::Kept(checksum),
        source: source.to_owned(),
        width,
        height,
        preview,
        choose: false,
        order: 0,
    })
}

/// The picture in `bytes`, its size and a small copy of it to show.
fn preview(bytes: &[u8]) -> Result<(u32, u32, SharedPixelBuffer<Rgba8Pixel>), String> {
    let picture = crate::gui::decode_picture(bytes).map_err(|e| e.to_string())?;
    let (width, height) = (picture.width(), picture.height());
    let small = picture.thumbnail(PREVIEW, PREVIEW).to_rgba8();
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        small.as_raw(),
        small.width(),
        small.height(),
    );
    Ok((width, height, buffer))
}

#[cfg(test)]
mod tests {
    use freya_media::pictures::Pictures;

    use super::*;

    #[test]
    fn finds_the_kept_pictures_and_keeps_the_users_own() {
        let dir = std::env::temp_dir().join(format!("freya-finder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jpeg = |rgb: [u8; 3], width: u32, height: u32| {
            let mut bytes = Vec::new();
            image::RgbImage::from_pixel(width, height, image::Rgb(rgb))
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Jpeg,
                )
                .unwrap();
            bytes
        };
        let pictures = Pictures::new(dir.join("pictures"));
        let folder = pictures.put(&jpeg([0, 0, 200], 1920, 1080)).unwrap();
        let kept = pictures.put(&jpeg([200, 0, 0], 1000, 1000)).unwrap();
        let library = freya_library::Library::open_in_memory().unwrap();
        let mut finder = PictureFinder::new(Some(Arc::new(freya_server::LocalLibrary::new(
            library,
            Some(pictures),
        ))));
        finder.find(Request {
            name: "Ghost".into(),
            deezer: false,
            mbid: None,
            search: false,
            fanart_key: None,
            kept: vec![
                (PictureKind::Backdrop, folder.clone(), "Folder".into()),
                (PictureKind::Portrait, kept.clone(), "Deezer".into()),
            ],
            covers: Vec::new(),
        });
        assert!(finder.searching());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut found = Vec::new();
        while finder.searching() {
            assert!(std::time::Instant::now() < deadline);
            found.extend(finder.poll().0);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(found.len(), 2);
        let wide = &found[0];
        assert_eq!(
            (wide.kind, &wide.pick),
            (PictureKind::Backdrop, &Pick::Kept(folder))
        );
        assert_eq!((wide.width, wide.height), (1920, 1080));
        assert_eq!(wide.preview.width(), PREVIEW);
        assert_eq!(found[1].pick, Pick::Kept(kept));

        let own = finder
            .add(PictureKind::Portrait, &jpeg([0, 200, 0], 600, 600))
            .unwrap();
        assert!(own.choose);
        assert!(matches!(own.pick, Pick::Kept(_)));
        assert!(finder.add(PictureKind::Portrait, b"not a picture").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
