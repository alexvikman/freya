//! Small album covers for grids, and artists' pictures in the sizes shown. Each is made once,
//! on threads of their own, and kept as a file, so showing thousands of albums only ever decodes
//! the covers on screen. The window never waits for the music's folders, which may be on a
//! network drive: what was made before shows at once, while a thread looks whether it is still
//! right; and every artist's and album's are made ahead, a few at a time, so they are there
//! when scrolled to. All of them can be made again without the internet: from the music's
//! folders, or from the pictures downloaded for artists, which are kept apart (see
//! [`crate::pictures`]). That an album has no cover is only remembered until Freya quits, so a
//! cover added later is found.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use crate::artist_image;
use crate::cover::read_cover;
use crate::library::PictureKind;
use crate::pictures::Pictures;

/// The longest side of a thumbnail, in pixels: sharp in a 150-point grid tile on a Retina
/// screen, about 30 KB as a JPEG.
pub const SIZE: u32 = 300;
const JPEG_QUALITY: u8 = 85;
/// The longest side of an artist's picture on their page.
const LARGE: u32 = 1400;
/// The longest side of a wide picture across the top of an artist's page: sharp across a 4K
/// screen.
const BACKDROP: u32 = 3840;
const BACKDROP_QUALITY: u8 = 90;
/// How the cache is laid out, noted in it. Before, it held downloaded pictures and markers
/// of what the internet said too, under names that did not tell them apart.
const LAYOUT: &str = "2";

/// What the library knows of a picture of an artist from the internet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Downloaded {
    /// Never asked for, or the source had none long ago: ask, when allowed.
    Unknown,
    /// The source had none, lately.
    Missing,
    /// The picture kept among the downloaded ones, by its SHA-256.
    Kept(String),
}

/// Where an artist's picture can come from: see [`crate::artist_image`].
#[derive(Clone, Debug)]
pub struct ArtistSource {
    pub name: Option<String>,
    /// A track of each of the artist's albums.
    pub album_tracks: Vec<PathBuf>,
    /// Whether Deezer may be asked, and its picture shown.
    pub online: bool,
    /// Their MusicBrainz id and the user's fanart.tv key, when both are known: fanart.tv is
    /// asked first then, as it knows which artist of a name they are.
    pub fanart: Option<(String, String)>,
    /// What Deezer gave before.
    pub deezer: Downloaded,
    /// The picture the user chose, or that they chose none (their album covers); before
    /// all else.
    pub chosen: Option<Downloaded>,
}

/// Where a wide picture of an artist can come from: see [`crate::artist_image`].
#[derive(Clone, Debug)]
pub struct BackdropSource {
    pub name: Option<String>,
    /// A track of each of the artist's albums.
    pub album_tracks: Vec<PathBuf>,
    /// Their MusicBrainz id, from their tags or MusicBrainz's search.
    pub mbid: Option<String>,
    /// Whether MusicBrainz may be searched for their id: not when it found none lately.
    pub search: bool,
    /// The user's personal fanart.tv key; without one, fanart.tv is not asked.
    pub fanart_key: Option<String>,
    /// What fanart.tv gave before.
    pub fanart: Downloaded,
    /// The wide picture the user chose, or that they chose none; before all else.
    pub chosen: Option<Downloaded>,
}

/// What a thread learned from the internet, for the library to keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fetched {
    /// `source` was asked for the artist's picture just now, and gave this one, or none.
    Picture {
        artist: String,
        kind: PictureKind,
        source: &'static str,
        picture: Option<String>,
        /// The MusicBrainz id it was asked for by, for fanart.tv: the answer is of no use once
        /// the artist is matched to another.
        mbid: Option<String>,
    },
    /// MusicBrainz's search was asked for the artist's id just now.
    ArtistId {
        artist: String,
        mbid: Option<String>,
    },
}

/// An album's cover to make, from one of its tracks.
struct AlbumJob(PathBuf);

/// Where a wide picture of an artist is made from.
enum BackdropJob {
    Local(PathBuf),
    /// One kept among the downloaded pictures: the user's choice.
    Kept(String),
    /// fanart.tv's: the one kept when there is one, else asked for.
    Fanart {
        name: String,
        mbid: Option<String>,
        search: bool,
        key: String,
        kept: Option<String>,
    },
}

/// What a thread did.
enum Done {
    /// The thumbnail or picture for `key` to show now, and an artist's large picture; none
    /// when there is none. `remade` when the file shown was made again, as it changed.
    Shown {
        key: String,
        shown: Option<PathBuf>,
        large: Option<PathBuf>,
        remade: bool,
        fetched: Vec<Fetched>,
    },
    /// A wide picture, made or not.
    Backdrop {
        key: String,
        made: bool,
        fetched: Vec<Fetched>,
    },
}

/// What is known about one album's thumbnail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Thumbnail {
    /// Made; the file to show.
    Ready(PathBuf),
    /// The album has no cover, as far as this run of Freya knows.
    None,
    /// Being made; [`Thumbnails::poll`] says when.
    Pending,
}

/// A thread taking jobs, those the window waits for before those made ahead.
struct Worker<J> {
    urgent: Sender<(String, J)>,
    later: Sender<(String, J)>,
}

impl<J: Send + 'static> Worker<J> {
    /// Starts a thread doing each job with `run`, telling `done`.
    fn start(
        name: &str,
        done: Sender<Done>,
        mut run: impl FnMut(String, J) -> Done + Send + 'static,
    ) -> Option<Self> {
        let (urgent, urgent_jobs) = channel::<(String, J)>();
        let (later, later_jobs) = channel::<(String, J)>();
        let started = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                loop {
                    let job = match urgent_jobs.try_recv() {
                        Ok(job) => job,
                        Err(TryRecvError::Disconnected) => break,
                        Err(TryRecvError::Empty) => match later_jobs.try_recv() {
                            Ok(job) => job,
                            Err(_) => match urgent_jobs.recv_timeout(Duration::from_millis(100)) {
                                Ok(job) => job,
                                Err(RecvTimeoutError::Timeout) => continue,
                                Err(RecvTimeoutError::Disconnected) => break,
                            },
                        },
                    };
                    if done.send(run(job.0, job.1)).is_err() {
                        break;
                    }
                }
            });
        match started {
            Ok(_) => Some(Self { urgent, later }),
            Err(e) => {
                log::warn!("could not start {name}: {e}");
                None
            }
        }
    }
}

/// The pictures made for each artist, kept between runs, so they show at once.
const INDEX: &str = "artists.index";

