//! What the internet knows of the library's artists, asked for by a scan once the tracks and
//! covers are in: their MusicBrainz id when their tags give none, their pictures from fanart.tv
//! and Deezer, and what Last.fm and Wikipedia say of them. Only what an artist lacks is asked
//! for, and what is found stays theirs: neither a later scan nor a source that has another
//! picture now changes it, only the user. Showing the library never asks the internet.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::{ArtistText, Library, PictureKind, Result, ScanEvent, plain_name};
use crate::artist_image::{self, Online as Answer};
use crate::pictures::Pictures;
use crate::wikipedia::{self, Article};

/// Artists looked up at once: most of it is waiting for the services.
const WORKERS: usize = 4;
/// How often each service is asked at most: MusicBrainz once a second, Deezer 50 times in 5
/// seconds, Last.fm 5 times a second; each well below.
const DEEZER_INTERVAL: Duration = Duration::from_millis(150);
const LASTFM_INTERVAL: Duration = Duration::from_millis(250);
/// The progress is told at most this often.
const TELL_INTERVAL: Duration = Duration::from_millis(500);

/// Last.fm, asked for what it says of an artist: given by the app, which holds the key.
pub trait LastFm: Send + Sync {
    /// What Last.fm says of the artist `name`, the one with the MusicBrainz id `mbid` when
    /// known. An error means Last.fm could not be asked, or knows no such artist.
    fn artist_text(
        &self,
        name: &str,
        mbid: Option<&str>,
    ) -> std::result::Result<ArtistText, String>;
}

/// Which services a scan may ask, as the user allows.
#[derive(Clone, Default)]
pub struct Online {
    /// Deezer, for pictures of artists fanart.tv has none of.
    pub deezer: bool,
    /// The user's personal fanart.tv key; without one, fanart.tv is not asked.
    pub fanart_key: Option<String>,
    pub lastfm: Option<Arc<dyn LastFm>>,
    pub wikipedia: bool,
}

impl std::fmt::Debug for Online {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Online")
            .field("deezer", &self.deezer)
            .field("fanart", &self.fanart_key.is_some())
            .field("lastfm", &self.lastfm.is_some())
            .field("wikipedia", &self.wikipedia)
            .finish()
    }
}

impl Online {
    /// Whether any service may be asked.
    pub fn any(&self) -> bool {
        self.deezer || self.fanart_key.is_some() || self.lastfm.is_some() || self.wikipedia
    }

    /// Whether anything needs an artist's MusicBrainz id.
    fn needs_ids(&self) -> bool {
        self.fanart_key.is_some() || self.wikipedia
    }
}

/// What to ask of the internet about one artist.
#[derive(Debug, Default)]
struct Job {
    name: String,
    /// Their MusicBrainz id, when known; else whether to search for it.
    mbid: Option<String>,
    search: bool,
    portrait: bool,
    backdrop: bool,
    text: bool,
    article: bool,
}

impl Job {
    fn anything(&self) -> bool {
        self.search || self.portrait || self.backdrop || self.text || self.article
    }
}

/// What the internet said of one artist; each `None` where it was not asked, or could not be.
#[derive(Debug, Default)]
struct Found {
    /// The id MusicBrainz's search found, or that it found none.
    searched: Option<Option<String>>,
    /// The source asked for each picture, and the picture kept, if it had one.
    portrait: Option<(&'static str, Option<String>)>,
    backdrop: Option<(&'static str, Option<String>)>,
    text: Option<ArtistText>,
    article: Option<Option<Article>>,
    /// The id the pictures from fanart.tv and the article were asked by.
    mbid: Option<String>,
}

/// One service's turn: no sooner than `interval` after the last.
struct Pace {
    interval: Duration,
    last: Mutex<Option<Instant>>,
}

impl Pace {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: Mutex::new(None),
        }
    }

    /// Waits for the turn, holding it so the others wait after.
    fn wait(&self) {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(wait) = last.and_then(|at| self.interval.checked_sub(at.elapsed())) {
            std::thread::sleep(wait);
        }
        *last = Some(Instant::now());
    }
}

struct Paces {
    musicbrainz: Pace,
    deezer: Pace,
    lastfm: Pace,
}

