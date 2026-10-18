//! Artists MusicBrainz knows by a name, for the user to say which one theirs is: on a thread
//! of its own, each with a small picture from fanart.tv when the user has a key, to tell
//! artists of one name apart.

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

use freya_library::artist_image::{self, ArtistMatch};
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