pub struct Thumbnails {
    dir: PathBuf,
    pictures: Pictures,
    /// Albums and artists asked for, with what is known about them.
    known: HashMap<String, Thumbnail>,
    /// The large picture of each artist asked for, by their key, once made.
    portraits: HashMap<String, PathBuf>,
    /// The file shown for each artist, and their large picture, as made last: by their key.
    index: HashMap<String, (PathBuf, Option<PathBuf>)>,
    index_changed: bool,
    index_saved: Instant,
    /// Asked of a thread in this run, by their key: each once; and those of artists, kept in
    /// the index.
    queued: HashSet<String>,
    artist_keys: HashSet<String>,
    /// Jobs a thread has not done yet.
    waiting: usize,
    /// Wide pictures asked of the internet in this run, by their key: each once, even when it
    /// could not be reached.
    asked: HashSet<String>,
    covers: Option<Worker<AlbumJob>>,
    artists: Option<Worker<ArtistSource>>,
    /// Wide pictures, on a thread of their own: MusicBrainz is asked once a second at most,
    /// which would hold up the covers.
    backdrop_jobs: Option<Sender<(String, BackdropJob)>>,
    finished_tx: Sender<Done>,
    finished: Receiver<Done>,
    /// What the threads learned, until the library takes it.
    fetched: Vec<Fetched>,
    /// Keys whose file was made again since the library last asked, so it reads them anew.
    remade: Vec<String>,
}

impl Thumbnails {
    /// Thumbnails kept in `dir`, which is created when the first one is made, from covers and
    /// from the downloaded `pictures`.
    pub fn new(dir: PathBuf, pictures: Pictures) -> Self {
        let (finished_tx, finished) = channel();
        let index = read_index(&dir);
        Self {
            dir,
            pictures,
            known: HashMap::new(),
            portraits: HashMap::new(),
            index,
            index_changed: false,
            index_saved: Instant::now(),
            queued: HashSet::new(),
            artist_keys: HashSet::new(),
            waiting: 0,
            asked: HashSet::new(),
            covers: None,
            artists: None,
            backdrop_jobs: None,
            finished_tx,
            finished,
            fetched: Vec::new(),
            remade: Vec::new(),
        }
    }

