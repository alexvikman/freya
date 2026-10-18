//! Small album covers for grids. Each is made once, on a thread of its own, and kept as a file,
//! so showing thousands of albums only ever decodes the covers on screen. That an album has no
//! cover is only remembered until Freya quits, so a cover added later is found.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use crate::cover::read_cover;

/// The longest side of a thumbnail, in pixels: sharp in a 150-point grid tile on a Retina
/// screen, about 30 KB as a JPEG.
pub const SIZE: u32 = 300;
const JPEG_QUALITY: u8 = 85;
/// The longest side of an artist's picture at the top of their page.
const LARGE: u32 = 1400;
/// The longest side of a wide picture across the top of an artist's page: sharp across a 4K
/// screen.
const BACKDROP: u32 = 3840;
const BACKDROP_QUALITY: u8 = 90;
/// How long fanart.tv having no wide picture of an artist is believed, before it is asked
/// again: its users add pictures all the time.
const NO_BACKDROP_KEPT: Duration = Duration::from_secs(30 * 24 * 3600);

/// Where an artist's picture can come from: see [`crate::artist_image`].
#[derive(Clone, Debug)]
pub struct ArtistSource {
    pub name: Option<String>,
    /// A track of each of the artist's albums.
    pub album_tracks: Vec<PathBuf>,
    /// Whether Deezer may be asked.
    pub online: bool,
}

/// Where a wide picture of an artist can come from: see [`crate::artist_image`].
#[derive(Clone, Debug)]
pub struct BackdropSource {
    pub name: Option<String>,
    /// A track of each of the artist's albums.
    pub album_tracks: Vec<PathBuf>,
    /// Their MusicBrainz id, when their tags give it.
    pub mbid: Option<String>,
    /// The user's personal fanart.tv key; without one, fanart.tv is not asked.
    pub fanart_key: Option<String>,
}

/// A thumbnail to make: an album's cover from one of its tracks, or an artist's picture.
enum Job {
    Album(PathBuf),
    Artist(ArtistSource),
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

pub struct Thumbnails {
    dir: PathBuf,
    /// Albums asked for, with what is known about them.
    known: HashMap<String, Thumbnail>,
    /// Started with the first request.
    jobs: Option<Sender<(String, Job)>>,
    /// Wide pictures, on a thread of their own: MusicBrainz is asked once a second at most,
    /// which would hold up the covers.
    backdrop_jobs: Option<Sender<(String, BackdropSource)>>,
    /// Albums whose covers were looked for, and whether a thumbnail was made.
    finished_tx: Sender<(String, bool)>,
    finished: Receiver<(String, bool)>,
}

impl Thumbnails {
    /// Thumbnails kept in `dir`, which is created when the first one is made.
    pub fn new(dir: PathBuf) -> Self {
        let (finished_tx, finished) = channel();
        Self {
            dir,
            known: HashMap::new(),
            jobs: None,
            backdrop_jobs: None,
            finished_tx,
            finished,
        }
    }

    /// A name for an artist's picture that stays the same between runs, apart from any
    /// album's.
    pub fn artist_key(artist: Option<&str>) -> String {
        Self::key(Some("\u{1}artist"), artist)
    }

    /// The picture for the artist `key`. When it is not made yet, it is made from `source`.
    pub fn get_artist(&mut self, key: &str, source: &ArtistSource) -> Thumbnail {
        if let Some(known) = self.known.get(key) {
            return known.clone();
        }
        let file = self.file(key);
        // Also older than the artist's folder, where a picture of them may have been put.
        let artist_dir_newer = |track: &PathBuf| {
            let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
            let artist_dir = track.parent().and_then(Path::parent);
            matches!((artist_dir.and_then(modified), modified(&file)), (Some(dir), Some(made)) if dir > made)
        };
        let first = source.album_tracks.first();
        let fresh = first.is_none_or(|track| !outdated(&file, track) && !artist_dir_newer(track));
        // Made with its large picture for the artist's page, or as a collage, which has none:
        // pictures made before there were large ones are made again.
        let whole = self.dir.join(format!("{key}-large.jpg")).exists()
            || self.dir.join(format!("{key}.collage")).exists();
        let state = if file.exists() && fresh && whole {
            Thumbnail::Ready(file)
        } else {
            self.send(key, Job::Artist(source.clone()));
            Thumbnail::Pending
        };
        self.known.insert(key.to_owned(), state.clone());
        state
    }

    /// The artist `key`'s picture at up to 1400 pixels, for their page, once made; none when
    /// it is a collage of their covers.
    pub fn artist_portrait(&self, key: &str) -> Option<PathBuf> {
        Some(self.dir.join(format!("{key}-large.jpg"))).filter(|file| file.exists())
    }

