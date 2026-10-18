//! Artists and albums MusicBrainz knows, for the user to say which one theirs is: on threads
//! of their own, each with a small picture, from fanart.tv for an artist when the user has a
//! key, from the Cover Art Archive for an album, to tell those of one name apart.

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

use freya_library::artist_image::{self, AlbumCandidate, ArtistMatch};
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Pictures asked of fanart.tv at once.
const WORKERS: usize = 4;
/// The longest side of a picture beside a match, in pixels: sharp at 64 points on a Retina
/// screen.
const PICTURE: u32 = 160;

enum Message {
    Matches(u64, Vec<ArtistMatch>),
    Picture(u64, String, SharedPixelBuffer<Rgba8Pixel>),
    Done(u64),
    Failed(u64, String),
}

/// What a search found since the last poll.
#[derive(Default)]
pub struct Found {
    /// The artists found, once MusicBrainz answered.
    pub matches: Option<Vec<ArtistMatch>>,
    /// Pictures of them, by their MusicBrainz id.
    pub pictures: Vec<(String, SharedPixelBuffer<Rgba8Pixel>)>,
    pub failed: Option<String>,
}

pub struct MatchFinder {
    /// Counted up for each search, so a thread still running for an earlier one is ignored.
    generation: u64,
    searching: bool,
    results: (Sender<Message>, Receiver<Message>),
}

impl MatchFinder {
    pub fn new() -> Self {
        Self {
            generation: 0,
            searching: false,
            results: channel(),
        }
    }

    /// Searches MusicBrainz for `query`, forgetting an earlier search; with pictures from
    /// fanart.tv when there is a `fanart_key`.
    pub fn find(&mut self, query: String, fanart_key: Option<String>) {
        self.generation += 1;
        self.searching = true;
        let (generation, results) = (self.generation, self.results.0.clone());
        let started = std::thread::Builder::new()
            .name("freya-match-finder".into())
            .spawn(move || {
                let matches = match artist_image::musicbrainz_artists(&query) {
                    Ok(matches) => matches,
                    Err(e) => {
                        let _ = results.send(Message::Failed(
                            generation,
                            format!("MusicBrainz could not be asked: {e}"),
                        ));
                        let _ = results.send(Message::Done(generation));
                        return;
                    }
                };
                let _ = results.send(Message::Matches(generation, matches.clone()));
                if let Some(key) = fanart_key {
                    pictures(&matches, &key, &|mbid, picture| {
                        let _ = results.send(Message::Picture(generation, mbid, picture));
                    });
                }
                let _ = results.send(Message::Done(generation));
            });
        if let Err(e) = started {
            log::warn!("could not look for matches: {e}");
            self.searching = false;
        }
    }

    /// Stops listening to the search going on.
    pub fn stop(&mut self) {
        self.generation += 1;
        self.searching = false;
    }

    pub fn searching(&self) -> bool {
        self.searching
    }

    pub fn poll(&mut self) -> Found {
        let mut found = Found::default();
        while let Ok(message) = self.results.1.try_recv() {
            match message {
                Message::Matches(generation, matches) if generation == self.generation => {
                    found.matches = Some(matches);
                }
                Message::Picture(generation, mbid, picture) if generation == self.generation => {
                    found.pictures.push((mbid, picture));
                }
                Message::Failed(generation, e) if generation == self.generation => {
                    found.failed = Some(e);
                }
                Message::Done(generation) if generation == self.generation => {
                    self.searching = false;
                }
                _ => {}
            }
        }
        found
    }
}

/// fanart.tv's best liked picture of each of `matches` that it has one of, small.
fn pictures(
    matches: &[ArtistMatch],
    key: &str,
    send: &(dyn Fn(String, SharedPixelBuffer<Rgba8Pixel>) + Sync),
) {
    let queue = Mutex::new(matches.iter().map(|m| m.mbid.clone()).collect::<Vec<_>>());
    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(matches.len()) {
            scope.spawn(|| {
                // The lock goes with the closure, before the download.
                while let Some(mbid) = queue.lock().ok().and_then(|mut q| q.pop()) {
                    match picture(&mbid, key) {
                        Ok(Some(picture)) => send(mbid, picture),
                        Ok(None) => {}
                        Err(e) => log::debug!("no picture of {mbid} from fanart.tv: {e}"),
                    }
                }
            });
        }
    });
}