    /// Clears a cache of an older layout of what it holds: thumbnails, pictures made for
    /// artists, and markers. Returns how many files were taken away.
    pub fn clear_older_layout(&self) -> std::io::Result<usize> {
        let marker = self.dir.join("layout");
        if std::fs::read_to_string(&marker).is_ok_and(|layout| layout.trim() == LAYOUT) {
            return Ok(0);
        }
        let mut removed = 0;
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                create_private_dir(&self.dir)?;
                std::fs::write(&marker, LAYOUT)?;
                return Ok(0);
            }
            Err(e) => return Err(e),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ours = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| ["jpg", "collage", "nobackdrop", "partial"].contains(&e));
            if ours && entry.file_type().is_ok_and(|t| t.is_file()) {
                std::fs::remove_file(&path)?;
                removed += 1;
            }
        }
        std::fs::write(&marker, LAYOUT)?;
        Ok(removed)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn pictures(&self) -> &Pictures {
        &self.pictures
    }

    /// A name for an artist's picture that stays the same between runs, apart from any
    /// album's.
    pub fn artist_key(artist: Option<&str>) -> String {
        Self::key(Some("\u{1}artist"), artist)
    }

    /// The picture for the artist `key`: the one made before at once, while a thread looks
    /// whether it is still right; else made from `source`, as soon as a thread can.
    pub fn get_artist(&mut self, key: &str, source: &ArtistSource) -> Thumbnail {
        if let Some(known) = self.known.get(key) {
            return known.clone();
        }
        let state = match self.index.get(key).cloned() {
            Some((shown, large)) => {
                if let Some(large) = large {
                    self.portraits.insert(key.to_owned(), large);
                }
                self.send_artist(key, source.clone(), false);
                Thumbnail::Ready(shown)
            }
            None => {
                self.send_artist(key, source.clone(), true);
                Thumbnail::Pending
            }
        };
        self.known.insert(key.to_owned(), state.clone());
        state
    }

    /// Makes the picture for the artist `key` ahead of their being shown, when a thread is
    /// free.
    pub fn prepare_artist(&mut self, key: &str, source: &ArtistSource) {
        // Those made before are looked over when shown, not all at once.
        if !self.known.contains_key(key)
            && !self.queued.contains(key)
            && !self.index.contains_key(key)
        {
            self.send_artist(key, source.clone(), false);
        }
    }

    /// The artist `key`'s picture at up to 1400 pixels, for their page, once made; none when
    /// it is a collage of their covers.
    pub fn artist_portrait(&self, key: &str) -> Option<PathBuf> {
        self.portraits.get(key).cloned()
    }

    /// The wide picture across the top of the page of the artist `key`. When it is not made
    /// yet, it is made from `source`: one in their folder, else fanart.tv's, when the user
    /// gave a key.
    pub fn get_backdrop(&mut self, key: &str, source: &BackdropSource) -> Thumbnail {
        let known_key = format!("{key}-backdrop");
        if let Some(known) = self.known.get(&known_key) {
            return known.clone();
        }
        let chosen = source
            .chosen
            .clone()
            .filter(|chosen| *chosen != Downloaded::Unknown);
        let local = artist_image::local_backdrop(&source.album_tracks).filter(|_| chosen.is_none());
        let fanart = match (&source.name, &source.fanart_key, &source.fanart) {
            _ if chosen.is_some() => None,
            (Some(name), Some(fanart_key), Downloaded::Kept(checksum)) => {
                Some((name, fanart_key, Some(checksum.clone())))
            }
            (Some(name), Some(fanart_key), Downloaded::Unknown)
                if !self.asked.contains(&known_key) =>
            {
                Some((name, fanart_key, None))
            }
            _ => None,
        };
        let (file, job) = match (chosen, local, fanart) {
            (Some(Downloaded::Kept(checksum)), ..) => {
                let file = self.file(&format!("{checksum}-wide"));
                let made = file.exists();
                (Some(file), (!made).then_some(BackdropJob::Kept(checksum)))
            }
            (Some(_), ..) => (None, None),
            (None, Some(local), _) => {
                let file = self.file(&format!("{key}-local-wide"));
                let fresh = !older(&file, &local);
                (Some(file), (!fresh).then_some(BackdropJob::Local(local)))
            }
            (None, None, Some((name, fanart_key, kept))) => {
                let file = kept
                    .as_ref()
                    .map(|checksum| self.file(&format!("{checksum}-wide")));
                if kept.is_none() {
                    self.asked.insert(known_key.clone());
                }
                let job = BackdropJob::Fanart {
                    name: name.clone(),
                    mbid: source.mbid.clone(),
                    search: source.search,
                    key: fanart_key.clone(),
                    kept,
                };
                let made = file.as_ref().is_some_and(|file| file.exists());
                (file, (!made).then_some(job))
            }
            (None, None, None) => (None, None),
        };
        let state = match (file, job) {
            (_, Some(job)) => {
                self.send_backdrop(&known_key, job);
                Thumbnail::Pending
            }
            (Some(file), None) => Thumbnail::Ready(file),
            (None, None) => Thumbnail::None,
        };
        self.known.insert(known_key, state.clone());
        state
    }

    /// Forgets the pictures of the artist `key`, to make them again from what the library
    /// says now: after the user chose others.
    pub fn forget_artist(&mut self, key: &str) {
        self.known.remove(key);
        self.portraits.remove(key);
        self.queued.remove(key);
        if self.index.remove(key).is_some() {
            self.index_changed = true;
        }
        self.forget_backdrop(key);
    }

    /// Forgets the wide picture of the artist `key`, to look for it again.
    pub fn forget_backdrop(&mut self, key: &str) {
        let known_key = format!("{key}-backdrop");
        self.known.remove(&known_key);
        self.asked.remove(&known_key);
    }

    /// Forgets which artists had no wide picture, to look again: after the fanart.tv key
    /// changed.
    pub fn forget_missing_backdrops(&mut self) {
        self.known
            .retain(|key, state| !(key.ends_with("-backdrop") && *state == Thumbnail::None));
        self.asked.retain(|key| !key.ends_with("-backdrop"));
    }

    /// A name for an album's thumbnail that stays the same between runs: 16 hex digits.
    pub fn key(artist: Option<&str>, album: Option<&str>) -> String {
        // FNV-1a: tiny and stable, unlike the standard library's hasher.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let text = format!("{}\0{}", artist.unwrap_or(""), album.unwrap_or(""));
        for byte in text.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{hash:016x}")
    }

    /// The thumbnail for `key`: the one made before at once, while a thread looks whether the
    /// cover changed; else made from `track`'s, one of the album's tracks, as soon as a thread
    /// can.
    pub fn get(&mut self, key: &str, track: &Path) -> Thumbnail {
        if let Some(known) = self.known.get(key) {
            return known.clone();
        }
        // The cache is on this computer: it is quick to look in.
        let file = self.file(key);
        let state = if file.exists() {
            self.send_album(key, track, false);
            Thumbnail::Ready(file)
        } else {
            self.send_album(key, track, true);
            Thumbnail::Pending
        };
        self.known.insert(key.to_owned(), state.clone());
        state
    }

    /// Makes the thumbnail for `key` ahead of its being shown, when a thread is free.
    pub fn prepare(&mut self, key: &str, track: &Path) {
        if !self.known.contains_key(key) && !self.queued.contains(key) && !self.file(key).exists() {
            self.send_album(key, track, false);
        }
    }

    /// What is known about `key`'s thumbnail or picture, if it was asked for in this run.
    pub fn known(&self, key: &str) -> Option<Thumbnail> {
        self.known.get(key).cloned()
    }

    /// How many covers and pictures are being made, looked over or downloaded.
    pub fn pending(&self) -> usize {
        self.waiting
    }

    /// The file shown for `key`, once made.
    pub fn ready(&self, key: &str) -> Option<&Path> {
        match self.known.get(key) {
            Some(Thumbnail::Ready(file)) => Some(file),
            _ => None,
        }
    }

    /// Takes in what the threads did since the last call. Returns true when anything shown
    /// changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(done) = self.finished.try_recv() {
            self.waiting = self.waiting.saturating_sub(1);
            match done {
                Done::Shown {
                    key,
                    shown,
                    large,
                    remade,
                    fetched,
                } => {
                    self.fetched.extend(fetched);
                    let state = shown.clone().map_or(Thumbnail::None, Thumbnail::Ready);
                    if remade {
                        self.remade.push(key.clone());
                    }
                    changed |= remade || self.known.get(&key) != Some(&state);
                    match &large {
                        Some(large) => self.portraits.insert(key.clone(), large.clone()),
                        None => self.portraits.remove(&key),
                    };
                    // Only artists are in the index: an album's file is named by its key.
                    if self.artist_keys.contains(&key) {
                        let entry = shown.map(|shown| (shown, large));
                        if self.index.get(&key) != entry.as_ref() {
                            match entry {
                                Some(entry) => self.index.insert(key.clone(), entry),
                                None => self.index.remove(&key),
                            };
                            self.index_changed = true;
                        }
                    }
                    self.known.insert(key, state);
                }
                Done::Backdrop { key, made, fetched } => {
                    self.fetched.extend(fetched);
                    if made {
                        // Found as a file on the next `get_backdrop`, made with what the
                        // library then knows.
                        self.known.remove(&key);
                    } else {
                        self.known.insert(key, Thumbnail::None);
                    }
                    changed = true;
                }
            }
        }
        changed
    }

    /// What the threads learned from the internet since the last call, for the library to
    /// keep before it asks for those pictures again.
    pub fn take_fetched(&mut self) -> Vec<Fetched> {
        std::mem::take(&mut self.fetched)
    }

    /// The keys whose file was made again since the last call: what was read of them before
    /// is old.
    pub fn take_remade(&mut self) -> Vec<String> {
        std::mem::take(&mut self.remade)
    }

    /// Keeps the index of artists' pictures, when it changed, at most every few seconds unless
    /// `now`.
    pub fn save_index(&mut self, now: bool) {
        if !self.index_changed || (!now && self.index_saved.elapsed() < Duration::from_secs(5)) {
            return;
        }
        self.index_saved = Instant::now();
        self.index_changed = false;
        let name = |path: &Path| {
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let text: String = self
            .index
            .iter()
            .map(|(key, (shown, large))| {
                let large = large.as_deref().map(name).unwrap_or_default();
                format!("{key}\t{}\t{large}\n", name(shown))
            })
            .collect();
        let saved = create_private_dir(&self.dir).and_then(|()| {
            let partial = self.dir.join(format!("{INDEX}.partial"));
            std::fs::write(&partial, text)?;
            std::fs::rename(&partial, self.dir.join(INDEX))
        });
        if let Err(e) = saved {
            log::debug!("could not keep the index of artists' pictures: {e}");
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.jpg"))
    }

    fn send_artist(&mut self, key: &str, source: ArtistSource, urgent: bool) {
        self.artist_keys.insert(key.to_owned());
        if self.artists.is_none() {
            let (dir, pictures) = (self.dir.clone(), self.pictures.clone());
            let mut asked = HashSet::new();
            self.artists = Worker::start(
                "freya-artist-pictures",
                self.finished_tx.clone(),
                move |key, source: ArtistSource| {
                    let mut fetched = Vec::new();
                    let (shown, large, remade) =
                        resolve_artist(&dir, &pictures, &key, &source, &mut asked, &mut fetched)
                            .unwrap_or_else(|e| {
                                log::debug!("no picture for {key}: {e}");
                                (None, None, false)
                            });
                    Done::Shown {
                        key,
                        shown,
                        large,
                        remade,
                        fetched,
                    }
                },
            );
        }
        if let Some(worker) = &self.artists {
            let queue = if urgent {
                &worker.urgent
            } else {
                &worker.later
            };
            if queue.send((key.to_owned(), source)).is_ok() {
                self.waiting += 1;
                self.queued.insert(key.to_owned());
            }
        }
    }

    fn send_album(&mut self, key: &str, track: &Path, urgent: bool) {
        if self.covers.is_none() {
            let dir = self.dir.clone();
            self.covers = Worker::start(
                "freya-covers",
                self.finished_tx.clone(),
                move |key, AlbumJob(track)| {
                    let (shown, remade) = resolve_album(&dir, &key, &track).unwrap_or_else(|e| {
                        log::debug!("no thumbnail for {key}: {e}");
                        (None, false)
                    });
                    Done::Shown {
                        key,
                        shown,
                        large: None,
                        remade,
                        fetched: Vec::new(),
                    }
                },
            );
        }
        if let Some(worker) = &self.covers {
            let queue = if urgent {
                &worker.urgent
            } else {
                &worker.later
            };
            if queue
                .send((key.to_owned(), AlbumJob(track.to_owned())))
                .is_ok()
            {
                self.waiting += 1;
                self.queued.insert(key.to_owned());
            }
        }
    }

    fn send_backdrop(&mut self, key: &str, job: BackdropJob) {
        let jobs = self.backdrop_jobs.get_or_insert_with(|| {
            let (jobs, queue) = channel::<(String, BackdropJob)>();
            let dir = self.dir.clone();
            let pictures = self.pictures.clone();
            let finished = self.finished_tx.clone();
            let started = std::thread::Builder::new()
                .name("freya-backdrops".into())
                .spawn(move || {
                    let mut searched = None;
                    for (key, job) in queue {
                        let artist = key.trim_end_matches("-backdrop");
                        let mut fetched = Vec::new();
                        let made = make_backdrop(
                            &dir,
                            &pictures,
                            artist,
                            &job,
                            &mut searched,
                            &mut fetched,
                        )
                        .unwrap_or_else(|e| {
                            log::warn!("no wide picture of {artist}: {e}");
                            false
                        });
                        if finished
                            .send(Done::Backdrop { key, made, fetched })
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            if let Err(e) = started {
                log::warn!("could not start looking for wide pictures: {e}");
            }
            jobs
        });
        if jobs.send((key.to_owned(), job)).is_ok() {
            self.waiting += 1;
        }
    }
}

/// The index of artists' pictures kept in `dir`: key, file shown, large file, by tabs.
fn read_index(dir: &Path) -> HashMap<String, (PathBuf, Option<PathBuf>)> {
    let Ok(text) = std::fs::read_to_string(dir.join(INDEX)) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let (key, shown, large) = (fields.next()?, fields.next()?, fields.next()?);
            if key.is_empty() || shown.is_empty() {
                return None;
            }
            let large = (!large.is_empty()).then(|| dir.join(large));
            Some((key.to_owned(), (dir.join(shown), large)))
        })
        .collect()
}

/// What to show for the artist `key`, as `source` says, made when missing or old: the user's
/// choice; else their picture in their folder; else one from the internet, when it may be
/// asked, each once in `asked`; else their album covers. With their large picture, and whether
/// the file shown was made again. What the internet said goes into `fetched`.
fn resolve_artist(
    dir: &Path,
    pictures: &Pictures,
    key: &str,
    source: &ArtistSource,
    asked: &mut HashSet<String>,
    fetched: &mut Vec<Fetched>,
) -> Result<Made, String> {
    let chosen = source
        .chosen
        .clone()
        .filter(|chosen| *chosen != Downloaded::Unknown);
    match &chosen {
        Some(Downloaded::Kept(checksum)) => {
            if let Some(made) = from_kept(dir, pictures, checksum)? {
                return Ok(made);
            }
            return collage_of(dir, key, &source.album_tracks);
        }
        Some(_) => return collage_of(dir, key, &source.album_tracks),
        None => {}
    }
    if let Some(file) = artist_image::local(&source.album_tracks) {
        let base = format!("{key}-local");
        let (shown, large) = portrait_files(dir, &base);
        if !older(&shown, &file) && !older(&large, &file) {
            return Ok((Some(shown), Some(large), false));
        }
        let picture = image::open(&file).map_err(|e| e.to_string())?;
        let (shown, large) = save_portrait(dir, &base, &picture)?;
        return Ok((Some(shown), Some(large), true));
    }
    let online = source.online || source.fanart.is_some();
    match (&source.name, &source.deezer) {
        (Some(_), Downloaded::Kept(checksum)) if online => {
            if let Some(made) = from_kept(dir, pictures, checksum)? {
                return Ok(made);
            }
        }
        (Some(name), Downloaded::Unknown) if online && asked.insert(key.to_owned()) => {
            let found =
                online_portrait(pictures, name, None, source.online, &source.fanart, fetched);
            match found {
                Ok(Some((checksum, picture))) => {
                    let (shown, large) = save_portrait(dir, &checksum, &picture)?;
                    return Ok((Some(shown), Some(large), true));
                }
                Ok(None) => {}
                Err(e) => log::debug!("no picture of {name} from the internet: {e}"),
            }
        }
        _ => {}
    }
    collage_of(dir, key, &source.album_tracks)
}

/// What is shown for an artist: the file, their large picture, and whether it was made again.
type Made = (Option<PathBuf>, Option<PathBuf>, bool);

/// The files of a portrait named `base`: in the grid's size, and large.
fn portrait_files(dir: &Path, base: &str) -> (PathBuf, PathBuf) {
    (
        dir.join(format!("{base}-{SIZE}.jpg")),
        dir.join(format!("{base}-large.jpg")),
    )
}

/// The portrait made from the downloaded picture `checksum`, made now if missing; none when
/// the picture is gone.
fn from_kept(dir: &Path, pictures: &Pictures, checksum: &str) -> Result<Option<Made>, String> {
    let (shown, large) = portrait_files(dir, checksum);
    if shown.exists() && large.exists() {
        return Ok(Some((Some(shown), Some(large), false)));
    }
    let Some(bytes) = pictures.read(checksum) else {
        return Ok(None);
    };
    let picture = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let (shown, large) = save_portrait(dir, checksum, &picture)?;
    Ok(Some((Some(shown), Some(large), true)))
}

/// Saves `picture` as the portrait named `base`, in the grid's size and large.
fn save_portrait(
    dir: &Path,
    base: &str,
    picture: &image::DynamicImage,
) -> Result<(PathBuf, PathBuf), String> {
    let large = if picture.width() > LARGE || picture.height() > LARGE {
        picture.resize(LARGE, LARGE, image::imageops::FilterType::Triangle)
    } else {
        picture.clone()
    };
    save_jpeg(
        dir,
        &format!("{base}-large"),
        &large.to_rgb8(),
        JPEG_QUALITY,
    )?;
    let thumbnail = picture
        .resize_to_fill(SIZE, SIZE, image::imageops::FilterType::Triangle)
        .to_rgb8();
    save_jpeg(dir, &format!("{base}-{SIZE}"), &thumbnail, JPEG_QUALITY)?;
    Ok(portrait_files(dir, base))
}

/// The artist `key`'s album covers as one picture, made again when a cover changed; none when
/// they have no covers.
fn collage_of(dir: &Path, key: &str, album_tracks: &[PathBuf]) -> Result<Made, String> {
    let file = dir.join(format!("{key}-collage.jpg"));
    let fresh = album_tracks
        .first()
        .is_none_or(|track| !outdated(&file, track));
    if file.exists() && fresh {
        return Ok((Some(file), None, false));
    }
    let Some(collage) = artist_image::collage(album_tracks, SIZE) else {
        let _ = std::fs::remove_file(&file);
        return Ok((None, None, false));
    };
    save_jpeg(dir, &format!("{key}-collage"), &collage, JPEG_QUALITY)?;
    Ok((Some(file), None, true))
}

/// The thumbnail `key` from `track`'s cover: as made before while the cover is unchanged,
/// else made again; none when the album has no cover. And whether it was made again.
fn resolve_album(dir: &Path, key: &str, track: &Path) -> Result<(Option<PathBuf>, bool), String> {
    let file = dir.join(format!("{key}.jpg"));
    if file.exists() && !outdated(&file, track) {
        return Ok((Some(file), false));
    }
    let made = make(dir, key, track)?;
    Ok((made.then_some(file), made))
}

/// A picture of the artist `name` from the internet and its checksum: the one kept as
/// `kept`, else fanart.tv's best liked, by their id, else Deezer's when `deezer`; kept among
/// the downloaded pictures. What was said goes into `fetched`; none when no source has a
/// picture of them.
fn online_portrait(
    pictures: &Pictures,
    name: &str,
    kept: Option<&str>,
    deezer: bool,
    fanart: &Option<(String, String)>,
    fetched: &mut Vec<Fetched>,
) -> Result<Option<(String, image::DynamicImage)>, String> {
    if let Some(checksum) = kept
        && let Some(bytes) = pictures.read(checksum)
    {
        let picture = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
        return Ok(Some((checksum.to_owned(), picture)));
    }
    let mut asked = None;
    let mut bytes = None;
    if let Some((mbid, key)) = fanart {
        let found = artist_image::fanart_urls(mbid, key).and_then(|urls| {
            urls.portraits
                .first()
                .map(|url| artist_image::download(url))
                .transpose()
        });
        match found {
            Ok(found) => {
                asked = Some("fanart.tv");
                bytes = found;
            }
            // Deezer, then; fanart.tv is asked again next run.
            Err(e) => log::debug!("no picture of {name} from fanart.tv: {e}"),
        }
    }
    if bytes.is_none() && deezer {
        asked = Some("deezer");
        // By their name, without the id of an artist split off.
        let online = artist_image::deezer(crate::library::plain_name(name));
        // Deezer allows 50 requests in 5 seconds; this stays well below.
        std::thread::sleep(std::time::Duration::from_millis(150));
        if let artist_image::Online::Picture(found) = online? {
            bytes = Some(found);
        }
    }
    let picture = match bytes {
        Some(bytes) => {
            let picture = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
            let checksum = pictures.put(&bytes).map_err(|e| e.to_string())?;
            Some((checksum, picture))
        }
        None => None,
    };
    if let Some(source) = asked {
        let mbid = fanart
            .as_ref()
            .filter(|_| source == "fanart.tv")
            .map(|(mbid, _)| mbid.clone());
        fetched.push(Fetched::Picture {
            artist: name.to_owned(),
            kind: PictureKind::Portrait,
            source,
            picture: picture.as_ref().map(|(checksum, _)| checksum.clone()),
            mbid,
        });
    }
    Ok(picture)
}

/// Makes the wide picture for the artist `key` as `job` says. What MusicBrainz and fanart.tv
/// said goes into `fetched`. `searched` is when MusicBrainz was last asked. Returns false when
/// there is no picture.
fn make_backdrop(
    dir: &Path,
    pictures: &Pictures,
    key: &str,
    job: &BackdropJob,
    searched: &mut Option<Instant>,
    fetched: &mut Vec<Fetched>,
) -> Result<bool, String> {
    let (picture, name) = match job {
        BackdropJob::Local(file) => (
            image::open(file).map_err(|e| e.to_string())?,
            format!("{key}-local-wide"),
        ),
        BackdropJob::Kept(checksum) => {
            let Some(bytes) = pictures.read(checksum) else {
                return Ok(false);
            };
            (
                image::load_from_memory(&bytes).map_err(|e| e.to_string())?,
                format!("{checksum}-wide"),
            )
        }
        BackdropJob::Fanart {
            name,
            mbid,
            search,
            key: fanart_key,
            kept,
        } => {
            let kept = kept
                .as_deref()
                .and_then(|checksum| Some((checksum.to_owned(), pictures.read(checksum)?)));
            let (checksum, bytes) = match kept {
                Some(kept) => kept,
                None => {
                    let mbid = match mbid {
                        Some(mbid) => Some(mbid.clone()),
                        None if *search => {
                            let mbid = search_artist_id(name, searched)?;
                            fetched.push(Fetched::ArtistId {
                                artist: name.clone(),
                                mbid: mbid.clone(),
                            });
                            mbid
                        }
                        None => None,
                    };
                    let online = match &mbid {
                        Some(mbid) => artist_image::fanart_backdrop(mbid, fanart_key)?,
                        None => artist_image::Online::NoPicture,
                    };
                    let kept = match online {
                        artist_image::Online::Picture(bytes) => {
                            let checksum = pictures.put(&bytes).map_err(|e| e.to_string())?;
                            Some((checksum, bytes))
                        }
                        artist_image::Online::NoPicture => None,
                    };
                    fetched.push(Fetched::Picture {
                        artist: name.clone(),
                        kind: PictureKind::Backdrop,
                        source: "fanart.tv",
                        picture: kept.as_ref().map(|(checksum, _)| checksum.clone()),
                        mbid: mbid.clone(),
                    });
                    match kept {
                        Some(kept) => kept,
                        None => return Ok(false),
                    }
                }
            };
            let picture = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
            (picture, format!("{checksum}-wide"))
        }
    };
    let picture = if picture.width() > BACKDROP || picture.height() > BACKDROP {
        picture.resize(BACKDROP, BACKDROP, image::imageops::FilterType::Lanczos3)
    } else {
        picture
    };
    save_jpeg(dir, &name, &picture.to_rgb8(), BACKDROP_QUALITY)?;
    Ok(true)
}

/// MusicBrainz's id of the artist `name`, asked no sooner than a second after the last time,
/// `searched`.
fn search_artist_id(name: &str, searched: &mut Option<Instant>) -> Result<Option<String>, String> {
    if let Some(wait) =
        searched.and_then(|at| artist_image::MUSICBRAINZ_INTERVAL.checked_sub(at.elapsed()))
    {
        std::thread::sleep(wait);
    }
    *searched = Some(Instant::now());
    artist_image::musicbrainz_id(crate::library::plain_name(name))
}

/// Writes `picture` as `name`, under another name first and then renamed, so a half-written
/// file is never shown.
fn save_jpeg(dir: &Path, name: &str, picture: &image::RgbImage, quality: u8) -> Result<(), String> {
    create_private_dir(dir).map_err(|e| e.to_string())?;
    let partial = dir.join(format!("{name}.partial"));
    let file = std::fs::File::create(&partial).map_err(|e| e.to_string())?;
    image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), quality)
        .encode_image(picture)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&partial, dir.join(format!("{name}.jpg"))).map_err(|e| e.to_string())
}

