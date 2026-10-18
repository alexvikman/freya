//! Small album covers for grids, and artists' pictures in the sizes shown. Each is made once,
//! on a thread of its own, and kept as a file, so showing thousands of albums only ever decodes
//! the covers on screen. All of them can be made again without the internet: from the music's
//! folders, or from the pictures downloaded for artists, which are kept apart (see
//! [`crate::pictures`]). That an album has no cover is only remembered until Freya quits, so a
//! cover added later is found.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

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

/// A thumbnail to make: an album's cover from one of its tracks, or an artist's picture.
enum Job {
    Album(PathBuf),
    Artist {
        job: ArtistJob,
        album_tracks: Vec<PathBuf>,
    },
}

/// Where an artist's picture is made from.
enum ArtistJob {
    /// Their picture in their folder.
    Local(PathBuf),
    /// One kept among the downloaded pictures: the user's choice.
    Kept(String),
    /// From the internet: the one kept when there is one, else asked of fanart.tv by their
    /// MusicBrainz id and key, then of Deezer by their name when it may be.
    Online {
        name: String,
        kept: Option<String>,
        deezer: bool,
        fanart: Option<(String, String)>,
    },
    /// Their album covers.
    Collage,
}

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

/// What a thread did: the thumbnail's key, whether it was made, and what it learned.
type Finished = (String, bool, Vec<Fetched>);

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

pub struct Thumbnails {
    dir: PathBuf,
    pictures: Pictures,
    /// Albums and artists asked for, with what is known about them.
    known: HashMap<String, Thumbnail>,
    /// The large picture of each artist asked for, by their key, once made.
    portraits: HashMap<String, PathBuf>,
    /// Pictures asked of the internet in this run, by their key: each once, even when it
    /// could not be reached.
    asked: HashSet<String>,
    /// Started with the first request.
    jobs: Option<Sender<(String, Job)>>,
    /// Wide pictures, on a thread of their own: MusicBrainz is asked once a second at most,
    /// which would hold up the covers.
    backdrop_jobs: Option<Sender<(String, BackdropJob)>>,
    finished_tx: Sender<Finished>,
    finished: Receiver<Finished>,
    /// What the threads learned, until the library takes it.
    fetched: Vec<Fetched>,
}