    /// The wide picture across the top of the page of the artist `key`. When it is not made
    /// yet, or fanart.tv had none a month ago, it is looked for from `source`.
    pub fn get_backdrop(&mut self, key: &str, source: &BackdropSource) -> Thumbnail {
        let known_key = format!("{key}-backdrop");
        if let Some(known) = self.known.get(&known_key) {
            return known.clone();
        }
        let file = self.file(&known_key);
        let marker = self.dir.join(format!("{key}.nobackdrop"));
        let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
        // A picture put in the artist's folder since changes the folder.
        let artist_dir = source
            .album_tracks
            .first()
            .and_then(|track| track.parent()?.parent());
        let fresh = |made: &Path| match (modified(made), artist_dir.and_then(modified)) {
            (Some(made), Some(dir)) => dir <= made,
            (made, _) => made.is_some(),
        };
        let believed = modified(&marker)
            .and_then(|made| made.elapsed().ok())
            .is_some_and(|age| age < NO_BACKDROP_KEPT);
        let state = if file.exists() && fresh(&file) {
            Thumbnail::Ready(file)
        } else if believed && fresh(&marker) {
            Thumbnail::None
        } else {
            self.send_backdrop(&known_key, source.clone());
            Thumbnail::Pending
        };
        self.known.insert(known_key, state.clone());
        state
    }

    /// Forgets which artists had no wide picture, to look again: after the fanart.tv key
    /// changed.
    pub fn forget_missing_backdrops(&mut self) {
        self.known
            .retain(|key, state| !(key.ends_with("-backdrop") && *state == Thumbnail::None));
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
            self.request(key, track);
            Thumbnail::Pending
        };
        self.known.insert(key.to_owned(), state.clone());
        state
    }

