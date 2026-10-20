//! Small album covers for grids, and artists' pictures in the sizes shown, made from Freya's
//! own pictures (see [`crate::pictures`]): the covers and pictures a scan kept. Never from the
//! music's folders, which may be on a network drive, nor the internet, so the window never
//! waits for either. Each is made once, on threads of their own, and kept as a file, so
//! showing thousands of albums only ever decodes the covers on screen; and every artist's and
//! album's are made ahead, a few at a time, so they are there when scrolled to.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::time::Duration;

use crate::artist_image;
use crate::pictures::Pictures;

/// Where the pictures thumbnails are made from come from: the picture kept as a checksum.
pub type PictureSource = std::sync::Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

/// The pictures in `pictures`, as a source of them.
pub fn source(pictures: Pictures) -> PictureSource {
    std::sync::Arc::new(move |checksum| pictures.read(checksum))
}

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

/// What is kept of an artist's picture: see [`Thumbnails::get_artist`].
#[derive(Clone, Debug, Default)]
pub struct ArtistSource {
    /// Their picture as kept: the one the user chose, or the one a scan found first.
    pub picture: Option<String>,
    /// Their albums' covers among Freya's pictures, for a collage while they have no picture.
    pub covers: Vec<String>,
}

/// A kept picture to make in the grid's size.
struct PictureJob;

/// What a thread makes of an artist's picture.
enum ArtistWork {
    /// Their portrait from the kept picture, else their covers' collage.
    Kept(String, Vec<String>),
    /// Their albums' covers in one picture.
    Collage(Vec<String>),
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
    },
    /// A wide picture, made or not.
    Backdrop { key: String, made: bool },
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

pub struct Thumbnails {
    dir: PathBuf,
    pictures: PictureSource,
    /// Albums and artists asked for, with what is known about them.
    known: HashMap<String, Thumbnail>,
    /// The large picture of each artist asked for, by their key, once made.
    portraits: HashMap<String, PathBuf>,
    /// Asked of a thread in this run, by their key: each once.
    queued: HashSet<String>,
    /// Jobs a thread has not done yet.
    waiting: usize,
    covers: Option<Worker<PictureJob>>,
    artists: Option<Worker<ArtistWork>>,
    /// Wide pictures, on a thread of their own: each is large, and would hold up the covers.
    backdrop_jobs: Option<Sender<(String, String)>>,
    finished_tx: Sender<Done>,
    finished: Receiver<Done>,
    /// Keys whose file was made again since the library last asked, so it reads them anew.
    remade: Vec<String>,
}