impl Library {
    /// The artist's MusicBrainz id as Freya knows it: the user's match, their tags', else
    /// what MusicBrainz's search found. And whether to search for it: never searched, or it
    /// found none long ago.
    pub fn known_artist_id(&self, artist: &str) -> Result<(Option<String>, bool)> {
        if let Some(mbid) = self.artist_edit(artist)?.mbid {
            return Ok((Some(mbid), false));
        }
        if let Some(mbid) = self.artist_mbid(Some(artist))? {
            return Ok((Some(mbid), false));
        }
        Ok(match self.searched_artist_id(artist)? {
            Some(searched) => {
                let again = searched.answer.is_none() && !searched.recent();
                (searched.answer, again)
            }
            None => (None, true),
        })
    }

    /// What to ask of the internet about `artist`: what they lack.
    fn job(&self, artist: &str, online: &Online) -> Result<Job> {
        let (mbid, search) = self.known_artist_id(artist)?;
        let lacks = |kind| -> Result<bool> {
            Ok(match self.artist_picture(artist, kind)? {
                None => true,
                // Theirs, or the user chose none.
                Some(row) if row.picture.is_some() || row.chosen() => false,
                // None was found lately.
                Some(row) => !row.recent(),
            })
        };
        let pictures = online.deezer || online.fanart_key.is_some();
        let article = online.wikipedia
            && match self.artist_article(artist)? {
                None => true,
                Some((id, _)) if Some(&id) != mbid.as_ref() => true,
                Some((_, kept)) => kept.answer.is_none() && !kept.recent(),
            };
        Ok(Job {
            name: artist.to_owned(),
            search: search && online.needs_ids(),
            portrait: pictures && lacks(PictureKind::Portrait)?,
            backdrop: online.fanart_key.is_some() && lacks(PictureKind::Backdrop)?,
            text: online.lastfm.is_some() && self.artist_text(artist)?.is_none(),
            article,
            mbid,
        })
    }

    /// Keeps what the internet said of the artist, unless they were matched to another
    /// artist of the name meanwhile: then what was asked by their old id is of no use.
    fn keep_found(&self, artist: &str, found: Found) -> Result<()> {
        let now = SystemTime::now();
        if let Some(mbid) = &found.searched {
            self.set_searched_artist_id(artist, mbid.as_deref(), now)?;
        }
        let current = self.known_artist_id(artist)?.0;
        let theirs = found.mbid.is_none() || found.mbid == current;
        for (kind, answer) in [
            (PictureKind::Portrait, found.portrait),
            (PictureKind::Backdrop, found.backdrop),
        ] {
            if let Some((source, picture)) = answer
                && (theirs || source == "deezer")
            {
                self.set_artist_picture(artist, kind, source, picture.as_deref(), now)?;
            }
        }
        if let (Some(text), true) = (&found.text, theirs) {
            self.set_artist_text(artist, text, now)?;
        }
        if let (Some(article), Some(mbid), true) = (&found.article, &found.mbid, theirs) {
            self.set_artist_article(artist, mbid, article.as_ref(), now)?;
        }
        Ok(())
    }
}