/// Whether `made` is missing or older than `source`, which it was made from.
fn older(made: &Path, source: &Path) -> bool {
    let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    match (modified(made), modified(source)) {
        (Some(made), Some(source)) => source > made,
        (made, _) => made.is_none(),
    }
}

/// Whether the thumbnail `file` is older than where its cover may come from: the track, with
/// its embedded picture, its folder, where adding a cover image changes the folder, and the
/// images in it.
fn outdated(file: &Path, track: &Path) -> bool {
    let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let Some(made) = modified(file) else {
        return true;
    };
    let newer = |path: &Path| modified(path).is_some_and(|changed| changed > made);
    let Some(folder) = track.parent() else {
        return newer(track);
    };
    let images = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            ["jpg", "jpeg", "png", "webp", "gif", "bmp"]
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        });
    newer(track) || newer(folder) || images.into_iter().any(|entry| newer(&entry.path()))
}

/// Makes the thumbnail for `key` from `track`'s cover. Returns false when there is no cover,
/// and then removes one made before.
fn make(dir: &Path, key: &str, track: &Path) -> Result<bool, String> {
    let Some(cover) = read_cover(track, SIZE) else {
        let _ = std::fs::remove_file(dir.join(format!("{key}.jpg")));
        return Ok(false);
    };
    let rgba = image::RgbaImage::from_raw(cover.width, cover.height, cover.rgba)
        .ok_or("the cover's size does not match its pixels")?;
    let rgb = image::DynamicImage::ImageRgba8(rgba).to_rgb8();
    save_jpeg(dir, key, &rgb, JPEG_QUALITY)?;
    Ok(true)
}