impl Thumbnails {
    /// Thumbnails kept in `dir`, which is created when the first one is made, from covers and
    /// from the downloaded `pictures`.
    pub fn new(dir: PathBuf, pictures: Pictures) -> Self {
        let (finished_tx, finished) = channel();
        Self {
            dir,
            pictures,
            known: HashMap::new(),
            portraits: HashMap::new(),
            asked: HashSet::new(),
            jobs: None,
            backdrop_jobs: None,
            finished_tx,
            finished,
            fetched: Vec::new(),
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

    /// The picture for the artist `key`. When it is not made yet, it is made from `source`:
    /// their picture in their folder, else Deezer's when allowed, else their album covers.
    pub fn get_artist(&mut self, key: &str, source: &ArtistSource) -> Thumbnail {
        if let Some(known) = self.known.get(key) {
            return known.clone();
        }
        let chosen = source
            .chosen
            .clone()
            .filter(|chosen| *chosen != Downloaded::Unknown);
        let local = artist_image::local(&source.album_tracks).filter(|_| chosen.is_none());
        let online = source.online || source.fanart.is_some();
        let deezer = match (&source.name, &source.deezer) {
            _ if chosen.is_some() => None,
            (Some(name), Downloaded::Kept(checksum)) if online => {
                Some((name.clone(), Some(checksum.clone())))
            }
            (Some(name), Downloaded::Unknown) if online && !self.asked.contains(key) => {
                Some((name.clone(), None))
            }
            _ => None,
        };
        let (thumbnail, large, job) = match (chosen, local, deezer) {
            (Some(Downloaded::Kept(checksum)), ..) => {
                let thumbnail = self.file(&format!("{checksum}-{SIZE}"));
                let large = self.file(&format!("{checksum}-large"));
                let made = thumbnail.exists() && large.exists();
                (
                    thumbnail,
                    Some(large),
                    (!made).then_some(ArtistJob::Kept(checksum)),
                )
            }
            (Some(_), ..) | (None, None, None) => {
                let collage = self.file(&format!("{key}-collage"));
                let fresh = source
                    .album_tracks
                    .first()
                    .is_none_or(|track| !outdated(&collage, track));
                let job = (!(collage.exists() && fresh)).then_some(ArtistJob::Collage);
                (collage, None, job)
            }
            (None, Some(file), _) => {
                let thumbnail = self.file(&format!("{key}-local"));
                let large = self.file(&format!("{key}-local-large"));
                let fresh = [&thumbnail, &large].iter().all(|made| !older(made, &file));
                (
                    thumbnail,
                    Some(large),
                    (!fresh).then_some(ArtistJob::Local(file)),
                )
            }
            (None, None, Some((name, Some(checksum)))) => {
                let thumbnail = self.file(&format!("{checksum}-{SIZE}"));
                let large = self.file(&format!("{checksum}-large"));
                let made = thumbnail.exists() && large.exists();
                let job = ArtistJob::Online {
                    name,
                    kept: Some(checksum),
                    deezer: source.online,
                    fanart: source.fanart.clone(),
                };
                (thumbnail, Some(large), (!made).then_some(job))
            }
            (None, None, Some((name, None))) => {
                self.asked.insert(key.to_owned());
                let job = ArtistJob::Online {
                    name,
                    kept: None,
                    deezer: source.online,
                    fanart: source.fanart.clone(),
                };
                // Its name is known once the internet answered.
                (PathBuf::new(), None, Some(job))
            }
        };
        match large {
            Some(large) if job.is_none() => self.portraits.insert(key.to_owned(), large),
            _ => self.portraits.remove(key),
        };
        let state = match job {
            None => Thumbnail::Ready(thumbnail),
            Some(job) => {
                let album_tracks = source.album_tracks.clone();
                self.send(key, Job::Artist { job, album_tracks });
                Thumbnail::Pending
            }
        };
        self.known.insert(key.to_owned(), state.clone());
        state
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

    /// The thumbnail for `key`. When it is not made yet, it is made from the cover of `track`,
    /// one of the album's tracks.
    pub fn get(&mut self, key: &str, track: &Path) -> Thumbnail {
        if let Some(known) = self.known.get(key) {
            return known.clone();
        }
        let file = self.file(key);
        let state = if file.exists() && !outdated(&file, track) {
            Thumbnail::Ready(file)
        } else {
            self.send(key, Job::Album(track.to_owned()));
            Thumbnail::Pending
        };
        self.known.insert(key.to_owned(), state.clone());
        state
    }

    /// How many covers and pictures are being made or downloaded.
    pub fn pending(&self) -> usize {
        self.known
            .values()
            .filter(|state| **state == Thumbnail::Pending)
            .count()
    }

    /// The file shown for `key`, once made.
    pub fn ready(&self, key: &str) -> Option<&Path> {
        match self.known.get(key) {
            Some(Thumbnail::Ready(file)) => Some(file),
            _ => None,
        }
    }

    /// Takes in thumbnails made since the last call. Returns true when there were any.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((key, made, fetched)) = self.finished.try_recv() {
            if made {
                // Found as a file on the next `get`, made with what the library then knows.
                self.known.remove(&key);
            } else {
                self.known.insert(key, Thumbnail::None);
            }
            self.fetched.extend(fetched);
            any = true;
        }
        any
    }

    /// What the threads learned from the internet since the last call, for the library to
    /// keep before it asks for those pictures again.
    pub fn take_fetched(&mut self) -> Vec<Fetched> {
        std::mem::take(&mut self.fetched)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.jpg"))
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
                        if finished.send((key, made, fetched)).is_err() {
                            break;
                        }
                    }
                });
            if let Err(e) = started {
                log::warn!("could not start looking for wide pictures: {e}");
            }
            jobs
        });
        let _ = jobs.send((key.to_owned(), job));
    }

    fn send(&mut self, key: &str, job: Job) {
        let jobs = self.jobs.get_or_insert_with(|| {
            let (jobs, queue) = channel::<(String, Job)>();
            let dir = self.dir.clone();
            let pictures = self.pictures.clone();
            let finished = self.finished_tx.clone();
            let started = std::thread::Builder::new()
                .name("freya-covers".into())
                .spawn(move || {
                    for (key, job) in queue {
                        let mut fetched = Vec::new();
                        let made = match &job {
                            Job::Album(track) => make(&dir, &key, track),
                            Job::Artist { job, album_tracks } => {
                                make_artist(&dir, &pictures, &key, job, album_tracks, &mut fetched)
                            }
                        };
                        let made = match made {
                            Ok(made) => made,
                            Err(e) => {
                                log::debug!("no thumbnail for {key}: {e}");
                                false
                            }
                        };
                        if finished.send((key, made, fetched)).is_err() {
                            break;
                        }
                    }
                });
            if let Err(e) = started {
                log::warn!("could not start making covers: {e}");
            }
            jobs
        });
        let _ = jobs.send((key.to_owned(), job));
    }
}