/// Asks the internet for what each of `artists` lacks, as `online` allows, and keeps it;
/// every artist in the library when `artists` is none. Stops early when `cancel` is set.
pub(super) fn fetch_artists(
    library: &Library,
    artists: Option<&[String]>,
    online: &Online,
    pictures: &Pictures,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(ScanEvent),
) -> Result<()> {
    if !online.any() {
        return Ok(());
    }
    let names: Vec<String> = match artists {
        Some(artists) => artists.to_vec(),
        None => library
            .artists()?
            .into_iter()
            .filter_map(|artist| artist.name)
            .collect(),
    };
    let mut jobs = Vec::new();
    for name in &names {
        let job = library.job(name, online)?;
        if job.anything() {
            jobs.push(job);
        }
    }
    let total = jobs.len();
    if total == 0 {
        return Ok(());
    }
    progress(ScanEvent::Artists { done: 0, total });
    let paces = Paces {
        musicbrainz: Pace::new(artist_image::MUSICBRAINZ_INTERVAL),
        deezer: Pace::new(DEEZER_INTERVAL),
        lastfm: Pace::new(LASTFM_INTERVAL),
    };
    let next = AtomicUsize::new(0);
    let (results, found) = channel::<(usize, Found)>();
    std::thread::scope(|threads| -> Result<()> {
        for _ in 0..WORKERS.min(total) {
            let results = results.clone();
            let (jobs, next, paces) = (&jobs, &next, &paces);
            threads.spawn(move || {
                while !cancel.load(Ordering::Relaxed) {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(i) else {
                        break;
                    };
                    if results
                        .send((i, ask(job, online, pictures, paces)))
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
        drop(results);
        let mut told = Instant::now();
        for (done, (i, found)) in (1..).zip(found) {
            let name = &jobs[i].name;
            library.keep_found(name, found)?;
            if told.elapsed() >= TELL_INTERVAL || done == total {
                told = Instant::now();
                progress(ScanEvent::Artists { done, total });
            }
        }
        Ok(())
    })
}

/// Asks the internet what `job` says, each service in its turn.
fn ask(job: &Job, online: &Online, pictures: &Pictures, paces: &Paces) -> Found {
    let name = plain_name(&job.name);
    let mut found = Found::default();
    let mut mbid = job.mbid.clone();
    if job.search {
        paces.musicbrainz.wait();
        match artist_image::musicbrainz_id(name) {
            Ok(id) => {
                mbid.clone_from(&id);
                found.searched = Some(id);
            }
            Err(e) => log::debug!("could not search MusicBrainz for {name}: {e}"),
        }
    }
    found.mbid.clone_from(&mbid);

    let fanart = match (&online.fanart_key, &mbid) {
        (Some(key), Some(mbid)) if job.portrait || job.backdrop => {
            match artist_image::fanart_urls(mbid, key) {
                Ok(urls) => Some(urls),
                Err(e) => {
                    log::debug!("could not ask fanart.tv about {name}: {e}");
                    None
                }
            }
        }
        _ => None,
    };
    if job.portrait {
        let from_fanart = fanart
            .as_ref()
            .and_then(|urls| urls.portraits.first())
            .and_then(|url| keep(url, pictures, name));
        found.portrait = match from_fanart {
            Some(picture) => Some(("fanart.tv", Some(picture))),
            None if online.deezer => {
                paces.deezer.wait();
                match artist_image::deezer(name) {
                    Ok(Answer::Picture(bytes)) => match pictures.put(&bytes) {
                        Ok(picture) => Some(("deezer", Some(picture))),
                        Err(e) => {
                            log::warn!("could not keep a picture of {name}: {e}");
                            None
                        }
                    },
                    Ok(Answer::NoPicture) => Some(("deezer", None)),
                    Err(e) => {
                        log::debug!("could not ask Deezer about {name}: {e}");
                        None
                    }
                }
            }
            // fanart.tv answered, with none.
            None if fanart.is_some() => Some(("fanart.tv", None)),
            None => None,
        };
    }
    if job.backdrop
        && let Some(urls) = &fanart
    {
        let picture = urls
            .backdrops
            .first()
            .and_then(|url| keep(url, pictures, name));
        // Asked again next scan when the download failed.
        if picture.is_some() || urls.backdrops.is_empty() {
            found.backdrop = Some(("fanart.tv", picture));
        }
    }
    if job.text
        && let Some(lastfm) = &online.lastfm
    {
        paces.lastfm.wait();
        match lastfm.artist_text(name, mbid.as_deref()) {
            Ok(text) => found.text = Some(text),
            Err(e) => log::debug!("no Last.fm information on {name}: {e}"),
        }
    }
    if job.article
        && let Some(mbid) = &mbid
    {
        paces.musicbrainz.wait();
        match wikipedia::article_of(mbid) {
            Ok(article) => found.article = Some(article),
            Err(e) => log::debug!("no Wikipedia article on {name}: {e}"),
        }
    }
    found
}

/// Downloads the picture at `url` into Freya's pictures: its checksum.
fn keep(url: &str, pictures: &Pictures, name: &str) -> Option<String> {
    let kept = artist_image::download(url)
        .and_then(|bytes| pictures.put(&bytes).map_err(|e| e.to_string()));
    match kept {
        Ok(checksum) => Some(checksum),
        Err(e) => {
            log::debug!("could not download a picture of {name}: {e}");
            None
        }
    }
}