/// Creates `dir`, readable only by the user where the system allows: covers say what music
/// they have.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("freya-thumbs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn thumbnails(dir: &Path) -> Thumbnails {
        Thumbnails::new(dir.join("covers"), Pictures::new(dir.join("pictures")))
    }

    /// Until the threads have done all they were asked.
    fn wait(thumbnails: &mut Thumbnails) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            thumbnails.poll();
            if thumbnails.pending() == 0 {
                break;
            }
            assert!(Instant::now() < deadline, "no thumbnail was made");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// An artist's folder with one album, whose cover is `cover`, and its track.
    fn artist_with_album(name: &str, cover: [u8; 3]) -> (PathBuf, PathBuf) {
        let artist = temp_dir(name);
        let album = artist.join("Homesick");
        std::fs::create_dir_all(&album).unwrap();
        image::RgbImage::from_pixel(600, 400, image::Rgb(cover))
            .save(album.join("cover.jpg"))
            .unwrap();
        let track = album.join("01.flac");
        std::fs::write(&track, b"").unwrap();
        (artist, track)
    }

    #[test]
    fn keys_are_stable_and_tell_albums_apart() {
        let key = Thumbnails::key(Some("Björk"), Some("Post"));
        assert_eq!(key.len(), 16);
        assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(key, Thumbnails::key(Some("Björk"), Some("Post")));
        assert_ne!(
            Thumbnails::key(Some("Björk"), Some("Post")),
            Thumbnails::key(Some("Björk"), Some("Debut"))
        );
        // The separator keeps "ab" + "c" apart from "a" + "bc".
        assert_ne!(
            Thumbnails::key(Some("ab"), Some("c")),
            Thumbnails::key(Some("a"), Some("bc"))
        );
    }

    #[test]
    fn makes_artist_pictures_from_their_folder_or_their_covers() {
        let (artist, track) = artist_with_album("artist-music", [0, 0, 200]);
        let source = ArtistSource {
            name: Some("Shores Of Null".into()),
            album_tracks: vec![track],
            online: false,
            fanart: None,
            deezer: Downloaded::Unknown,
            chosen: None,
        };
        let cache = temp_dir("artist-cache");

        // Without a picture of their own, and offline: their album's cover, square.
        let mut thumbnails = thumbnails(&cache);
        let key = Thumbnails::artist_key(source.name.as_deref());
        assert_ne!(key, Thumbnails::key(source.name.as_deref(), None));
        assert_eq!(thumbnails.get_artist(&key, &source), Thumbnail::Pending);
        wait(&mut thumbnails);
        let Thumbnail::Ready(file) = thumbnails.get_artist(&key, &source) else {
            panic!("made from the cover");
        };
        let made = image::open(&file).unwrap().to_rgb8();
        assert_eq!(made.dimensions(), (SIZE, SIZE));
        assert!(made.get_pixel(150, 150)[2] > 150, "blue, as the cover");
        assert_eq!(thumbnails.artist_portrait(&key), None, "a collage has none");

        // A picture in their folder, added later, is taken next run.
        std::thread::sleep(Duration::from_millis(20));
        image::RgbImage::from_pixel(300, 300, image::Rgb([220, 0, 0]))
            .save(artist.join("artist.jpg"))
            .unwrap();
        let mut later = super::tests::thumbnails(&cache);
        assert_eq!(later.get_artist(&key, &source), Thumbnail::Pending);
        wait(&mut later);
        let Thumbnail::Ready(file) = later.get_artist(&key, &source) else {
            panic!("made from the artist's picture");
        };
        assert!(image::open(&file).unwrap().to_rgb8().get_pixel(150, 150)[0] > 150);
        // And large, for their page.
        assert!(later.artist_portrait(&key).unwrap().exists());
        assert_eq!(later.ready(&key), Some(file.as_path()));
    }

    #[test]
    fn makes_artist_pictures_again_from_the_downloaded_one_without_the_internet() {
        let (_, track) = artist_with_album("kept-music", [0, 0, 200]);
        let cache = temp_dir("kept-cache");
        let pictures = Pictures::new(cache.join("pictures"));
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(1000, 1000, image::Rgb([0, 200, 0]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let checksum = pictures.put(&bytes).unwrap();
        let mut source = ArtistSource {
            name: Some("Ghost".into()),
            album_tracks: vec![track],
            online: true,
            fanart: None,
            deezer: Downloaded::Kept(checksum.clone()),
            chosen: None,
        };
        let mut thumbnails = thumbnails(&cache);
        let key = Thumbnails::artist_key(source.name.as_deref());
        assert_eq!(thumbnails.get_artist(&key, &source), Thumbnail::Pending);
        wait(&mut thumbnails);
        assert_eq!(thumbnails.take_fetched(), [], "Deezer was not asked");
        let Thumbnail::Ready(file) = thumbnails.get_artist(&key, &source) else {
            panic!("made from the kept picture");
        };
        assert_eq!(
            file,
            cache.join("covers").join(format!("{checksum}-{SIZE}.jpg"))
        );
        assert!(image::open(&file).unwrap().to_rgb8().get_pixel(150, 150)[1] > 150);
        assert!(thumbnails.artist_portrait(&key).unwrap().exists());

        // Deezer turned off: their covers again.
        source.online = false;
        let mut offline = super::tests::thumbnails(&cache);
        assert_eq!(offline.get_artist(&key, &source), Thumbnail::Pending);
        wait(&mut offline);
        let Thumbnail::Ready(file) = offline.get_artist(&key, &source) else {
            panic!("made from the cover");
        };
        assert!(image::open(&file).unwrap().to_rgb8().get_pixel(150, 150)[2] > 150);

        // Deezer had none lately: not asked.
        source.online = true;
        source.deezer = Downloaded::Missing;
        let mut missing = super::tests::thumbnails(&cache);
        missing.get_artist(&key, &source);
        wait(&mut missing);
        assert!(missing.take_fetched().is_empty(), "Deezer was not asked");
        assert!(matches!(
            missing.get_artist(&key, &source),
            Thumbnail::Ready(_)
        ));
    }

    #[test]
    fn the_users_choice_comes_before_the_artists_folder() {
        let (artist, track) = artist_with_album("chosen-music", [0, 0, 200]);
        image::RgbImage::from_pixel(300, 300, image::Rgb([220, 0, 0]))
            .save(artist.join("artist.jpg"))
            .unwrap();
        let cache = temp_dir("chosen-cache");
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(800, 800, image::Rgb([0, 200, 0]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let checksum = Pictures::new(cache.join("pictures")).put(&bytes).unwrap();
        let mut source = ArtistSource {
            name: Some("Ghost".into()),
            album_tracks: vec![track],
            online: false,
            fanart: None,
            deezer: Downloaded::Unknown,
            chosen: Some(Downloaded::Kept(checksum)),
        };
        let key = Thumbnails::artist_key(source.name.as_deref());
        let green = |thumbnails: &mut Thumbnails, source: &ArtistSource| {
            assert_eq!(thumbnails.get_artist(&key, source), Thumbnail::Pending);
            wait(thumbnails);
            let Thumbnail::Ready(file) = thumbnails.get_artist(&key, source) else {
                panic!("made");
            };
            image::open(&file).unwrap().to_rgb8().get_pixel(150, 150).0
        };
        let [r, g, _] = green(&mut thumbnails(&cache), &source);
        assert!(g > 150 && r < 100, "the chosen one, not the folder's");

        // None chosen: their covers, though their folder has a picture.
        source.chosen = Some(Downloaded::Missing);
        let [_, _, b] = green(&mut thumbnails(&cache), &source);
        assert!(b > 150, "the album's blue cover");
    }

    #[test]
    fn makes_wide_pictures_from_the_artists_folder_or_the_downloaded_one() {
        let (artist, track) = artist_with_album("backdrop-music", [0, 0, 200]);
        let mut source = BackdropSource {
            name: Some("Anna von Hausswolff".into()),
            album_tracks: vec![track],
            mbid: None,
            search: true,
            fanart_key: None,
            fanart: Downloaded::Unknown,
            chosen: None,
        };
        let cache = temp_dir("backdrop-cache");
        let key = Thumbnails::artist_key(source.name.as_deref());

        // None in their folder, and no key for fanart.tv.
        let mut thumbnails = thumbnails(&cache);
        assert_eq!(thumbnails.get_backdrop(&key, &source), Thumbnail::None);

        // fanart.tv had none lately: it is not asked again yet.
        source.fanart_key = Some("key".into());
        source.fanart = Downloaded::Missing;
        thumbnails.forget_missing_backdrops();
        assert_eq!(thumbnails.get_backdrop(&key, &source), Thumbnail::None);

        // The one fanart.tv gave before, made again without asking.
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(1920, 1080, image::Rgb([200, 0, 0]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let checksum = thumbnails.pictures().put(&bytes).unwrap();
        source.fanart = Downloaded::Kept(checksum.clone());
        let mut kept = super::tests::thumbnails(&cache);
        assert_eq!(kept.get_backdrop(&key, &source), Thumbnail::Pending);
        wait(&mut kept);
        assert_eq!(kept.take_fetched(), []);
        assert_eq!(
            kept.get_backdrop(&key, &source),
            Thumbnail::Ready(cache.join("covers").join(format!("{checksum}-wide.jpg")))
        );

        // One put in their folder comes first, taken at full size, up to 4K.
        image::RgbImage::from_pixel(4000, 2000, image::Rgb([0, 180, 0]))
            .save(artist.join("fanart.jpg"))
            .unwrap();
        let mut later = super::tests::thumbnails(&cache);
        assert_eq!(later.get_backdrop(&key, &source), Thumbnail::Pending);
        wait(&mut later);
        let Thumbnail::Ready(file) = later.get_backdrop(&key, &source) else {
            panic!("made from fanart.jpg");
        };
        let made = image::open(&file).unwrap();
        assert_eq!((made.width(), made.height()), (BACKDROP, 1920));
    }

    #[test]
    fn shows_an_artists_picture_made_before_at_once() {
        let (_, track) = artist_with_album("index-music", [0, 0, 200]);
        let source = ArtistSource {
            name: Some("Opeth".into()),
            album_tracks: vec![track],
            online: false,
            fanart: None,
            deezer: Downloaded::Unknown,
            chosen: None,
        };
        let cache = temp_dir("index-cache");
        let key = Thumbnails::artist_key(source.name.as_deref());
        let mut thumbnails = thumbnails(&cache);
        assert_eq!(thumbnails.get_artist(&key, &source), Thumbnail::Pending);
        wait(&mut thumbnails);
        let made = thumbnails.get_artist(&key, &source);
        assert!(matches!(made, Thumbnail::Ready(_)));
        thumbnails.save_index(true);

        // The next run: shown before a thread has looked at the music's folders.
        let mut next = super::tests::thumbnails(&cache);
        assert_eq!(next.get_artist(&key, &source), made);
        wait(&mut next);
        assert!(next.take_remade().is_empty(), "nothing changed");

        // Made ahead of its being shown.
        let other = Thumbnails::artist_key(Some("Ghost"));
        next.prepare_artist(&other, &source);
        wait(&mut next);
        assert!(matches!(next.known(&other), Some(Thumbnail::Ready(_))));
    }

    #[test]
    fn clears_a_cache_of_the_older_layout_once() {
        let cache = temp_dir("layout");
        let covers = cache.join("covers");
        std::fs::create_dir_all(&covers).unwrap();
        for name in [
            "a.jpg",
            "a-large.jpg",
            "a.collage",
            "b.nobackdrop",
            "notes.txt",
        ] {
            std::fs::write(covers.join(name), b"x").unwrap();
        }
        let thumbnails = thumbnails(&cache);
        assert_eq!(thumbnails.clear_older_layout().unwrap(), 4);
        assert!(covers.join("notes.txt").exists(), "not ours");
        std::fs::write(covers.join("c.jpg"), b"x").unwrap();
        assert_eq!(thumbnails.clear_older_layout().unwrap(), 0, "once");
        assert!(covers.join("c.jpg").exists());
        // And a new cache is marked as of this layout.
        let fresh = temp_dir("layout-new");
        assert_eq!(
            super::tests::thumbnails(&fresh)
                .clear_older_layout()
                .unwrap(),
            0
        );
        assert!(fresh.join("covers").join("layout").exists());
    }

    #[test]
    fn makes_thumbnails_once_and_looks_again_for_missing_covers_next_run() {
        let music = temp_dir("music");
        let image = image::RgbImage::from_pixel(1200, 1200, image::Rgb([200, 100, 50]));
        image.save(music.join("cover.jpg")).unwrap();
        let track = music.join("song.mp3");
        std::fs::write(&track, b"not really audio").unwrap();
        let bare = temp_dir("bare").join("song.mp3");
        std::fs::write(&bare, b"not really audio").unwrap();

        let cache = temp_dir("cache");
        let mut thumbnails = thumbnails(&cache);
        assert_eq!(thumbnails.get("a", &track), Thumbnail::Pending);
        assert_eq!(
            thumbnails.get("a", &track),
            Thumbnail::Pending,
            "asked once"
        );
        wait(&mut thumbnails);
        let Thumbnail::Ready(file) = thumbnails.get("a", &track) else {
            panic!("the thumbnail is made");
        };
        let made = image::open(&file).unwrap();
        assert_eq!((made.width(), made.height()), (SIZE, SIZE));

        assert_eq!(thumbnails.get("b", &bare), Thumbnail::Pending);
        wait(&mut thumbnails);
        assert_eq!(thumbnails.get("b", &bare), Thumbnail::None);
        assert_eq!(thumbnails.known.len(), 2);

        // A new run finds the thumbnail made, and looks again for the missing cover.
        let mut again = super::tests::thumbnails(&cache);
        assert_eq!(again.get("a", &track), Thumbnail::Ready(file.clone()));
        assert_eq!(again.get("b", &bare), Thumbnail::Pending);

        // A cover replaced after the thumbnail was made: the next run makes it again. It kept
        // the old one for good.
        std::thread::sleep(Duration::from_millis(20));
        let image = image::RgbImage::from_pixel(600, 300, image::Rgb([0, 0, 255]));
        image.save(music.join("cover.jpg")).unwrap();
        // The old one shows at once, while a thread looks.
        let mut later = super::tests::thumbnails(&cache);
        assert_eq!(later.get("a", &track), Thumbnail::Ready(file.clone()));
        wait(&mut later);
        assert_eq!(later.take_remade(), ["a"]);
        let Thumbnail::Ready(file) = later.get("a", &track) else {
            panic!("the thumbnail is made again");
        };
        let made = image::open(&file).unwrap();
        assert_eq!((made.width(), made.height()), (SIZE, SIZE / 2));
    }
}