impl Thumbnails {
    /// Thumbnails kept in `dir`, which is created when the first one is made, from covers and
    /// from the downloaded `pictures`.
    pub fn new(dir: PathBuf, pictures: PictureSource) -> Self {
        let (finished_tx, finished) = channel();
        Self {
            dir,
            pictures,
            known: HashMap::new(),
            portraits: HashMap::new(),
            queued: HashSet::new(),
            waiting: 0,
            covers: None,
            artists: None,
            backdrop_jobs: None,
            finished_tx,
            finished,
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

    /// A name for an artist's picture that stays the same between runs, apart from any
    /// album's.
    pub fn artist_key(artist: Option<&str>) -> String {
        Self::key(Some("\u{1}artist"), artist)
    }

    /// The picture for the artist `key`, as `source` says: their kept picture, else their
    /// albums' covers. Shown at once when made before; else made as soon as a thread can,
    /// from Freya's own pictures: never from the music's files, nor the internet.
    pub fn get_artist(&mut self, key: &str, source: &ArtistSource) -> Thumbnail {
        if let Some(known) = self.known.get(key) {
            return known.clone();
        }
        let (shown, work) = self.plan_artist(key, source);
        if let Some(work) = work {
            self.send_artist(key, work, true);
        }
        let state = shown.unwrap_or(Thumbnail::Pending);
        self.known.insert(key.to_owned(), state.clone());
        state
    }

    /// Makes the picture for the artist `key` ahead of its being shown, when a thread is
    /// free: when it is not made yet.
    pub fn prepare_artist(&mut self, key: &str, source: &ArtistSource) {
        if self.known.contains_key(key) || self.queued.contains(key) {
            return;
        }
        let (shown, work) = self.plan_artist(key, source);
        if let Some(shown) = shown {
            self.known.insert(key.to_owned(), shown);
        }
        if let Some(work) = work {
            self.send_artist(key, work, false);
        }
    }

    /// What to show for the artist `key` now, when made, and what a thread is to make.
    fn plan_artist(
        &mut self,
        key: &str,
        source: &ArtistSource,
    ) -> (Option<Thumbnail>, Option<ArtistWork>) {
        let covers = source.covers.clone();
        match &source.picture {
            Some(checksum) => {
                let (shown, large) = portrait_files(&self.dir, checksum);
                if shown.exists() && large.exists() {
                    self.portraits.insert(key.to_owned(), large);
                    (Some(Thumbnail::Ready(shown)), None)
                } else {
                    (None, Some(ArtistWork::Kept(checksum.clone(), covers)))
                }
            }
            None if covers.is_empty() => (Some(Thumbnail::None), None),
            None => {
                let file = self.file(&collage_name(key, &covers));
                if file.exists() {
                    (Some(Thumbnail::Ready(file)), None)
                } else {
                    (None, Some(ArtistWork::Collage(covers)))
                }
            }
        }
    }

    /// The artist `key`'s picture at up to 1400 pixels, for their page, once made; none when
    /// it is a collage of their covers.
    pub fn artist_portrait(&self, key: &str) -> Option<PathBuf> {
        self.portraits.get(key).cloned()
    }

    /// The wide picture across the top of the page of the artist `key`: `picture`, theirs as
    /// kept, made in its size when missing; none when they have none.
    pub fn get_backdrop(&mut self, key: &str, picture: Option<&str>) -> Thumbnail {
        let known_key = format!("{key}-backdrop");
        if let Some(known) = self.known.get(&known_key) {
            return known.clone();
        }
        let state = match picture {
            None => Thumbnail::None,
            Some(checksum) => {
                let file = self.file(&format!("{checksum}-wide"));
                if file.exists() {
                    Thumbnail::Ready(file)
                } else {
                    self.send_backdrop(&known_key, checksum);
                    Thumbnail::Pending
                }
            }
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
        self.forget_backdrop(key);
    }

    /// Forgets the wide picture of the artist `key`, to show it again from what the library
    /// says now.
    pub fn forget_backdrop(&mut self, key: &str) {
        self.known.remove(&format!("{key}-backdrop"));
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

    /// The kept picture `checksum`, an album's cover, in the grid's size: made from Freya's own
    /// copy when missing, never from the music's files.
    pub fn get_picture(&mut self, checksum: &str) -> Thumbnail {
        if let Some(known) = self.known.get(checksum) {
            return known.clone();
        }
        // The cache is on this computer: it is quick to look in.
        let file = self.file(&format!("{checksum}-{SIZE}"));
        let state = if file.exists() {
            Thumbnail::Ready(file)
        } else {
            self.send_picture(checksum, true);
            Thumbnail::Pending
        };
        self.known.insert(checksum.to_owned(), state.clone());
        state
    }

    /// Makes the kept picture `checksum` in the grid's size ahead of its being shown.
    pub fn prepare_picture(&mut self, checksum: &str) {
        let missing = !self.known.contains_key(checksum)
            && !self.queued.contains(checksum)
            && !self.file(&format!("{checksum}-{SIZE}")).exists();
        if missing {
            self.send_picture(checksum, false);
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
                } => {
                    let state = shown.clone().map_or(Thumbnail::None, Thumbnail::Ready);
                    if remade {
                        self.remade.push(key.clone());
                    }
                    changed |= remade || self.known.get(&key) != Some(&state);
                    match &large {
                        Some(large) => self.portraits.insert(key.clone(), large.clone()),
                        None => self.portraits.remove(&key),
                    };
                    self.known.insert(key, state);
                }
                Done::Backdrop { key, made } => {
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

    /// The keys whose file was made again since the last call: what was read of them before
    /// is old.
    pub fn take_remade(&mut self) -> Vec<String> {
        std::mem::take(&mut self.remade)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.jpg"))
    }

    fn send_artist(&mut self, key: &str, work: ArtistWork, urgent: bool) {
        if self.artists.is_none() {
            let (dir, pictures) = (self.dir.clone(), self.pictures.clone());
            self.artists = Worker::start(
                "freya-artist-pictures",
                self.finished_tx.clone(),
                move |key, work: ArtistWork| {
                    let (shown, large, remade) = make_artist(&dir, &pictures, &key, work)
                        .unwrap_or_else(|e| {
                            log::debug!("no picture for {key}: {e}");
                            (None, None, false)
                        });
                    Done::Shown {
                        key,
                        shown,
                        large,
                        remade,
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
            if queue.send((key.to_owned(), work)).is_ok() {
                self.waiting += 1;
                self.queued.insert(key.to_owned());
            }
        }
    }

    fn send_picture(&mut self, checksum: &str, urgent: bool) {
        if self.covers.is_none() {
            let (dir, pictures) = (self.dir.clone(), self.pictures.clone());
            self.covers = Worker::start(
                "freya-covers",
                self.finished_tx.clone(),
                move |checksum, PictureJob| {
                    let shown = shrink(&dir, &pictures, &checksum).unwrap_or_else(|e| {
                        log::debug!("no thumbnail of the picture {checksum}: {e}");
                        None
                    });
                    Done::Shown {
                        key: checksum,
                        shown,
                        large: None,
                        remade: false,
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
            if queue.send((checksum.to_owned(), PictureJob)).is_ok() {
                self.waiting += 1;
                self.queued.insert(checksum.to_owned());
            }
        }
    }

    fn send_backdrop(&mut self, key: &str, checksum: &str) {
        let jobs = self.backdrop_jobs.get_or_insert_with(|| {
            let (jobs, queue) = channel::<(String, String)>();
            let dir = self.dir.clone();
            let pictures = self.pictures.clone();
            let finished = self.finished_tx.clone();
            let started = std::thread::Builder::new()
                .name("freya-backdrops".into())
                .spawn(move || {
                    for (key, checksum) in queue {
                        let made = make_backdrop(&dir, &pictures, &checksum).unwrap_or_else(|e| {
                            log::warn!("no wide picture {checksum}: {e}");
                            false
                        });
                        if finished.send(Done::Backdrop { key, made }).is_err() {
                            break;
                        }
                    }
                });
            if let Err(e) = started {
                log::warn!("could not start making wide pictures: {e}");
            }
            jobs
        });
        if jobs.send((key.to_owned(), checksum.to_owned())).is_ok() {
            self.waiting += 1;
        }
    }
}

/// Makes the picture for the artist `key` as `work` says, from Freya's pictures only.
fn make_artist(
    dir: &Path,
    pictures: &PictureSource,
    key: &str,
    work: ArtistWork,
) -> Result<Made, String> {
    match work {
        ArtistWork::Kept(checksum, covers) => match from_kept(dir, pictures, &checksum)? {
            Some(made) => Ok(made),
            None => collage_of(dir, pictures, key, &covers),
        },
        ArtistWork::Collage(covers) => collage_of(dir, pictures, key, &covers),
    }
}

/// The name of the collage of `covers` for the artist `key`: another set of covers, another
/// collage.
fn collage_name(key: &str, covers: &[String]) -> String {
    let covers = Thumbnails::key(Some(&covers.join(",")), None);
    format!("{key}-collage-{covers}")
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
fn from_kept(dir: &Path, pictures: &PictureSource, checksum: &str) -> Result<Option<Made>, String> {
    let (shown, large) = portrait_files(dir, checksum);
    if shown.exists() && large.exists() {
        return Ok(Some((Some(shown), Some(large), false)));
    }
    let Some(bytes) = pictures(checksum) else {
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

/// The artist `key`'s albums' covers, among Freya's pictures, as one picture; none when they
/// have none.
fn collage_of(
    dir: &Path,
    pictures: &PictureSource,
    key: &str,
    covers: &[String],
) -> Result<Made, String> {
    let name = collage_name(key, covers);
    let file = dir.join(format!("{name}.jpg"));
    if file.exists() {
        return Ok((Some(file), None, false));
    }
    let images: Vec<image::DynamicImage> = covers
        .iter()
        .filter_map(|checksum| image::load_from_memory(&pictures(checksum)?).ok())
        .take(4)
        .collect();
    let Some(collage) = artist_image::collage_of(images, SIZE) else {
        return Ok((None, None, false));
    };
    save_jpeg(dir, &name, &collage, JPEG_QUALITY)?;
    Ok((Some(file), None, true))
}

/// The kept picture `checksum` in the grid's size, made from Freya's copy; none when that is
/// gone.
fn shrink(dir: &Path, pictures: &PictureSource, checksum: &str) -> Result<Option<PathBuf>, String> {
    let Some(bytes) = pictures(checksum) else {
        return Ok(None);
    };
    let picture = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let small = if picture.width() > SIZE || picture.height() > SIZE {
        picture.thumbnail(SIZE, SIZE)
    } else {
        picture
    };
    let name = format!("{checksum}-{SIZE}");
    save_jpeg(dir, &name, &small.to_rgb8(), JPEG_QUALITY)?;
    Ok(Some(dir.join(format!("{name}.jpg"))))
}

/// Makes the kept picture `checksum` in the size of a wide picture across an artist's page.
/// Returns false when the picture is gone.
fn make_backdrop(dir: &Path, pictures: &PictureSource, checksum: &str) -> Result<bool, String> {
    let Some(bytes) = pictures(checksum) else {
        return Ok(false);
    };
    let picture = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let picture = if picture.width() > BACKDROP || picture.height() > BACKDROP {
        picture.resize(BACKDROP, BACKDROP, image::imageops::FilterType::Lanczos3)
    } else {
        picture
    };
    save_jpeg(
        dir,
        &format!("{checksum}-wide"),
        &picture.to_rgb8(),
        BACKDROP_QUALITY,
    )?;
    Ok(true)
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
    use std::time::Instant;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("freya-thumbs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn thumbnails(dir: &Path) -> Thumbnails {
        Thumbnails::new(dir.join("covers"), source(pictures_of(dir)))
    }

    /// The pictures thumbnails in `dir` are made from.
    fn pictures_of(dir: &Path) -> Pictures {
        Pictures::new(dir.join("pictures"))
    }

    /// The pictures `thumbnails` are made from.
    fn pictures(thumbnails: &Thumbnails) -> Pictures {
        pictures_of(thumbnails.dir().parent().unwrap())
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

    /// A picture of one color kept among `thumbnails`' pictures: its checksum.
    fn kept(thumbnails: &Thumbnails, rgb: [u8; 3], width: u32, height: u32) -> String {
        let mut jpeg = Vec::new();
        image::RgbImage::from_pixel(width, height, image::Rgb(rgb))
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        pictures(thumbnails).put(&jpeg).unwrap()
    }

    /// The color in the middle of the picture shown for `key`, once made.
    fn shown(thumbnails: &mut Thumbnails, key: &str, source: &ArtistSource) -> [u8; 3] {
        if thumbnails.get_artist(key, source) == Thumbnail::Pending {
            wait(thumbnails);
        }
        let Thumbnail::Ready(file) = thumbnails.get_artist(key, source) else {
            panic!("made");
        };
        let made = image::open(&file).unwrap().to_rgb8();
        made.get_pixel(made.width() / 2, made.height() / 2).0
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
    fn makes_artist_pictures_from_their_kept_one_or_their_covers() {
        let cache = temp_dir("artist-cache");
        let mut thumbnails = thumbnails(&cache);
        let blue = kept(&thumbnails, [0, 0, 200], 600, 400);
        let red = kept(&thumbnails, [220, 0, 0], 800, 800);
        let mut source = ArtistSource::default();
        let key = Thumbnails::artist_key(Some("Shores Of Null"));
        assert_ne!(key, Thumbnails::key(Some("Shores Of Null"), None));

        // Without any picture or cover: nothing to make.
        assert_eq!(thumbnails.get_artist(&key, &source), Thumbnail::None);

        // Their album's cover, square.
        source.covers = vec![blue];
        let mut covers = super::tests::thumbnails(&cache);
        assert!(
            shown(&mut covers, &key, &source)[2] > 150,
            "blue, as the cover"
        );
        assert_eq!(covers.artist_portrait(&key), None, "a collage has none");

        // Their kept picture, large too, for their page.
        source.picture = Some(red.clone());
        let mut picture = super::tests::thumbnails(&cache);
        assert!(shown(&mut picture, &key, &source)[0] > 150);
        assert!(picture.artist_portrait(&key).unwrap().exists());
        assert_eq!(
            picture.ready(&key),
            Some(
                cache
                    .join("covers")
                    .join(format!("{red}-{SIZE}.jpg"))
                    .as_path()
            )
        );

        // Made before: shown at once the next run, with no thread asked.
        let mut next = super::tests::thumbnails(&cache);
        assert!(matches!(
            next.get_artist(&key, &source),
            Thumbnail::Ready(_)
        ));
        assert_eq!(next.pending(), 0);

        // A kept picture that is gone: their covers.
        source.picture = Some("gone".into());
        let mut gone = super::tests::thumbnails(&cache);
        assert!(shown(&mut gone, &key, &source)[2] > 150);
    }

    #[test]
    fn makes_wide_pictures_from_the_kept_one() {
        let cache = temp_dir("backdrop-cache");
        let mut thumbnails = thumbnails(&cache);
        let key = Thumbnails::artist_key(Some("Anna von Hausswolff"));
        assert_eq!(thumbnails.get_backdrop(&key, None), Thumbnail::None);

        let green = kept(&thumbnails, [0, 180, 0], 4000, 2000);
        let mut made = super::tests::thumbnails(&cache);
        assert_eq!(made.get_backdrop(&key, Some(&green)), Thumbnail::Pending);
        wait(&mut made);
        let Thumbnail::Ready(file) = made.get_backdrop(&key, Some(&green)) else {
            panic!("made from the kept one");
        };
        assert_eq!(file, cache.join("covers").join(format!("{green}-wide.jpg")));
        let picture = image::open(&file).unwrap();
        assert_eq!((picture.width(), picture.height()), (BACKDROP, 1920));

        // Made before: at once.
        let mut again = super::tests::thumbnails(&cache);
        assert_eq!(
            again.get_backdrop(&key, Some(&green)),
            Thumbnail::Ready(file)
        );
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
    fn makes_a_kept_cover_in_the_grids_size_once() {
        let cache = temp_dir("cache");
        let mut thumbnails = thumbnails(&cache);
        let mut jpeg = Vec::new();
        image::RgbImage::from_pixel(600, 300, image::Rgb([0, 0, 255]))
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let checksum = pictures(&thumbnails).put(&jpeg).unwrap();
        assert_eq!(thumbnails.get_picture(&checksum), Thumbnail::Pending);
        assert_eq!(
            thumbnails.get_picture(&checksum),
            Thumbnail::Pending,
            "asked once"
        );
        wait(&mut thumbnails);
        let Thumbnail::Ready(file) = thumbnails.get_picture(&checksum) else {
            panic!("the thumbnail is made");
        };
        let made = image::open(&file).unwrap();
        assert_eq!((made.width(), made.height()), (SIZE, SIZE / 2));

        // A new run finds it made; a picture Freya does not have has none.
        let mut again = super::tests::thumbnails(&cache);
        assert_eq!(again.get_picture(&checksum), Thumbnail::Ready(file));
        assert_eq!(again.get_picture("gone"), Thumbnail::Pending);
        wait(&mut again);
        assert_eq!(again.get_picture("gone"), Thumbnail::None);
    }
}