fn picture(mbid: &str, key: &str) -> Result<Option<SharedPixelBuffer<Rgba8Pixel>>, String> {
    let urls = artist_image::fanart_urls(mbid, key)?;
    let Some(url) = urls.portraits.first().or(urls.backdrops.first()) else {
        return Ok(None);
    };
    // fanart.tv keeps a small copy of each picture beside it.
    let small = url.replacen("/fanart/", "/preview/", 1);
    let bytes = artist_image::download(&small).or_else(|_| artist_image::download(url))?;
    let picture = image::load_from_memory(&bytes)
        .map_err(|e| e.to_string())?
        .resize_to_fill(PICTURE, PICTURE, image::imageops::FilterType::Triangle)
        .to_rgba8();
    Ok(Some(SharedPixelBuffer::clone_from_slice(
        picture.as_raw(),
        picture.width(),
        picture.height(),
    )))
}

/// Albums MusicBrainz knows by a title and an artist, for the user to say which one theirs
/// is, and so which artist made it: each with its cover from the Cover Art Archive.
pub struct AlbumFinder {
    generation: u64,
    searching: bool,
    results: (Sender<AlbumMessage>, Receiver<AlbumMessage>),
}

enum AlbumMessage {
    Albums(u64, Vec<AlbumCandidate>),
    Cover(u64, String, SharedPixelBuffer<Rgba8Pixel>),
    Done(u64),
    Failed(u64, String),
}

/// What an album search found since the last poll.
#[derive(Default)]
pub struct AlbumsFound {
    pub albums: Option<Vec<AlbumCandidate>>,
    /// Covers, by the album's release group.
    pub covers: Vec<(String, SharedPixelBuffer<Rgba8Pixel>)>,
    pub failed: Option<String>,
}

const COVER_ART: &str = "https://coverartarchive.org/release-group";

impl AlbumFinder {
    pub fn new() -> Self {
        Self {
            generation: 0,
            searching: false,
            results: channel(),
        }
    }

    /// Searches MusicBrainz for albums titled `title` by `artist`, forgetting an earlier
    /// search.
    pub fn find(&mut self, title: String, artist: String) {
        self.generation += 1;
        self.searching = true;
        let (generation, results) = (self.generation, self.results.0.clone());
        let started = std::thread::Builder::new()
            .name("freya-album-finder".into())
            .spawn(move || {
                let albums = match artist_image::musicbrainz_albums(&title, &artist) {
                    Ok(albums) => albums,
                    Err(e) => {
                        let _ = results.send(AlbumMessage::Failed(
                            generation,
                            format!("MusicBrainz could not be asked: {e}"),
                        ));
                        let _ = results.send(AlbumMessage::Done(generation));
                        return;
                    }
                };
                let _ = results.send(AlbumMessage::Albums(generation, albums.clone()));
                let queue = Mutex::new(
                    albums
                        .iter()
                        .map(|a| a.release_group.clone())
                        .collect::<Vec<_>>(),
                );
                std::thread::scope(|scope| {
                    for _ in 0..WORKERS.min(albums.len()) {
                        scope.spawn(|| {
                            // The lock goes with the closure, before the download.
                            while let Some(group) = queue.lock().ok().and_then(|mut q| q.pop()) {
                                if let Ok(cover) = cover(&group) {
                                    let _ =
                                        results.send(AlbumMessage::Cover(generation, group, cover));
                                }
                            }
                        });
                    }
                });
                let _ = results.send(AlbumMessage::Done(generation));
            });
        if let Err(e) = started {
            log::warn!("could not look for albums: {e}");
            self.searching = false;
        }
    }

    pub fn stop(&mut self) {
        self.generation += 1;
        self.searching = false;
    }

    pub fn searching(&self) -> bool {
        self.searching
    }

    pub fn poll(&mut self) -> AlbumsFound {
        let mut found = AlbumsFound::default();
        while let Ok(message) = self.results.1.try_recv() {
            match message {
                AlbumMessage::Albums(generation, albums) if generation == self.generation => {
                    found.albums = Some(albums);
                }
                AlbumMessage::Cover(generation, group, cover) if generation == self.generation => {
                    found.covers.push((group, cover));
                }
                AlbumMessage::Failed(generation, e) if generation == self.generation => {
                    found.failed = Some(e);
                }
                AlbumMessage::Done(generation) if generation == self.generation => {
                    self.searching = false;
                }
                _ => {}
            }
        }
        found
    }
}

/// The cover of the album `group` from the Cover Art Archive, small.
fn cover(group: &str) -> Result<SharedPixelBuffer<Rgba8Pixel>, String> {
    let bytes = artist_image::download(&format!("{COVER_ART}/{group}/front-250"))?;
    let cover = image::load_from_memory(&bytes)
        .map_err(|e| e.to_string())?
        .resize_to_fill(PICTURE, PICTURE, image::imageops::FilterType::Triangle)
        .to_rgba8();
    Ok(SharedPixelBuffer::clone_from_slice(
        cover.as_raw(),
        cover.width(),
        cover.height(),
    ))
}