    /// Takes in thumbnails made since the last call. Returns true when there were any.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((key, made)) = self.finished.try_recv() {
            if made {
                // Found as a file on the next `get`.
                self.known.remove(&key);
            } else {
                self.known.insert(key, Thumbnail::None);
            }
            any = true;
        }
        any
    }

    /// Where `key`'s thumbnail is kept, made or not.
    pub fn file_of(&self, key: &str) -> PathBuf {
        self.file(key)
    }

    fn file(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.jpg"))
    }

    fn request(&mut self, key: &str, track: &Path) {
        self.send(key, Job::Album(track.to_owned()));
    }

    fn send_backdrop(&mut self, key: &str, source: BackdropSource) {
        let jobs = self.backdrop_jobs.get_or_insert_with(|| {
            let (jobs, queue) = channel::<(String, BackdropSource)>();
            let dir = self.dir.clone();
            let finished = self.finished_tx.clone();
            let started = std::thread::Builder::new()
                .name("freya-backdrops".into())
                .spawn(move || {
                    let mut asked = None;
                    for (key, source) in queue {
                        let artist = key.trim_end_matches("-backdrop");
                        let made =
                            make_backdrop(&dir, artist, &source, &mut asked).unwrap_or_else(|e| {
                                log::warn!("no wide picture of {:?}: {e}", source.name);
                                false
                            });
                        if finished.send((key, made)).is_err() {
                            break;
                        }
                    }
                });
            if let Err(e) = started {
                log::warn!("could not start looking for wide pictures: {e}");
            }
            jobs
        });
        let _ = jobs.send((key.to_owned(), source));
    }

    fn send(&mut self, key: &str, job: Job) {
        let jobs = self.jobs.get_or_insert_with(|| {
            let (jobs, queue) = channel::<(String, Job)>();
            let dir = self.dir.clone();
            let finished = self.finished_tx.clone();
            let started = std::thread::Builder::new()
                .name("freya-covers".into())
                .spawn(move || {
                    for (key, job) in queue {
                        let made = match &job {
                            Job::Album(track) => make(&dir, &key, track),
                            Job::Artist(source) => make_artist(&dir, &key, source),
                        };
                        let made = match made {
                            Ok(made) => made,
                            Err(e) => {
                                log::debug!("no thumbnail for {key}: {e}");
                                false
                            }
                        };
                        if finished.send((key, made)).is_err() {
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

/// Makes the picture for the artist `key`: their own picture next to their music, else
/// Deezer's when allowed, else their album covers. Returns false when there is none, or when
/// Deezer could not be asked, so the next run asks again.
fn make_artist(dir: &Path, key: &str, source: &ArtistSource) -> Result<bool, String> {
    use crate::artist_image::{self, Online};

    let picture = match artist_image::local(&source.album_tracks) {
        Some(file) => image::open(&file).ok(),
        None => None,
    };
    let picture = match (picture, &source.name) {
        (Some(picture), _) => Some(picture),
        (None, Some(name)) if source.online => {
            let online = artist_image::deezer(name);
            // Deezer allows 50 requests in 5 seconds; this stays well below.
            std::thread::sleep(std::time::Duration::from_millis(150));
            match online? {
                Online::Picture(bytes) => image::load_from_memory(&bytes).ok(),
                Online::NoPicture => None,
            }
        }
        _ => None,
    };
    let collage_marker = dir.join(format!("{key}.collage"));
    let thumbnail = match picture {
        Some(picture) => {
            // Large too, for the top of the artist's page.
            let large = if picture.width() > LARGE || picture.height() > LARGE {
                picture.resize(LARGE, LARGE, image::imageops::FilterType::Triangle)
            } else {
                picture.clone()
            };
            save_jpeg(dir, &format!("{key}-large"), &large.to_rgb8())?;
            let _ = std::fs::remove_file(&collage_marker);
            picture
                .resize_to_fill(SIZE, SIZE, image::imageops::FilterType::Triangle)
                .to_rgb8()
        }
        None => match artist_image::collage(&source.album_tracks, SIZE) {
            Some(collage) => {
                let _ = std::fs::remove_file(dir.join(format!("{key}-large.jpg")));
                create_private_dir(dir).map_err(|e| e.to_string())?;
                std::fs::write(&collage_marker, b"").map_err(|e| e.to_string())?;
                collage
            }
            None => return Ok(false),
        },
    };
    save_jpeg(dir, key, &thumbnail)?;
    Ok(true)
}

/// Makes the wide picture for the artist `key`: from their folder, else from fanart.tv when
/// the user gave a key. When fanart.tv has none, a marker says so, holding the artist's
/// MusicBrainz id, so the next look a month later need not search for it. `asked` is when
/// MusicBrainz was last asked. Returns false when there is no picture.
fn make_backdrop(
    dir: &Path,
    key: &str,
    source: &BackdropSource,
    asked: &mut Option<Instant>,
) -> Result<bool, String> {
    use crate::artist_image::{self, Online};

    let marker = dir.join(format!("{key}.nobackdrop"));
    let picture = if let Some(file) = artist_image::local_backdrop(&source.album_tracks) {
        image::open(&file).map_err(|e| e.to_string())?
    } else {
        let Some(fanart_key) = &source.fanart_key else {
            return Ok(false);
        };
        let found_before = std::fs::read_to_string(&marker)
            .ok()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty());
        let mbid = match (source.mbid.clone().or(found_before), &source.name) {
            (Some(mbid), _) => Some(mbid),
            (None, Some(name)) => {
                if let Some(wait) = asked
                    .and_then(|at| artist_image::MUSICBRAINZ_INTERVAL.checked_sub(at.elapsed()))
                {
                    std::thread::sleep(wait);
                }
                *asked = Some(Instant::now());
                artist_image::musicbrainz_id(name)?
            }
            (None, None) => None,
        };
        let online = match &mbid {
            Some(mbid) => artist_image::fanart_backdrop(mbid, fanart_key)?,
            None => Online::NoPicture,
        };
        match online {
            Online::Picture(bytes) => image::load_from_memory(&bytes).map_err(|e| e.to_string())?,
            Online::NoPicture => {
                create_private_dir(dir).map_err(|e| e.to_string())?;
                std::fs::write(&marker, mbid.unwrap_or_default()).map_err(|e| e.to_string())?;
                return Ok(false);
            }
        }
    };
    let picture = if picture.width() > BACKDROP || picture.height() > BACKDROP {
        picture.resize(BACKDROP, BACKDROP, image::imageops::FilterType::Lanczos3)
    } else {
        picture
    };
    save_jpeg_with(
        dir,
        &format!("{key}-backdrop"),
        &picture.to_rgb8(),
        BACKDROP_QUALITY,
    )?;
    let _ = std::fs::remove_file(&marker);
    Ok(true)
}

/// Writes `picture` as `key`'s thumbnail, under another name first and then renamed, so a
/// half-written file is never shown.
fn save_jpeg(dir: &Path, key: &str, picture: &image::RgbImage) -> Result<(), String> {
    save_jpeg_with(dir, key, picture, JPEG_QUALITY)
}

fn save_jpeg_with(
    dir: &Path,
    key: &str,
    picture: &image::RgbImage,
    quality: u8,
) -> Result<(), String> {
    create_private_dir(dir).map_err(|e| e.to_string())?;
    let partial = dir.join(format!("{key}.partial"));
    let file = std::fs::File::create(&partial).map_err(|e| e.to_string())?;
    image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), quality)
        .encode_image(picture)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&partial, dir.join(format!("{key}.jpg"))).map_err(|e| e.to_string())
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
    save_jpeg(dir, key, &rgb)?;
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
    use std::time::{Duration, Instant};

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("freya-thumbs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wait(thumbnails: &mut Thumbnails) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !thumbnails.poll() {
            assert!(Instant::now() < deadline, "no thumbnail was made");
            std::thread::sleep(Duration::from_millis(10));
        }
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
        let artist = temp_dir("artist-music");
        let album = artist.join("Homesick");
        std::fs::create_dir_all(&album).unwrap();
        image::RgbImage::from_pixel(600, 400, image::Rgb([0, 0, 200]))
            .save(album.join("cover.jpg"))
            .unwrap();
        let track = album.join("01.flac");
        std::fs::write(&track, b"").unwrap();
        let source = ArtistSource {
            name: Some("Shores Of Null".into()),
            album_tracks: vec![track],
            online: false,
        };
        let cache = temp_dir("artist-cache").join("covers");

        // Without a picture of their own, and offline: their album's cover, square.
        let mut thumbnails = Thumbnails::new(cache.clone());
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

        // A picture in their folder, added later, is taken next run.
        std::thread::sleep(Duration::from_millis(20));
        image::RgbImage::from_pixel(300, 300, image::Rgb([220, 0, 0]))
            .save(artist.join("artist.jpg"))
            .unwrap();
        let mut later = Thumbnails::new(cache);
        assert_eq!(later.get_artist(&key, &source), Thumbnail::Pending);
        wait(&mut later);
        let Thumbnail::Ready(file) = later.get_artist(&key, &source) else {
            panic!("made from the artist's picture");
        };
        assert!(image::open(&file).unwrap().to_rgb8().get_pixel(150, 150)[0] > 150);
        // And large, for their page; a collage has none.
        assert!(later.artist_portrait(&key).is_some());
    }

    #[test]
    fn makes_wide_pictures_from_the_artists_folder_and_believes_fanart_tv_for_a_month() {
        let artist = temp_dir("backdrop-music");
        let album = artist.join("Dead Magic");
        std::fs::create_dir_all(&album).unwrap();
        let track = album.join("01.flac");
        std::fs::write(&track, b"").unwrap();
        let mut source = BackdropSource {
            name: Some("Anna von Hausswolff".into()),
            album_tracks: vec![track],
            mbid: None,
            fanart_key: None,
        };
        let cache = temp_dir("backdrop-cache").join("covers");
        let key = Thumbnails::artist_key(source.name.as_deref());

        // None in their folder, and no key for fanart.tv.
        let mut thumbnails = Thumbnails::new(cache.clone());
        assert_eq!(thumbnails.get_backdrop(&key, &source), Thumbnail::Pending);
        wait(&mut thumbnails);
        assert_eq!(thumbnails.get_backdrop(&key, &source), Thumbnail::None);

        // fanart.tv had none a while ago: it is not asked again yet.
        source.fanart_key = Some("key".into());
        std::thread::sleep(Duration::from_millis(20));
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join(format!("{key}.nobackdrop")), "mbid").unwrap();
        thumbnails.forget_missing_backdrops();
        assert_eq!(thumbnails.get_backdrop(&key, &source), Thumbnail::None);

        // One put in their folder is taken at full size, up to 4K.
        std::thread::sleep(Duration::from_millis(20));
        image::RgbImage::from_pixel(4000, 2000, image::Rgb([0, 180, 0]))
            .save(artist.join("fanart.jpg"))
            .unwrap();
        let mut later = Thumbnails::new(cache.clone());
        assert_eq!(later.get_backdrop(&key, &source), Thumbnail::Pending);
        wait(&mut later);
        let Thumbnail::Ready(file) = later.get_backdrop(&key, &source) else {
            panic!("made from fanart.jpg");
        };
        let made = image::open(&file).unwrap();
        assert_eq!((made.width(), made.height()), (BACKDROP, 1920));
        assert!(!cache.join(format!("{key}.nobackdrop")).exists());
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

        let cache = temp_dir("cache").join("covers");
        let mut thumbnails = Thumbnails::new(cache.clone());
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
        let mut again = Thumbnails::new(cache.clone());
        assert_eq!(again.get("a", &track), Thumbnail::Ready(file.clone()));
        assert_eq!(again.get("b", &bare), Thumbnail::Pending);

        // A cover replaced after the thumbnail was made: the next run makes it again. It kept
        // the old one for good.
        std::thread::sleep(Duration::from_millis(20));
        let image = image::RgbImage::from_pixel(600, 300, image::Rgb([0, 0, 255]));
        image.save(music.join("cover.jpg")).unwrap();
        let mut later = Thumbnails::new(cache);
        assert_eq!(later.get("a", &track), Thumbnail::Pending);
        wait(&mut later);
        let Thumbnail::Ready(file) = later.get("a", &track) else {
            panic!("the thumbnail is made again");
        };
        let made = image::open(&file).unwrap();
        assert_eq!((made.width(), made.height()), (SIZE, SIZE / 2));
    }
}