/// Makes the picture for the artist `key` as `job` says, in the grid's size and large. What
/// Deezer gave goes into `fetched`. Without a picture from Deezer, whether it had none or could
/// not be reached, their album covers stand in. Returns false when there is nothing to show.
fn make_artist(
    dir: &Path,
    pictures: &Pictures,
    key: &str,
    job: &ArtistJob,
    album_tracks: &[PathBuf],
    fetched: &mut Vec<Fetched>,
) -> Result<bool, String> {
    let (picture, name) = match job {
        ArtistJob::Local(file) => (
            Some(image::open(file).map_err(|e| e.to_string())?),
            format!("{key}-local"),
        ),
        ArtistJob::Kept(checksum) => match pictures.read(checksum) {
            Some(bytes) => (
                Some(image::load_from_memory(&bytes).map_err(|e| e.to_string())?),
                checksum.clone(),
            ),
            // Gone: their covers stand in.
            None => (None, String::new()),
        },
        ArtistJob::Online {
            name,
            kept,
            deezer,
            fanart,
        } => match online_portrait(pictures, name, kept.as_deref(), *deezer, fanart, fetched) {
            Ok(Some((checksum, picture))) => (Some(picture), checksum),
            Ok(None) => (None, String::new()),
            Err(e) => {
                log::debug!("no picture of {name} from the internet: {e}");
                (None, String::new())
            }
        },
        ArtistJob::Collage => (None, String::new()),
    };
    let Some(picture) = picture else {
        let Some(collage) = artist_image::collage(album_tracks, SIZE) else {
            return Ok(false);
        };
        save_jpeg(dir, &format!("{key}-collage"), &collage, JPEG_QUALITY)?;
        return Ok(true);
    };
    let large = if picture.width() > LARGE || picture.height() > LARGE {
        picture.resize(LARGE, LARGE, image::imageops::FilterType::Triangle)
    } else {
        picture.clone()
    };
    save_jpeg(
        dir,
        &format!("{name}-large"),
        &large.to_rgb8(),
        JPEG_QUALITY,
    )?;
    let thumbnail = picture
        .resize_to_fill(SIZE, SIZE, image::imageops::FilterType::Triangle)
        .to_rgb8();
    let size = if name.ends_with("-local") {
        String::new()
    } else {
        format!("-{SIZE}")
    };
    save_jpeg(dir, &format!("{name}{size}"), &thumbnail, JPEG_QUALITY)?;
    Ok(true)
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

    fn wait(thumbnails: &mut Thumbnails) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !thumbnails.poll() {
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
        let mut later = super::tests::thumbnails(&cache);
        assert_eq!(later.get("a", &track), Thumbnail::Pending);
        wait(&mut later);
        let Thumbnail::Ready(file) = later.get("a", &track) else {
            panic!("the thumbnail is made again");
        };
        let made = image::open(&file).unwrap();
        assert_eq!((made.width(), made.height()), (SIZE, SIZE / 2));
    }
}
