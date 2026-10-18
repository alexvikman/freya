//! The library as the skin shows it: browsing by artist, album, song and genre, what was added
//! and played lately, searching, and the scans that keep it up to date.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use freya_library::cue::Part;
use freya_library::library::{
    Album, Artist, ArtistEdit, ArtistOrder, Genre, PictureKind, Played, ScanEvent, Scanner, Track,
};
use freya_library::pictures::Pictures;
use freya_library::thumbnails::{
    ArtistSource, BackdropSource, Downloaded, Fetched, Thumbnail, Thumbnails,
};
use freya_library::{Entry, Library};
use slint::{Image, ModelRc, VecModel};
use slint_interpreter::{Struct, Value};

use super::artist_info::ArtistInfos;
use super::skin_state::{ModelRows, format_time, string};
use super::skin_window::SkinWindow;

/// Search results are cut off here; nobody scrolls through more, and building rows for the
/// whole library on every keystroke would be wasted work.
const SEARCH_LIMIT: usize = 1_000;
/// While a scan runs, the rows are reloaded at most this often.
const RELOAD_INTERVAL: Duration = Duration::from_secs(1);
/// Decoded covers kept for scrolling back: about 360 KB each, 46 MB in all. More than a grid
/// shows at once full screen on a 4K screen (about 90): with fewer, the covers on screen pushed
/// each other out and were decoded again whenever the grid was drawn.
const RECENT_COVERS: usize = 128;
/// How many albums or tracks the Recents sections show.
const RECENT: usize = 200;

/// A list the library can show. The first seven start a section; the others are opened from a
/// row.
#[derive(Clone, Debug, PartialEq)]
enum Level {
    Artists,
    Albums,
    Songs,
    Genres,
    RecentlyAdded,
    RecentlyPlayed,
    MostPlayed,
    ArtistAlbums {
        artist: Option<String>,
    },
    GenreAlbums {
        genre: Option<String>,
    },
    AlbumTracks {
        artist: Option<String>,
        album: Option<String>,
    },
}

impl Level {
    /// The list a section starts with, by the section's name in the skin API.
    fn section(name: &str) -> Option<Self> {
        Some(match name {
            "artists" => Self::Artists,
            "albums" => Self::Albums,
            "songs" => Self::Songs,
            "genres" => Self::Genres,
            "recently-added" => Self::RecentlyAdded,
            "recently-played" => Self::RecentlyPlayed,
            "most-played" => Self::MostPlayed,
            _ => return None,
        })
    }

    fn section_name(&self) -> &'static str {
        match self {
            Self::Albums => "albums",
            Self::Songs => "songs",
            Self::Genres => "genres",
            Self::RecentlyAdded => "recently-added",
            Self::RecentlyPlayed => "recently-played",
            Self::MostPlayed => "most-played",
            _ => "artists",
        }
    }

    /// How the skin best shows this list: see `Library.layout`.
    fn layout(&self) -> &'static str {
        match self {
            Self::Albums
            | Self::RecentlyAdded
            | Self::ArtistAlbums { .. }
            | Self::GenreAlbums { .. } => "grid",
            Self::Songs | Self::RecentlyPlayed | Self::MostPlayed => "table",
            Self::Artists | Self::Genres | Self::AlbumTracks { .. } => "list",
        }
    }
}

/// What of the library is open: the lists opened from a section, and a search; a place to go
/// back to.
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryPlace {
    opened: Vec<Level>,
    query: String,
}

impl LibraryPlace {
    /// Whether `next` is this search typed on: the same lists, searching still.
    pub fn continued_by(&self, next: &Self) -> bool {
        !self.query.trim().is_empty() && !next.query.trim().is_empty() && self.opened == next.opened
    }
}

/// How an artist's albums are ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlbumSort {
    /// Newest first; albums without a year last.
    ReleaseDate,
    Name,
}

impl AlbumSort {
    /// By its name in the skin API and settings: "release-date" or "name".
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "release-date" => Some(Self::ReleaseDate),
            "name" => Some(Self::Name),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::ReleaseDate => "release-date",
            Self::Name => "name",
        }
    }

    fn sort(self, albums: &mut [Album]) {
        let name = |a: &Album| a.name.as_deref().map(str::to_lowercase);
        match self {
            // `None` sorts first, so the newest-first order puts albums without a year last.
            Self::ReleaseDate => {
                albums.sort_by(|a, b| b.year.cmp(&a.year).then_with(|| name(a).cmp(&name(b))))
            }
            Self::Name => albums.sort_by_key(|a| (a.name.is_none(), name(a))),
        }
    }
}

/// What one row stands for.
enum Item {
    Artist(Artist),
    Album(Album),
    Genre(Genre),
    Track(Track),
    /// A track in Recently Played or Most Played.
    Played(Played),
}

impl Item {
    fn track(&self) -> Option<&Track> {
        match self {
            Self::Track(track) | Self::Played(Played { track, .. }) => Some(track),
            _ => None,
        }
    }
}

pub struct LibraryPanel {
    /// Where the database is kept.
    path: Option<PathBuf>,
    /// Opened when a skin first shows the library.
    db: Option<Library>,
    scanner: Scanner,
    /// The lists opened, the section's own list first; `back` returns to the one before.
    opened: Vec<Level>,
    /// Search text; when not empty, search results are shown instead of `level`.
    query: String,
    items: Vec<Item>,
    rows: ModelRows,
    title: String,
    subtitle: String,
    /// Totals of the whole library, counted again only when its tracks may have changed: not
    /// for every keystroke of a search.
    stats: Option<String>,
    progress: Option<(usize, usize)>,
    /// Rows need reloading, e.g. because a scan wrote more tracks.
    stale: bool,
    reloaded: Instant,
    /// Folders last given to the skin, in use and being edited.
    folders_shown: Option<Vec<PathBuf>>,
    draft_shown: Option<Vec<PathBuf>>,
    /// Album covers for grids; `None` without a cache folder.
    thumbnails: Option<Thumbnails>,
    /// For each album shown, by its cover's key, a track to take the cover from.
    cover_tracks: HashMap<String, PathBuf>,
    recent_covers: RecentCovers,
    /// What the internet said of artists, until the library is free to keep it: written
    /// while a scan writes, it would hold up the window.
    fetched: Vec<Fetched>,
    /// `Library.covers-ready`: goes up as covers are made.
    covers_ready: i32,
    /// Plays not yet written: a scan may be writing to the database.
    plays: Vec<(PathBuf, Option<Part>, SystemTime)>,
    /// How an artist's albums are ordered, and whether they are shown as a list of albums
    /// with their tracks rather than a grid of covers.
    album_sort: AlbumSort,
    album_list: bool,
    /// A scan was started and its end not yet taken in.
    scanning: bool,
    /// The folder a scan looks through or reads now.
    scan_folder: Option<PathBuf>,
    /// The track playing, by its file and where in it it starts, and its row, or -1.
    playing: Option<(PathBuf, Duration)>,
    playing_row: i32,
    /// The album artist and album of the track playing, when it is in the library.
    playing_album: Option<(Option<String>, Option<String>)>,
    /// A row for the skin to put the keyboard on and scroll to, each time `reveals` counts
    /// up; -1 to show the list from the top.
    reveal_row: i32,
    reveals: i32,
    /// How artists are ordered, whether they show as a list rather than a grid of pictures,
    /// and whether pictures of them may come from Deezer.
    artist_order: ArtistOrder,
    artist_list: bool,
    online_pictures: bool,
    /// The artists shown, by their picture's key.
    artist_keys: HashMap<String, Option<String>>,
    /// While an artist's albums are shown, the page over them; and what Last.fm says of
    /// artists, if it may be asked.
    artist_page: Option<ArtistPage>,
    artist_infos: ArtistInfos,
    online_bios: bool,
    /// What the user changed about artists, by the tags' name for them.
    edits: HashMap<String, ArtistEdit>,
    /// The user's personal fanart.tv key, for wide pictures of artists.
    fanart_key: Option<String>,
    popular_rows: ModelRows,
    /// While an album's tracks are shown: how often each was played, and its page.
    play_counts: HashMap<(PathBuf, Option<Duration>), u32>,
    album_page: Option<AlbumPage>,
}

/// What an artist's own page shows over their albums.
struct ArtistPage {
    name: Option<String>,
    /// The name shown: the one the user gave them, else the tags'.
    shown_name: String,
    /// Whether the biography is the user's own, rather than Last.fm's.
    own_biography: bool,
    detail: String,
    genres: String,
    /// Their picture's key; a wide picture of them and their picture large, once made; and a
    /// color from the one shown.
    picture: String,
    banner: Option<Image>,
    portrait: Option<Image>,
    /// Whether `portrait` is made from their picture large, rather than the grid's.
    portrait_large: bool,
    /// Their picture large, uncut.
    picture_full: Option<Image>,
    color: Option<[u8; 3]>,
    /// A track of each of their albums, which their pictures can be made from.
    album_tracks: Vec<PathBuf>,
    summary: String,
    biography: String,
    tags: String,
    /// Their most played tracks, and whether all ten show rather than five.
    popular: Vec<Played>,
    more: bool,
}

/// A folder as the scan shows it: its parent and its own name, "Peter Gabriel / So", which is
/// mostly the artist and the album.
fn folder_name(folder: &Path) -> String {
    let mut names: Vec<String> = folder
        .components()
        .rev()
        .filter_map(|c| match c {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .take(2)
        .collect();
    names.reverse();
    names.join(" / ")
}

/// The size of an artist's round picture: sharp at 232 points on a Retina screen.
const ROUND_PICTURE: u32 = 480;

/// The picture in `file`, square and cut round with a soft edge, for a skin to show as is:
/// Slint's software renderer clips to rectangles only.
fn round_picture(file: &Path) -> Option<Image> {
    let square = image::open(file)
        .ok()?
        .resize_to_fill(
            ROUND_PICTURE,
            ROUND_PICTURE,
            image::imageops::FilterType::Lanczos3,
        )
        .to_rgba8();
    let mut pixels =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(ROUND_PICTURE, ROUND_PICTURE);
    let radius = ROUND_PICTURE as f32 / 2.0;
    for ((x, y, from), to) in square.enumerate_pixels().zip(pixels.make_mut_slice()) {
        let (dx, dy) = (x as f32 + 0.5 - radius, y as f32 + 0.5 - radius);
        // Partly covered at the edge, by how far the circle reaches into the pixel.
        let cover = (radius - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
        // Premultiplied, as Slint draws it.
        let [r, g, b, a] = from.0;
        let alpha = f32::from(a) / 255.0 * cover;
        let channel = |c: u8| (f32::from(c) * alpha).round() as u8;
        *to = slint::Rgba8Pixel::new(
            channel(r),
            channel(g),
            channel(b),
            (alpha * 255.0).round() as u8,
        );
    }
    Some(Image::from_rgba8_premultiplied(pixels))
}

/// Popular tracks shown on an artist's page, or all of them with "more".
const POPULAR: usize = 5;
const POPULAR_MORE: usize = 10;

/// What an album's own page shows over its tracks.
#[derive(Clone, Debug, PartialEq)]
struct AlbumPage {
    title: String,
    artist: String,
    detail: String,
    cover: String,
    /// From the cover, once its thumbnail is made.
    color: Option<[u8; 3]>,
}

/// The covers shown last, so scrolling back does not decode them again.
#[derive(Default)]
struct RecentCovers {
    /// Least recently shown first.
    order: VecDeque<String>,
    images: HashMap<String, Image>,
}

impl RecentCovers {
    fn get(&mut self, key: &str) -> Option<Image> {
        let image = self.images.get(key)?.clone();
        self.order.retain(|k| k != key);
        self.order.push_back(key.to_owned());
        Some(image)
    }

    fn put(&mut self, key: &str, image: Image) {
        if self.order.len() >= RECENT_COVERS
            && let Some(oldest) = self.order.pop_front()
        {
            self.images.remove(&oldest);
        }
        self.order.push_back(key.to_owned());
        self.images.insert(key.to_owned(), image);
    }
}

impl LibraryPanel {
    /// A library kept at `path`, with album covers made in `covers` and the pictures of
    /// artists downloaded kept in `pictures`.
    pub fn new(path: Option<PathBuf>, covers: Option<PathBuf>, pictures: Option<PathBuf>) -> Self {
        Self {
            path,
            db: None,
            scanner: Scanner::new(),
            opened: vec![Level::Artists],
            query: String::new(),
            items: Vec::new(),
            rows: ModelRows::new(),
            title: String::new(),
            subtitle: String::new(),
            stats: None,
            progress: None,
            stale: false,
            reloaded: Instant::now(),
            folders_shown: None,
            draft_shown: None,
            thumbnails: covers
                .zip(pictures)
                .map(|(covers, pictures)| Thumbnails::new(covers, Pictures::new(pictures))),
            fetched: Vec::new(),
            cover_tracks: HashMap::new(),
            recent_covers: RecentCovers::default(),
            covers_ready: 0,
            plays: Vec::new(),
            scanning: false,
            scan_folder: None,
            album_sort: AlbumSort::ReleaseDate,
            album_list: false,
            playing: None,
            playing_row: -1,
            playing_album: None,
            reveal_row: -1,
            reveals: 0,
            play_counts: HashMap::new(),
            album_page: None,
            artist_order: ArtistOrder::Name,
            artist_list: false,
            online_pictures: true,
            artist_keys: HashMap::new(),
            artist_page: None,
            artist_infos: ArtistInfos::new(),
            edits: HashMap::new(),
            online_bios: true,
            fanart_key: None,
            popular_rows: ModelRows::new(),
        }
    }

    /// Shows the library in a skin window that has the `Library` global. The first time,
    /// opens the library and scans `folders` for changes.
    /// Opens the library for `ui`, the first time also bringing it in line with `folders`,
    /// unless `scan` is false: when the settings were damaged, a missing folder list would
    /// otherwise remove every track.
    pub fn attach(&mut self, ui: &SkinWindow, folders: &[PathBuf], scan: bool) {
        let _ = ui.set_global_property("Library", "rows", self.rows.model_value());
        let _ = ui.set_global_property("Library", "popular", self.popular_rows.model_value());
        self.folders_shown = None;
        self.draft_shown = None;
        if self.db.is_some() {
            return;
        }
        let Some(path) = &self.path else {
            return;
        };
        match Library::open(path) {
            Ok(db) => self.db = Some(db),
            Err(e) => {
                log::warn!("could not open the library at {}: {e}", path.display());
                return;
            }
        }
        self.find_playing_album();
        self.clear_older_cache();
        self.reload();
        if scan {
            self.rescan(folders);
        } else {
            log::warn!("the library is not checked against its folders: the settings were damaged");
        }
    }

    /// Takes away what an older Freya kept in the cache and keeps elsewhere now: pictures it
    /// downloaded, mixed with the thumbnails, and what Last.fm said, as files beside them.
    fn clear_older_cache(&self) {
        let Some(thumbnails) = &self.thumbnails else {
            return;
        };
        match thumbnails.clear_older_layout() {
            Ok(0) => {}
            Ok(n) => log::info!("cleared {n} files of an older layout from the cache"),
            Err(e) => log::warn!("could not clear the older cache: {e}"),
        }
        let Some(artists) = self
            .covers_dir()
            .and_then(Path::parent)
            .map(|c| c.join("artists"))
        else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&artists) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|e| e == "json" || e == "partial")
            {
                let _ = std::fs::remove_file(path);
            }
        }
        // Gone when empty.
        let _ = std::fs::remove_dir(&artists);
    }

    fn covers_dir(&self) -> Option<&Path> {
        self.thumbnails.as_ref().map(Thumbnails::dir)
    }

    /// Until the scan's end has been taken in by `poll`, not only while its thread runs: a
    /// scan that ends between two polls has news that is not shown yet.
    pub fn is_scanning(&self) -> bool {
        self.scanning
    }

    pub fn rescan(&mut self, folders: &[PathBuf]) {
        if let (Some(path), Some(_)) = (&self.path, &self.db) {
            self.scanner.start(path.clone(), folders.to_vec());
            self.scanning = true;
        }
    }

    /// Handles scan progress and covers made. Returns true when the rows changed.
    pub fn poll(&mut self) -> bool {
        if let Some(thumbnails) = &mut self.thumbnails
            && thumbnails.poll()
        {
            self.fetched.extend(thumbnails.take_fetched());
            if !self.scanning {
                self.write_fetched();
            }
            self.covers_ready += 1;
            self.color_album_page();
            self.show_artist_picture();
        }
        if self.artist_infos.poll() {
            if !self.scanning {
                self.write_fetched();
            }
            self.show_artist_info();
        }
        // Asked before the events are read: a scan that had ended by then has sent them all.
        let ended = !self.scanner.is_running();
        let mut finished = false;
        for event in self.scanner.poll() {
            match event {
                ScanEvent::Progress { done, total } => {
                    self.progress = Some((done, total));
                    self.stale = true;
                }
                ScanEvent::Looking(folder) => self.scan_folder = Some(folder),
                ScanEvent::Finished(summary) => {
                    self.progress = None;
                    self.scanning = false;
                    // A scan that found nothing new leaves the rows as they are.
                    self.stale |= summary.added + summary.updated + summary.removed > 0;
                    finished = true;
                }
                ScanEvent::Failed(e) => {
                    log::warn!("library scan failed: {e}");
                    self.progress = None;
                    self.scanning = false;
                    finished = true;
                }
            }
        }
        // A scan whose thread ended without saying so is over all the same.
        if ended && self.scanning {
            self.scanning = false;
            finished = true;
        }
        if !self.scanning {
            self.scan_folder = None;
        }
        // Plays noted while the scan wrote.
        if finished && self.write_plays() && self.shows_plays() {
            self.stale = true;
        }
        if finished {
            self.write_fetched();
            self.keep_named_pictures();
        }
        if self.stale && (finished || self.reloaded.elapsed() >= RELOAD_INTERVAL) {
            self.reload();
            return true;
        }
        finished
    }

    // ----- History -----------------------------------------------------------------------------

    /// Notes that a track was played just now.
    pub fn record_play(&mut self, path: PathBuf, part: Option<Part>) {
        if self.path.is_none() {
            return;
        }
        self.plays.push((path, part, SystemTime::now()));
        // While a scan writes, the play waits rather than the window.
        if !self.scanning && self.write_plays() && self.shows_plays() {
            self.reload();
        }
    }

    /// Whether the rows shown are made from plays.
    fn shows_plays(&self) -> bool {
        !self.searching() && matches!(self.level(), Level::RecentlyPlayed | Level::MostPlayed)
    }

    /// Writes the plays noted, waiting for a scan if one is running. For when Freya quits.
    pub fn save(&mut self) {
        self.write_plays();
        self.write_fetched();
    }

    /// Keeps what the internet said of artists.
    fn write_fetched(&mut self) {
        let Some(db) = &self.db else {
            return;
        };
        self.artist_infos.save(db);
        let now = SystemTime::now();
        for fetched in self.fetched.drain(..) {
            let written = match &fetched {
                Fetched::Picture {
                    artist,
                    kind,
                    source,
                    picture,
                } => db.set_artist_picture(artist, *kind, source, picture.as_deref(), now),
                Fetched::ArtistId { artist, mbid } => {
                    db.set_searched_artist_id(artist, mbid.as_deref(), now)
                }
            };
            if let Err(e) = written {
                log::warn!("could not keep {fetched:?}: {e}");
            }
        }
    }

    /// Takes away the downloaded pictures no artist has any longer.
    fn keep_named_pictures(&self) {
        let (Some(db), Some(thumbnails)) = (&self.db, &self.thumbnails) else {
            return;
        };
        if !self.fetched.is_empty() {
            return;
        }
        let kept = db
            .artist_picture_checksums()
            .map_err(|e| e.to_string())
            .and_then(|named| {
                thumbnails
                    .pictures()
                    .keep_only(&named)
                    .map_err(|e| e.to_string())
            });
        match kept {
            Ok(0) => {}
            Ok(n) => log::info!("took away {n} downloaded pictures no artist has"),
            Err(e) => log::warn!("could not tidy the downloaded pictures: {e}"),
        }
    }

    /// What the library knows of the artist's picture from the internet: noted just now, or
    /// kept.
    fn downloaded(&self, artist: Option<&str>, kind: PictureKind) -> Downloaded {
        let Some(artist) = artist else {
            return Downloaded::Missing;
        };
        let noted = self.fetched.iter().rev().find_map(|fetched| match fetched {
            Fetched::Picture {
                artist: a,
                kind: k,
                picture,
                ..
            } if a == artist && *k == kind => Some(picture.clone()),
            _ => None,
        });
        let kept = || {
            let asked = self.db.as_ref()?.artist_picture(artist, kind).ok()??;
            match asked.answer {
                Some(checksum) => Some(Some(checksum)),
                None if asked.recent() => Some(None),
                None => None,
            }
        };
        match noted.or_else(kept) {
            Some(Some(checksum)) => Downloaded::Kept(checksum),
            Some(None) => Downloaded::Missing,
            None => Downloaded::Unknown,
        }
    }

    /// Where the wide picture of the artist on the page can come from, as the library knows it
    /// now.
    fn backdrop_source(&self, page: &ArtistPage) -> BackdropSource {
        let name = page.name.as_deref();
        // The user's, else the tags'.
        let tagged = name
            .and_then(|n| self.edits.get(n)?.mbid.clone())
            .or_else(|| {
                self.db
                    .as_ref()
                    .and_then(|db| db.artist_mbid(name).ok().flatten())
            });
        let noted = self.fetched.iter().rev().find_map(|fetched| match fetched {
            Fetched::ArtistId { artist, mbid } if Some(artist.as_str()) == name => {
                Some(mbid.clone())
            }
            _ => None,
        });
        let searched = noted.map(|mbid| (mbid, true)).or_else(|| {
            let asked = self.db.as_ref()?.searched_artist_id(name?).ok()??;
            let recent = asked.recent();
            Some((asked.answer, recent))
        });
        let (mbid, search) = match (tagged, searched) {
            (Some(mbid), _) => (Some(mbid), false),
            (None, Some((Some(mbid), _))) => (Some(mbid), false),
            // MusicBrainz found none lately: not searched again yet.
            (None, Some((None, recent))) => (None, !recent),
            (None, None) => (None, true),
        };
        BackdropSource {
            name: page.name.clone(),
            album_tracks: page.album_tracks.clone(),
            mbid,
            search,
            fanart_key: self.fanart_key.clone(),
            fanart: self.downloaded(name, PictureKind::Backdrop),
        }
    }

    /// Writes the plays noted. Returns true when any were written.
    fn write_plays(&mut self) -> bool {
        let Some(db) = &self.db else {
            return false;
        };
        let mut written = false;
        while let Some((path, part, at)) = self.plays.first() {
            if let Err(e) = db.record_play(path, *part, *at) {
                log::warn!("could not note a play of {}: {e}", path.display());
                break;
            }
            self.plays.remove(0);
            written = true;
        }
        written
    }

    // ----- Navigation --------------------------------------------------------------------------

    pub fn search(&mut self, text: &str) {
        if text.trim() == self.query.trim() {
            self.query = text.to_owned();
            return;
        }
        self.query = text.to_owned();
        self.reload();
    }

    /// Starts browsing a section, from its own list.
    pub fn show(&mut self, section: &str) {
        if let Some(level) = Level::section(section) {
            self.opened = vec![level];
            self.query.clear();
            self.reload();
        }
    }

    /// Opens an artist, album or genre. For a track, returns the tracks shown, to play from
    /// that one.
    pub fn activate(&mut self, row: usize) -> Option<(Vec<Track>, usize)> {
        let level = match self.items.get(row)? {
            Item::Artist(artist) => Level::ArtistAlbums {
                artist: artist.name.clone(),
            },
            Item::Album(album) => Level::AlbumTracks {
                artist: album.artist.clone(),
                album: album.name.clone(),
            },
            Item::Genre(genre) => Level::GenreAlbums {
                genre: genre.name.clone(),
            },
            // The tracks shown, or in a discography the album's, playing from this one.
            Item::Track(_) | Item::Played(_) => {
                let section = self.section_of(row);
                let tracks: Vec<Track> = self.items[section.clone()]
                    .iter()
                    .filter_map(Item::track)
                    .cloned()
                    .collect();
                let start = self.items[section.start..row]
                    .iter()
                    .filter(|item| item.track().is_some())
                    .count();
                return Some((tracks, start));
            }
        };
        self.opened.push(level);
        self.query.clear();
        self.reload();
        None
    }

    /// The track playing or paused, by its file and where in the file it starts (a CUE
    /// sheet's tracks share one), to mark its row.
    pub fn set_playing(&mut self, track: Option<(&Path, Duration)>) {
        let track = track.map(|(path, start)| (path.to_owned(), start));
        if self.playing != track {
            self.playing = track;
            self.find_playing_row();
            self.find_playing_album();
        }
    }

    fn find_playing_album(&mut self) {
        self.playing_album = match (&self.db, &self.playing) {
            (Some(db), Some((path, start))) => db
                .track_at(path, *start)
                .ok()
                .flatten()
                .map(|track| (track.album_artist, track.album)),
            _ => None,
        };
    }

    /// Opens the artist of the track playing, or its album with its row marked. Returns
    /// false when the track is not in the library.
    pub fn open_playing(&mut self, album: bool) -> bool {
        let Some((artist, name)) = self.playing_album.clone() else {
            return false;
        };
        self.opened = vec![
            Level::Artists,
            Level::ArtistAlbums {
                artist: artist.clone(),
            },
        ];
        if album {
            self.opened.push(Level::AlbumTracks {
                artist,
                album: name,
            });
        }
        self.query.clear();
        self.reload();
        self.reveal_row = if album { self.playing_row } else { -1 };
        self.reveals += 1;
        true
    }

    fn find_playing_row(&mut self) {
        let Some((path, start)) = &self.playing else {
            self.playing_row = -1;
            return;
        };
        self.playing_row = self
            .items
            .iter()
            .position(|item| {
                item.track().is_some_and(|t| {
                    t.path == *path && t.part.map_or(Duration::ZERO, |p| p.start) == *start
                })
            })
            .map_or(-1, |row| row as i32);
    }

    /// The page of the album shown, and how often its tracks were played; for anything else,
    /// none.
    fn load_album_page(&mut self) {
        self.play_counts.clear();
        self.album_page = None;
        let (Some(db), Level::AlbumTracks { artist, album }) = (&self.db, self.level().clone())
        else {
            return;
        };
        if self.searching() {
            return;
        }
        self.play_counts = db
            .album_plays(artist.as_deref(), album.as_deref())
            .unwrap_or_else(|e| {
                log::warn!("library: {e}");
                HashMap::new()
            });
        let tracks: Vec<&Track> = self.items.iter().filter_map(Item::track).collect();
        let Some(first) = tracks.first() else {
            return;
        };
        let cover_album = Album {
            artist: artist.clone(),
            name: album.clone(),
            year: None,
            tracks: 0,
            track: first.path.clone(),
        };
        let cover = cover_key(&cover_album);
        self.cover_tracks.insert(cover.clone(), first.path.clone());
        let mut detail = Vec::new();
        if let Some(year) = tracks.iter().filter_map(|t| t.year).min() {
            detail.push(year.to_string());
        }
        let total: Duration = tracks.iter().filter_map(|t| t.duration).sum();
        let mut size = count(tracks.len(), "track");
        if !total.is_zero() {
            size = format!("{size}, {}", long_time(total));
        }
        detail.push(size);
        self.album_page = Some(AlbumPage {
            title: album_name(album.as_deref()),
            artist: self.shown_artist(artist.as_deref()),
            detail: detail.join(" · "),
            cover,
            color: None,
        });
        self.color_album_page();
    }

    /// The page of the artist whose albums are shown; for anything else, none.
    fn load_artist_page(&mut self) {
        let more = self.artist_page.as_ref().is_some_and(|p| p.more);
        self.artist_page = None;
        let (Some(db), Level::ArtistAlbums { artist }) = (&self.db, self.level().clone()) else {
            self.popular_rows.sync(Vec::new());
            return;
        };
        if self.searching() {
            self.popular_rows.sync(Vec::new());
            return;
        }
        let summary = db.artist_summary(artist.as_deref(), 3).ok();
        let popular = db
            .artist_popular(artist.as_deref(), POPULAR_MORE)
            .unwrap_or_default();
        let mut detail = Vec::new();
        if let Some(s) = &summary {
            detail.push(count(s.albums as usize, "album"));
            detail.push(count(s.tracks as usize, "track"));
            if !s.duration.is_zero() {
                detail.push(long_time(s.duration));
            }
        }
        let album_tracks = db
            .albums(artist.as_deref())
            .map(|albums| albums.into_iter().map(|a| a.track).collect())
            .unwrap_or_default();
        let picture = Thumbnails::artist_key(artist.as_deref());
        self.artist_keys.insert(picture.clone(), artist.clone());
        self.artist_page = Some(ArtistPage {
            shown_name: self.shown_artist(artist.as_deref()),
            own_biography: false,
            name: artist,
            detail: detail.join(" · "),
            genres: summary.map(|s| s.genres.join(", ")).unwrap_or_default(),
            picture,
            banner: None,
            portrait: None,
            portrait_large: false,
            picture_full: None,
            color: None,
            album_tracks,
            summary: String::new(),
            biography: String::new(),
            tags: String::new(),
            popular,
            more,
        });
        self.show_artist_picture();
        self.show_artist_info();
        self.show_popular();
    }

    /// The artist page's wide picture, or else their picture large, and a color from the one
    /// shown, once made; asking for them to be made the first time.
    fn show_artist_picture(&mut self) {
        let Some(page) = &self.artist_page else {
            return;
        };
        let key = page.picture.clone();
        let needs_banner = page.banner.is_none();
        let needs_portrait = !page.portrait_large;
        let backdrop = self.backdrop_source(page);
        // Made, or asked to be made, as for the grid.
        self.cover(&key);
        let Some(thumbnails) = &mut self.thumbnails else {
            return;
        };
        let color_of = |file: &Path| {
            image::open(file)
                .ok()
                .map(|i| freya_library::cover::page_color(i.thumbnail(64, 64).to_rgb8().as_raw()))
        };
        let banner = match needs_banner.then(|| thumbnails.get_backdrop(&key, &backdrop)) {
            Some(Thumbnail::Ready(file)) => Image::load_from_path(&file)
                .ok()
                .map(|image| (image, color_of(&file))),
            _ => None,
        };
        let portrait_file = thumbnails.artist_portrait(&key);
        // Their picture large, else the grid's while it is made, or of a collage.
        let portrait = match (&portrait_file, needs_portrait) {
            (Some(file), true) => round_picture(file).map(|image| (image, true)),
            (None, true) => thumbnails
                .ready(&key)
                .and_then(round_picture)
                .map(|image| (image, false)),
            _ => None,
        };
        let Some(page) = &mut self.artist_page else {
            return;
        };
        if let Some((image, color)) = banner {
            page.banner = Some(image);
            page.color = color.or(page.color);
        }
        if let Some((image, large)) = portrait {
            page.portrait = Some(image);
            page.portrait_large = large;
            page.picture_full = portrait_file
                .as_deref()
                .filter(|_| large)
                .and_then(|file| Image::load_from_path(file).ok());
        }
        if page.color.is_none() {
            // Without a wide picture: the color of their picture, or of what the grid shows.
            page.color = match portrait_file {
                Some(file) => color_of(&file),
                None => thumbnails.ready(&key).and_then(color_of),
            };
        }
    }

    /// What Last.fm says of the artist, once it has; asking it the first time.
    fn show_artist_info(&mut self) {
        let Some(page) = &mut self.artist_page else {
            return;
        };
        let Some(name) = page.name.as_deref() else {
            return;
        };
        // The user's own replaces Last.fm's, which is then not asked.
        let own = self.edits.get(name).and_then(|e| e.biography.clone());
        page.own_biography = own.is_some();
        if let Some(own) = &own {
            page.summary = own.split("\n\n").next().unwrap_or(own).trim().to_owned();
            page.biography.clone_from(own);
            page.tags.clear();
            return;
        }
        if let Some(info) = self
            .artist_infos
            .get(name, self.online_bios, self.db.as_ref())
        {
            page.summary.clone_from(&info.summary);
            page.biography.clone_from(&info.biography);
            page.tags = info
                .tags
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
        }
    }

    fn show_popular(&mut self) {
        let rows = match &self.artist_page {
            Some(page) => {
                let shown = if page.more { POPULAR_MORE } else { POPULAR };
                page.popular
                    .iter()
                    .take(shown)
                    .enumerate()
                    .map(|(i, played)| {
                        let item = Item::Track(played.track.clone());
                        let mut row = row_value(&item, None, |_| String::new(), Some(played.plays));
                        if let Value::Struct(s) = &mut row {
                            s.set_field("number".into(), string((i + 1).to_string()));
                            let album = Album {
                                artist: played.track.album_artist.clone(),
                                name: played.track.album.clone(),
                                year: None,
                                tracks: 0,
                                track: played.track.path.clone(),
                            };
                            let key = cover_key(&album);
                            self.cover_tracks
                                .insert(key.clone(), played.track.path.clone());
                            s.set_field("cover".into(), string(key));
                        }
                        row
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        self.popular_rows.sync(rows);
    }

    /// Plays the artist's popular tracks shown, from the `index`th.
    pub fn popular_tracks(&self) -> Vec<Track> {
        self.artist_page.as_ref().map_or_else(Vec::new, |page| {
            let shown = if page.more { POPULAR_MORE } else { POPULAR };
            page.popular
                .iter()
                .take(shown)
                .map(|p| p.track.clone())
                .collect()
        })
    }

    /// Shows all ten popular tracks, or five again.
    pub fn toggle_more_popular(&mut self) {
        if let Some(page) = &mut self.artist_page {
            page.more = !page.more;
        }
        self.show_popular();
    }

    /// The user's personal fanart.tv key, or none: a new one looks again for wide pictures
    /// it had none of.
    pub fn set_fanart_key(&mut self, key: Option<String>) {
        if self.fanart_key != key {
            self.fanart_key = key;
            if let Some(thumbnails) = &mut self.thumbnails {
                thumbnails.forget_missing_backdrops();
            }
            self.show_artist_picture();
        }
    }

    /// Whether Last.fm may be asked what it says of artists.
    pub fn set_online_bios(&mut self, online: bool) {
        self.online_bios = online;
    }

    /// Takes the album page's color from its cover, once the cover's thumbnail is made.
    fn color_album_page(&mut self) {
        let Some(page) = &self.album_page else {
            return;
        };
        if page.color.is_some() {
            return;
        }
        let (Some(thumbnails), Some(track)) =
            (&mut self.thumbnails, self.cover_tracks.get(&page.cover))
        else {
            return;
        };
        let color = match thumbnails.get(&page.cover, track) {
            Thumbnail::Ready(file) => image::open(&file)
                .map(|image| freya_library::cover::page_color(image.to_rgb8().as_raw()))
                .ok(),
            // No cover: the page is grey, as for a cover of grey.
            Thumbnail::None => Some(freya_library::cover::page_color(&[])),
            Thumbnail::Pending => None,
        };
        if let Some(page) = &mut self.album_page {
            page.color = color;
        }
    }

    /// The name an artist is shown by: the one the user gave them, else the tags'.
    fn shown_artist(&self, name: Option<&str>) -> String {
        name.and_then(|n| self.edits.get(n)?.name.clone())
            .unwrap_or_else(|| artist_name(name))
    }

    /// Opens the artist of the album shown.
    pub fn open_album_artist(&mut self) {
        if let Level::AlbumTracks { artist, .. } = self.level().clone() {
            self.opened.push(Level::ArtistAlbums { artist });
            self.query.clear();
            self.reload();
        }
    }

    /// What is open now.
    pub fn place(&self) -> LibraryPlace {
        LibraryPlace {
            opened: self.opened.clone(),
            query: self.query.clone(),
        }
    }

    /// Opens what was open at `place`.
    pub fn go_to(&mut self, place: &LibraryPlace) {
        if self.place() != *place {
            self.opened = place.opened.clone();
            self.query = place.query.clone();
            self.reload();
        }
    }

    /// Orders the artists by `order`.
    pub fn set_artist_order(&mut self, order: ArtistOrder) {
        if self.artist_order != order {
            self.artist_order = order;
            self.reload_artists();
        }
    }

    /// Shows the artists as a list, or as a grid of their pictures.
    pub fn set_artist_list(&mut self, list: bool) {
        if self.artist_list != list {
            self.artist_list = list;
            self.reload_artists();
        }
    }

    /// Whether pictures of artists may come from Deezer, when they have none of their own.
    pub fn set_online_pictures(&mut self, online: bool) {
        self.online_pictures = online;
    }

    fn reload_artists(&mut self) {
        if *self.level() == Level::Artists {
            self.reload();
        }
    }

    /// Orders an artist's albums by `sort`.
    pub fn set_album_sort(&mut self, sort: AlbumSort) {
        if self.album_sort != sort {
            self.album_sort = sort;
            self.reload_albums();
        }
    }

    /// Shows an artist's albums as a list, each with its tracks, or as a grid of covers.
    pub fn set_album_list(&mut self, list: bool) {
        if self.album_list != list {
            self.album_list = list;
            self.reload_albums();
        }
    }

    fn reload_albums(&mut self) {
        if matches!(self.level(), Level::ArtistAlbums { .. }) {
            self.reload();
        }
    }

    /// Whether an artist's albums are shown with their tracks, one album after another.
    fn discography(&self) -> bool {
        self.album_list && !self.searching() && matches!(self.level(), Level::ArtistAlbums { .. })
    }

    /// The rows of the album `row` is in, in a discography: from its album row to the next
    /// album's. Elsewhere all rows.
    fn section_of(&self, row: usize) -> std::ops::Range<usize> {
        if !self.discography() {
            return 0..self.items.len();
        }
        let is_album = |item: &Item| matches!(item, Item::Album(_));
        let start = self.items[..=row].iter().rposition(is_album).unwrap_or(0);
        let end = self.items[row + 1..]
            .iter()
            .position(is_album)
            .map_or(self.items.len(), |next| row + 1 + next);
        start..end
    }

    pub fn back(&mut self) {
        if !self.query.is_empty() {
            self.query.clear();
        } else if self.opened.len() > 1 {
            self.opened.pop();
        } else {
            return;
        }
        self.reload();
    }

    /// The list shown when not searching.
    fn level(&self) -> &Level {
        self.opened
            .last()
            .expect("a section's own list is always open")
    }

    fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// Every track a row stands for: an artist's or album's, or the track itself.
    pub fn tracks_of(&self, row: usize) -> Vec<Track> {
        let Some(db) = &self.db else {
            return Vec::new();
        };
        let result = match self.items.get(row) {
            Some(Item::Artist(artist)) => db.artist_tracks(artist.name.as_deref()),
            Some(Item::Album(album)) => {
                db.album_tracks(album.artist.as_deref(), album.name.as_deref())
            }
            Some(Item::Genre(genre)) => db.genre_tracks(genre.name.as_deref()),
            Some(item @ (Item::Track(_) | Item::Played(_))) => {
                Ok(item.track().cloned().into_iter().collect())
            }
            None => Ok(Vec::new()),
        };
        result.unwrap_or_else(|e| {
            log::warn!("library: {e}");
            Vec::new()
        })
    }

    /// Every track of every row shown, each once: in a discography, the albums' tracks are
    /// rows of their own too.
    pub fn all_tracks(&self) -> Vec<Track> {
        let discography = self.discography();
        (0..self.items.len())
            .filter(|&row| !discography || matches!(self.items[row], Item::Album(_)))
            .flat_map(|row| self.tracks_of(row))
            .collect()
    }

    // ----- What the skin shows -----------------------------------------------------------------

    /// Reads the rows for the current level or search from the database.
    fn reload(&mut self) {
        if std::mem::take(&mut self.stale) {
            self.stats = None;
        }
        self.reloaded = Instant::now();
        let Some(db) = &self.db else {
            return;
        };
        let started = Instant::now();
        self.edits = db.artist_edits().unwrap_or_else(|e| {
            log::warn!("could not read what was changed about artists: {e}");
            HashMap::new()
        });
        let result = self.load(db);
        match result {
            Ok(items) => self.items = items,
            Err(e) => {
                log::warn!("library: {e}");
                self.items.clear();
            }
        }
        if self.stats.is_none() {
            self.stats = Some(db.stats().map(|s| stats_text(&s)).unwrap_or_default());
        }
        let (title, subtitle) = self.heading();
        self.title = title;
        self.subtitle = subtitle;
        self.cover_tracks.clear();
        self.artist_keys.clear();
        for item in &self.items {
            match item {
                Item::Album(album) => {
                    self.cover_tracks
                        .insert(cover_key(album), album.track.clone());
                }
                Item::Artist(artist) => {
                    self.artist_keys.insert(
                        Thumbnails::artist_key(artist.name.as_deref()),
                        artist.name.clone(),
                    );
                }
                _ => {}
            }
        }
        self.load_album_page();
        self.load_artist_page();
        let mut album_artist = match self.level() {
            Level::AlbumTracks { artist, .. } if !self.searching() => Some(artist.as_deref()),
            _ => None,
        };
        let discography = self.discography();
        let by_count = *self.level() == Level::MostPlayed;
        let now = SystemTime::now();
        let mut rows = Vec::with_capacity(self.items.len());
        for item in &self.items {
            // In a discography, each album's tracks follow it, numbered as in the album.
            if discography && let Item::Album(album) = item {
                album_artist = Some(album.artist.as_deref());
                rows.push(row_value(item, None, |_| String::new(), None));
                continue;
            }
            let plays = item.track().and_then(|t| {
                self.play_counts
                    .get(&(t.path.clone(), t.part.map(|p| p.start)))
                    .copied()
            });
            rows.push(row_value(
                item,
                album_artist,
                |played| played_text(played, by_count, now),
                plays,
            ));
        }
        self.rows.sync(rows);
        self.find_playing_row();
        log::debug!("library rows loaded in {:?}", started.elapsed());
    }

    fn load(&self, db: &Library) -> freya_library::library::Result<Vec<Item>> {
        if self.searching() {
            let tracks = db.search(&self.query, SEARCH_LIMIT)?;
            return Ok(tracks.into_iter().map(Item::Track).collect());
        }
        let albums = |albums: Vec<Album>| albums.into_iter().map(Item::Album).collect();
        let tracks = |tracks: Vec<Track>| tracks.into_iter().map(Item::Track).collect();
        Ok(match self.level() {
            Level::Artists => db
                .artists_by(self.artist_order)?
                .into_iter()
                .map(Item::Artist)
                .collect(),
            Level::Albums => albums(db.all_albums()?),
            Level::Songs => tracks(db.all_tracks()?),
            Level::Genres => db.genres()?.into_iter().map(Item::Genre).collect(),
            Level::RecentlyAdded => albums(db.recently_added(RECENT)?),
            Level::RecentlyPlayed => db
                .recently_played(RECENT)?
                .into_iter()
                .map(Item::Played)
                .collect(),
            Level::MostPlayed => db
                .most_played(RECENT)?
                .into_iter()
                .map(Item::Played)
                .collect(),
            Level::ArtistAlbums { artist } => {
                let mut list = db.albums(artist.as_deref())?;
                self.album_sort.sort(&mut list);
                if !self.album_list {
                    return Ok(albums(list));
                }
                let mut items = Vec::new();
                for album in list {
                    let tracks = db.album_tracks(album.artist.as_deref(), album.name.as_deref())?;
                    items.push(Item::Album(album));
                    items.extend(tracks.into_iter().map(Item::Track));
                }
                items
            }
            Level::GenreAlbums { genre } => albums(db.genre_albums(genre.as_deref())?),
            Level::AlbumTracks { artist, album } => {
                tracks(db.album_tracks(artist.as_deref(), album.as_deref())?)
            }
        })
    }

    fn heading(&self) -> (String, String) {
        let n = self.items.len();
        if self.searching() {
            let subtitle = if n >= SEARCH_LIMIT {
                format!("The first {} matching tracks", thousands(n))
            } else {
                count(n, "matching track")
            };
            return ("Search".to_owned(), subtitle);
        }
        let album_tracks: u32 = self
            .items
            .iter()
            .map(|i| match i {
                Item::Album(a) => a.tracks,
                _ => 0,
            })
            .sum();
        // A discography's rows are its tracks as well as its albums.
        let albums = self
            .items
            .iter()
            .filter(|i| matches!(i, Item::Album(_)))
            .count();
        let albums_and_tracks = format!(
            "{} · {}",
            count(albums, "album"),
            count(album_tracks as usize, "track")
        );
        match self.level() {
            Level::Artists => ("Artists".to_owned(), count(n, "artist")),
            Level::Albums => ("Albums".to_owned(), count(n, "album")),
            Level::Songs => ("Songs".to_owned(), count(n, "track")),
            Level::Genres => ("Genres".to_owned(), count(n, "genre")),
            Level::RecentlyAdded => ("Recently Added".to_owned(), count(n, "album")),
            Level::RecentlyPlayed => ("Recently Played".to_owned(), count(n, "track")),
            Level::MostPlayed => ("Most Played".to_owned(), count(n, "track")),
            Level::ArtistAlbums { artist } => {
                (self.shown_artist(artist.as_deref()), albums_and_tracks)
            }
            Level::GenreAlbums { genre } => (genre_name(genre.as_deref()), albums_and_tracks),
            Level::AlbumTracks { artist, album } => {
                let tracks: Vec<&Track> = self.items.iter().filter_map(Item::track).collect();
                let mut parts = vec![self.shown_artist(artist.as_deref())];
                if let Some(year) = tracks.iter().filter_map(|t| t.year).min() {
                    parts.push(year.to_string());
                }
                parts.push(count(tracks.len(), "track"));
                let total: Duration = tracks.iter().filter_map(|t| t.duration).sum();
                if !total.is_zero() {
                    parts.push(format_time(total));
                }
                (album_name(album.as_deref()), parts.join(" · "))
            }
        }
    }

    /// The cover for a row's cover key, or an empty image while it is made or when the album
    /// has none.
    pub fn cover(&mut self, key: &str) -> Image {
        if let Some(image) = self.recent_covers.get(key) {
            return image;
        }
        let state = match (&self.db, self.artist_keys.get(key)) {
            // An artist's picture, from a track of each of their albums.
            (Some(db), Some(name)) => {
                let album_tracks = db
                    .albums(name.as_deref())
                    .map(|albums| albums.into_iter().map(|a| a.track).collect())
                    .unwrap_or_default();
                let source = ArtistSource {
                    name: name.clone(),
                    album_tracks,
                    online: self.online_pictures,
                    deezer: self.downloaded(name.as_deref(), PictureKind::Portrait),
                };
                match &mut self.thumbnails {
                    Some(thumbnails) => thumbnails.get_artist(key, &source),
                    None => return Image::default(),
                }
            }
            _ => {
                let (Some(thumbnails), Some(track)) =
                    (&mut self.thumbnails, self.cover_tracks.get(key))
                else {
                    return Image::default();
                };
                thumbnails.get(key, track)
            }
        };
        let Thumbnail::Ready(file) = state else {
            return Image::default();
        };
        match Image::load_from_path(&file) {
            Ok(image) => {
                self.recent_covers.put(key, image.clone());
                image
            }
            Err(_) => {
                log::debug!("could not read the cover {}", file.display());
                Image::default()
            }
        }
    }

    /// Values for the `Library` global, except the rows and folders.
    pub fn values(&self) -> Vec<(&'static str, Value)> {
        let status = match self.progress {
            Some((done, total)) => {
                format!("Reading {} of {} files", thousands(done), thousands(total))
            }
            None if self.scanning => "Checking for changes…".to_owned(),
            None => self.stats.clone().unwrap_or_default(),
        };
        let layout = if self.searching() {
            "table"
        } else if self.discography() {
            "discography"
        } else if *self.level() == Level::Artists && !self.artist_list {
            "grid"
        } else {
            self.level().layout()
        };
        vec![
            ("section", string(self.opened[0].section_name())),
            ("layout", string(layout)),
            ("title", string(&self.title)),
            ("subtitle", string(&self.subtitle)),
            (
                "can-go-back",
                Value::Bool(self.opened.len() > 1 || !self.query.is_empty()),
            ),
            ("query", string(&self.query)),
            ("scanning", Value::Bool(self.scanning)),
            (
                "scan-folder",
                string(
                    self.scan_folder
                        .as_deref()
                        .map(folder_name)
                        .unwrap_or_default(),
                ),
            ),
            ("covers-ready", Value::Number(f64::from(self.covers_ready))),
            ("status", string(status)),
            (
                "album-options",
                Value::Bool(
                    !self.searching() && matches!(self.level(), Level::ArtistAlbums { .. }),
                ),
            ),
            ("playing-row", Value::Number(f64::from(self.playing_row))),
            ("playing-known", Value::Bool(self.playing_album.is_some())),
            ("reveal-row", Value::Number(f64::from(self.reveal_row))),
            ("reveals", Value::Number(f64::from(self.reveals))),
            ("album-page", self.album_page_value()),
            ("artist-page", self.artist_page_value()),
            ("artist-bios-online", Value::Bool(self.online_bios)),
            (
                "fanart-key",
                string(self.fanart_key.clone().unwrap_or_default()),
            ),
            (
                "artist-options",
                Value::Bool(!self.searching() && *self.level() == Level::Artists),
            ),
            ("artist-sort", string(artist_order_name(self.artist_order))),
            (
                "artist-view",
                string(if self.artist_list { "list" } else { "grid" }),
            ),
            ("artist-pictures-online", Value::Bool(self.online_pictures)),
            ("album-sort", string(self.album_sort.name())),
            (
                "album-view",
                string(if self.album_list { "list" } else { "grid" }),
            ),
        ]
    }

    /// Gives the skin the folders in use, and those being edited, when they changed.
    pub fn show_folders(
        &mut self,
        ui: &SkinWindow,
        folders: &[PathBuf],
        draft: Option<&[PathBuf]>,
    ) {
        let list = |folders: &[PathBuf]| {
            let values: Vec<Value> = folders.iter().map(|f| string(display_path(f))).collect();
            Value::Model(ModelRc::new(VecModel::from(values)))
        };
        if self.folders_shown.as_deref() != Some(folders) {
            let _ = ui.set_global_property("Library", "folders", list(folders));
            self.folders_shown = Some(folders.to_vec());
        }
        let draft = draft.unwrap_or_default();
        if self.draft_shown.as_deref() != Some(draft) {
            let _ = ui.set_global_property("Library", "folders-draft", list(draft));
            self.draft_shown = Some(draft.to_vec());
        }
    }
}

impl LibraryPanel {
    fn artist_page_value(&self) -> Value {
        let page = self.artist_page.as_ref().filter(|_| !self.searching());
        let [r, g, b] = page
            .and_then(|p| p.color)
            .unwrap_or(freya_library::cover::page_color(&[]));
        let text = |f: fn(&ArtistPage) -> String| string(page.map(f).unwrap_or_default());
        Value::Struct(Struct::from_iter([
            ("shown".to_owned(), Value::Bool(page.is_some())),
            ("name".to_owned(), text(|p| p.shown_name.clone())),
            (
                "own-biography".to_owned(),
                Value::Bool(page.is_some_and(|p| p.own_biography)),
            ),
            ("detail".to_owned(), text(|p| p.detail.clone())),
            ("genres".to_owned(), text(|p| p.genres.clone())),
            ("picture".to_owned(), text(|p| p.picture.clone())),
            (
                "banner".to_owned(),
                Value::Image(page.and_then(|p| p.banner.clone()).unwrap_or_default()),
            ),
            (
                "portrait".to_owned(),
                Value::Image(page.and_then(|p| p.portrait.clone()).unwrap_or_default()),
            ),
            (
                "picture-full".to_owned(),
                Value::Image(
                    page.and_then(|p| p.picture_full.clone())
                        .unwrap_or_default(),
                ),
            ),
            (
                "color".to_owned(),
                Value::Brush(slint::Brush::SolidColor(slint::Color::from_rgb_u8(r, g, b))),
            ),
            ("summary".to_owned(), text(|p| p.summary.clone())),
            ("biography".to_owned(), text(|p| p.biography.clone())),
            ("tags".to_owned(), text(|p| p.tags.clone())),
            ("more".to_owned(), Value::Bool(page.is_some_and(|p| p.more))),
            (
                "popular-count".to_owned(),
                Value::Number(page.map_or(0, |p| p.popular.len()) as f64),
            ),
        ]))
    }

    fn album_page_value(&self) -> Value {
        let page = self.album_page.clone().filter(|_| !self.searching());
        let [r, g, b] = page
            .as_ref()
            .and_then(|p| p.color)
            .unwrap_or(freya_library::cover::page_color(&[]));
        let page = page.unwrap_or(AlbumPage {
            title: String::new(),
            artist: String::new(),
            detail: String::new(),
            cover: String::new(),
            color: None,
        });
        Value::Struct(Struct::from_iter([
            ("shown".to_owned(), Value::Bool(!page.title.is_empty())),
            ("title".to_owned(), string(page.title)),
            ("artist".to_owned(), string(page.artist)),
            ("detail".to_owned(), string(page.detail)),
            ("cover".to_owned(), string(page.cover)),
            (
                "color".to_owned(),
                Value::Brush(slint::Brush::SolidColor(slint::Color::from_rgb_u8(r, g, b))),
            ),
        ]))
    }
}

/// An artist order by its name in the skin API and settings.
pub fn artist_order(name: &str) -> Option<ArtistOrder> {
    match name {
        "name" => Some(ArtistOrder::Name),
        "most-played" => Some(ArtistOrder::MostPlayed),
        "recently-added" => Some(ArtistOrder::RecentlyAdded),
        _ => None,
    }
}

fn artist_order_name(order: ArtistOrder) -> &'static str {
    match order {
        ArtistOrder::Name => "name",
        ArtistOrder::MostPlayed => "most-played",
        ArtistOrder::RecentlyAdded => "recently-added",
    }
}

/// "47 min 34 s", or "1 h 5 min" from an hour on, as an album's length reads.
fn long_time(total: Duration) -> String {
    let seconds = total.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours} h {minutes} min")
    } else {
        format!("{minutes} min {seconds} s")
    }
}

/// A playlist entry for a library track, with its tags already known.
pub fn entry(track: &Track) -> Entry {
    Entry::with_tags(&track.path, track.part, &track.tags())
}

/// One `LibraryRow`. `album_artist` is set while an album's tracks are shown: track numbers
/// are then shown, and the artist only where it differs from the album's. `played` says when
/// or how often a played track was played.
fn row_value(
    item: &Item,
    album_artist: Option<Option<&str>>,
    played: impl Fn(&Played) -> String,
    plays: Option<u32>,
) -> Value {
    let in_album = album_artist.is_some();
    let mut played_text = String::new();
    let mut artist = String::new();
    let mut album = String::new();
    let mut cover = String::new();
    let mut number = String::new();
    let mut duration = String::new();
    let (kind, title, detail) = match item {
        Item::Artist(a) => (
            "artist",
            {
                cover = Thumbnails::artist_key(a.name.as_deref());
                a.shown_name
                    .clone()
                    .unwrap_or_else(|| artist_name(a.name.as_deref()))
            },
            format!(
                "{} · {}",
                count(a.albums as usize, "album"),
                count(a.tracks as usize, "track")
            ),
        ),
        Item::Genre(g) => (
            "genre",
            genre_name(g.name.as_deref()),
            format!(
                "{} · {}",
                count(g.albums as usize, "album"),
                count(g.tracks as usize, "track")
            ),
        ),
        Item::Album(a) => {
            let tracks = count(a.tracks as usize, "track");
            let detail = match a.year {
                Some(year) => format!("{year} · {tracks}"),
                None => tracks,
            };
            artist = artist_name(a.artist.as_deref());
            cover = cover_key(a);
            ("album", album_name(a.name.as_deref()), detail)
        }
        Item::Track(t) | Item::Played(Played { track: t, .. }) => {
            if let Item::Played(p) = item {
                played_text = played(p);
            }
            artist = t.artist.clone().unwrap_or_default();
            album = t.album.clone().unwrap_or_default();
            let detail = match album_artist {
                // Only worth saying when the track is by someone else than the album.
                Some(album_artist) => t
                    .artist
                    .clone()
                    .filter(|a| Some(a.as_str()) != album_artist)
                    .unwrap_or_default(),
                None => [t.artist.as_deref(), t.album.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · "),
            };
            number = match (in_album, t.disc, t.track) {
                (true, Some(disc), Some(track)) if disc > 1 => format!("{disc}.{track:02}"),
                (true, _, Some(track)) => track.to_string(),
                _ => String::new(),
            };
            duration = t.duration.map(format_time).unwrap_or_default();
            ("track", t.display_title(), detail)
        }
    };
    Value::Struct(Struct::from_iter([
        ("kind".to_owned(), string(kind)),
        ("title".to_owned(), string(title)),
        ("detail".to_owned(), string(detail)),
        ("artist".to_owned(), string(artist)),
        ("album".to_owned(), string(album)),
        ("cover".to_owned(), string(cover)),
        ("number".to_owned(), string(number)),
        ("duration".to_owned(), string(duration)),
        ("played".to_owned(), string(played_text)),
        (
            "plays".to_owned(),
            string(
                plays
                    .filter(|&n| n > 0)
                    .map(|n| thousands(n as usize))
                    .unwrap_or_default(),
            ),
        ),
    ]))
}

/// "12 plays", or when the track was played last: "3 days ago".
fn played_text(played: &Played, by_count: bool, now: SystemTime) -> String {
    if by_count {
        count(played.plays as usize, "play")
    } else {
        ago(played.last, now)
    }
}

/// How long before `now` a time was: "Just now", "5 minutes ago", "3 weeks ago".
fn ago(time: SystemTime, now: SystemTime) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let seconds = now.duration_since(time).unwrap_or_default().as_secs();
    let (n, unit) = match seconds {
        s if s < MINUTE => return "Just now".to_owned(),
        s if s < HOUR => (s / MINUTE, "minute"),
        s if s < DAY => (s / HOUR, "hour"),
        s if s < 7 * DAY => (s / DAY, "day"),
        s if s < 30 * DAY => (s / (7 * DAY), "week"),
        s if s < 365 * DAY => (s / (30 * DAY), "month"),
        s => (s / (365 * DAY), "year"),
    };
    format!("{} ago", count(n as usize, unit))
}

fn cover_key(album: &Album) -> String {
    Thumbnails::key(album.artist.as_deref(), album.name.as_deref())
}

fn artist_name(name: Option<&str>) -> String {
    name.unwrap_or("Unknown artist").to_owned()
}

fn album_name(name: Option<&str>) -> String {
    name.unwrap_or("Unknown album").to_owned()
}

fn genre_name(name: Option<&str>) -> String {
    name.unwrap_or("Unknown genre").to_owned()
}

fn stats_text(stats: &freya_library::library::Stats) -> String {
    if stats.tracks == 0 {
        return String::new();
    }
    format!(
        "{} · {}",
        count(stats.tracks as usize, "track"),
        count(stats.albums as usize, "album")
    )
}

/// "1 track", "1,204 tracks".
fn count(n: usize, noun: &str) -> String {
    let plural = if n == 1 { "" } else { "s" };
    format!("{} {noun}{plural}", thousands(n))
}

/// 12345 → "12,345".
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A folder as the user knows it: "~/Music" rather than "/Users/name/Music".
fn display_path(path: &Path) -> String {
    let home = directories::BaseDirs::new().map(|d| d.home_dir().to_owned());
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if cfg!(unix) => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_what_the_internet_said_before_and_just_now() {
        let mut panel = LibraryPanel::new(None, None, None);
        panel.db = Some(Library::open_in_memory().unwrap());
        let portrait = PictureKind::Portrait;
        assert_eq!(
            panel.downloaded(Some("Ghost"), portrait),
            Downloaded::Unknown
        );
        assert_eq!(panel.downloaded(None, portrait), Downloaded::Missing);

        // Noted while a scan wrote: known before it is kept.
        panel.fetched.push(Fetched::Picture {
            artist: "Ghost".into(),
            kind: portrait,
            source: "deezer",
            picture: Some("aaa".into()),
        });
        panel.fetched.push(Fetched::ArtistId {
            artist: "Ghost".into(),
            mbid: None,
        });
        assert_eq!(
            panel.downloaded(Some("Ghost"), portrait),
            Downloaded::Kept("aaa".into())
        );
        panel.write_fetched();
        assert!(panel.fetched.is_empty());
        assert_eq!(
            panel.downloaded(Some("Ghost"), portrait),
            Downloaded::Kept("aaa".into())
        );

        // MusicBrainz found no id lately, and fanart.tv was never asked.
        let page = ArtistPage {
            name: Some("Ghost".into()),
            shown_name: "Ghost".into(),
            own_biography: false,
            detail: String::new(),
            genres: String::new(),
            picture: String::new(),
            banner: None,
            portrait: None,
            portrait_large: false,
            picture_full: None,
            color: None,
            album_tracks: Vec::new(),
            summary: String::new(),
            biography: String::new(),
            tags: String::new(),
            popular: Vec::new(),
            more: false,
        };
        let source = panel.backdrop_source(&page);
        assert_eq!((source.mbid, source.search), (None, false));
        assert_eq!(source.fanart, Downloaded::Unknown);

        // An answer of none from long ago is asked again.
        let db = panel.db.as_ref().unwrap();
        let long_ago = SystemTime::UNIX_EPOCH;
        db.set_artist_picture("Ghost", PictureKind::Backdrop, "fanart.tv", None, long_ago)
            .unwrap();
        db.set_searched_artist_id("Ghost", None, long_ago).unwrap();
        let source = panel.backdrop_source(&page);
        assert!(source.search);
        assert_eq!(source.fanart, Downloaded::Unknown);
        db.set_artist_picture(
            "Ghost",
            PictureKind::Backdrop,
            "fanart.tv",
            None,
            SystemTime::now(),
        )
        .unwrap();
        assert_eq!(panel.backdrop_source(&page).fanart, Downloaded::Missing);
    }

    #[test]
    fn opens_the_album_of_the_track_playing_at_its_row() {
        let dir = std::env::temp_dir().join(format!("freya-open-playing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
        for name in ["sine.flac", "sine.ogg"] {
            std::fs::copy(fixtures.join(name), dir.join(name)).unwrap();
        }
        let mut db = Library::open_in_memory().unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
        let mut panel = LibraryPanel::new(None, None, None);
        panel.db = Some(db);
        panel.reload();

        // Not in the library: nothing opens.
        panel.set_playing(Some((Path::new("/elsewhere.mp3"), Duration::ZERO)));
        assert!(!panel.open_playing(true));

        let playing = dir.join("sine.ogg");
        panel.set_playing(Some((&playing, Duration::ZERO)));
        assert!(panel.open_playing(true));
        assert!(matches!(panel.level(), Level::AlbumTracks { .. }));
        assert_eq!(panel.opened.len(), 3, "back goes to the artist");
        assert!(panel.playing_row >= 0);
        assert_eq!((panel.reveal_row, panel.reveals), (panel.playing_row, 1));
        let Item::Track(track) = &panel.items[panel.playing_row as usize] else {
            panic!("a track row");
        };
        assert_eq!(track.path, playing);

        assert!(panel.open_playing(false));
        assert!(matches!(panel.level(), Level::ArtistAlbums { .. }));
        assert_eq!((panel.reveal_row, panel.reveals), (-1, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shows_an_artist_by_what_the_user_changed() {
        let dir = std::env::temp_dir().join(format!("freya-edited-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
        std::fs::copy(fixtures.join("sine.mp3"), dir.join("sine.mp3")).unwrap();
        let mut db = Library::open_in_memory().unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
        let artist = db.artists().unwrap()[0].name.clone();
        assert_eq!(artist.as_deref(), Some("Freya"));
        let edit = ArtistEdit {
            name: Some("Freya Herself".into()),
            mbid: Some("0000-mbid".into()),
            biography: Some("First part.\n\nSecond part.".into()),
            ..ArtistEdit::default()
        };
        db.set_artist_edit(artist.as_deref().unwrap(), &edit)
            .unwrap();

        let mut panel = LibraryPanel::new(None, None, None);
        panel.db = Some(db);
        panel.online_bios = false;
        panel.reload();
        let Value::Model(rows) = panel.rows.model_value() else {
            panic!("a model");
        };
        let Some(Value::Struct(row)) = slint::Model::row_data(&rows, 0) else {
            panic!("an artist row");
        };
        assert_eq!(row.get_field("title"), Some(&string("Freya Herself")));

        panel.opened.push(Level::ArtistAlbums { artist });
        panel.reload();
        assert_eq!(panel.title, "Freya Herself");
        let page = panel.artist_page.as_ref().unwrap();
        assert_eq!(page.shown_name, "Freya Herself");
        assert!(page.own_biography);
        assert_eq!(page.summary, "First part.");
        assert_eq!(
            panel.backdrop_source(page).mbid.as_deref(),
            Some("0000-mbid")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_a_folder_by_its_last_two() {
        assert_eq!(
            folder_name(Path::new("/Volumes/Music/Peter Gabriel/So")),
            "Peter Gabriel / So"
        );
        assert_eq!(folder_name(Path::new("/Music")), "Music");
        assert_eq!(folder_name(Path::new("/")), "");
    }

    #[test]
    fn formats_counts() {
        assert_eq!(count(1, "track"), "1 track");
        assert_eq!(count(0, "album"), "0 albums");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn says_how_long_ago() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let ago = |seconds: u64| ago(now - Duration::from_secs(seconds), now);
        assert_eq!(ago(5), "Just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(3 * 3600 + 5), "3 hours ago");
        assert_eq!(ago(86_400), "1 day ago");
        assert_eq!(ago(20 * 86_400), "2 weeks ago");
        assert_eq!(ago(100 * 86_400), "3 months ago");
        assert_eq!(ago(800 * 86_400), "2 years ago");
    }

    fn album(name: Option<&str>, year: Option<u32>) -> Album {
        Album {
            artist: Some("Shores of Null".into()),
            name: name.map(Into::into),
            year,
            tracks: 2,
            track: PathBuf::from("/music/x.flac"),
        }
    }

    fn track(album: &str, number: u32) -> Track {
        Track {
            id: 0,
            path: PathBuf::from(format!("/music/{album}/{number}.flac")),
            part: None,
            title: Some(format!("{album} {number}")),
            artist: Some("Shores of Null".into()),
            album: Some(album.into()),
            album_artist: Some("Shores of Null".into()),
            year: None,
            track: Some(number),
            disc: None,
            duration: None,
        }
    }

    #[test]
    fn albums_sort_by_release_date_or_name() {
        let names = |albums: &[Album]| -> Vec<Option<String>> {
            albums.iter().map(|a| a.name.clone()).collect()
        };
        let mut albums = vec![
            album(Some("quiescence"), Some(2014)),
            album(None, Some(2030)),
            album(Some("Homesick"), Some(2026)),
            album(Some("Undated"), None),
            album(Some("Beyond the Shores"), Some(2020)),
        ];
        // Newest first; without a year last.
        AlbumSort::ReleaseDate.sort(&mut albums);
        assert_eq!(
            names(&albums),
            [
                None,
                Some("Homesick".into()),
                Some("Beyond the Shores".into()),
                Some("quiescence".into()),
                Some("Undated".into()),
            ]
        );
        // By name whatever the case; without a name last.
        AlbumSort::Name.sort(&mut albums);
        assert_eq!(
            names(&albums),
            [
                Some("Beyond the Shores".into()),
                Some("Homesick".into()),
                Some("quiescence".into()),
                Some("Undated".into()),
                None,
            ]
        );
        assert_eq!(AlbumSort::from_name("name"), Some(AlbumSort::Name));
        assert_eq!(
            AlbumSort::from_name(AlbumSort::ReleaseDate.name()),
            Some(AlbumSort::ReleaseDate)
        );
        assert_eq!(AlbumSort::from_name("year"), None);
    }

    #[test]
    fn a_track_in_a_discography_plays_its_own_album() {
        let mut panel = LibraryPanel::new(None, None, None);
        panel.opened = vec![Level::ArtistAlbums {
            artist: Some("Shores of Null".into()),
        }];
        panel.album_list = true;
        panel.items = vec![
            Item::Album(album(Some("Homesick"), Some(2026))),
            Item::Track(track("Homesick", 1)),
            Item::Track(track("Homesick", 2)),
            Item::Album(album(Some("Quiescence"), Some(2014))),
            Item::Track(track("Quiescence", 1)),
            Item::Track(track("Quiescence", 2)),
        ];
        assert_eq!(panel.values()[1].1, string("discography"));
        let (tracks, start) = panel.activate(5).unwrap();
        let titles: Vec<_> = tracks.iter().filter_map(|t| t.title.clone()).collect();
        assert_eq!(titles, ["Quiescence 1", "Quiescence 2"]);
        assert_eq!(start, 1);
        let (tracks, start) = panel.activate(1).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(start, 0);

        // As a grid, the same level is covers.
        panel.album_list = false;
        assert_eq!(panel.values()[1].1, string("grid"));
    }

    #[test]
    fn the_track_playing_is_marked_by_its_file_and_part() {
        let mut panel = LibraryPanel::new(None, None, None);
        let disc = |n: u64| Track {
            path: PathBuf::from("/music/disc.flac"),
            part: Some(Part {
                start: Duration::from_secs(n * 200),
                end: None,
            }),
            ..track("Disc", n as u32)
        };
        panel.items = vec![
            Item::Track(track("Homesick", 1)),
            Item::Track(disc(0)),
            Item::Track(disc(1)),
        ];
        let playing_row = |panel: &LibraryPanel| match panel
            .values()
            .into_iter()
            .find(|(name, _)| *name == "playing-row")
        {
            Some((_, Value::Number(row))) => row,
            other => panic!("{other:?}"),
        };
        assert_eq!(playing_row(&panel), -1.0);
        // The second track of a disc ripped as one file: the same file, a later start.
        panel.set_playing(Some((
            Path::new("/music/disc.flac"),
            Duration::from_secs(200),
        )));
        assert_eq!(playing_row(&panel), 2.0);
        panel.set_playing(Some((Path::new("/music/Homesick/1.flac"), Duration::ZERO)));
        assert_eq!(playing_row(&panel), 0.0);
        panel.set_playing(None);
        assert_eq!(playing_row(&panel), -1.0);
    }

    #[test]
    fn an_albums_page_has_its_heading_and_each_tracks_plays() {
        let dir = std::env::temp_dir().join(format!("freya-album-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
        for name in ["sine.flac", "sine.ogg"] {
            std::fs::copy(fixtures.join(name), dir.join(name)).unwrap();
        }
        let mut db = Library::open_in_memory().unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
        for _ in 0..1203 {
            db.record_play(&dir.join("sine.flac"), None, SystemTime::now())
                .unwrap();
        }
        let album = db
            .all_albums()
            .unwrap()
            .into_iter()
            .next()
            .expect("an album");

        let mut panel = LibraryPanel::new(None, None, None);
        panel.db = Some(db);
        panel.opened = vec![
            Level::Albums,
            Level::AlbumTracks {
                artist: album.artist.clone(),
                album: album.name.clone(),
            },
        ];
        panel.reload();
        let Value::Struct(page) = panel.album_page_value() else {
            panic!("a struct");
        };
        let text = |name: &str| match page.get_field(name) {
            Some(Value::String(s)) => s.to_string(),
            other => panic!("{name}: {other:?}"),
        };
        assert_eq!(page.get_field("shown"), Some(&Value::Bool(true)));
        assert_eq!(text("title"), album_name(album.name.as_deref()));
        assert_eq!(text("artist"), artist_name(album.artist.as_deref()));
        assert!(text("detail").contains("2 tracks"), "{}", text("detail"));
        assert!(!text("cover").is_empty());

        use slint::Model;
        let Value::Model(rows) = panel.rows.model_value() else {
            panic!("rows are a model");
        };
        let plays: Vec<String> = rows
            .iter()
            .map(|row| match row {
                Value::Struct(row) => match row.get_field("plays") {
                    Some(Value::String(s)) => s.to_string(),
                    _ => String::new(),
                },
                _ => String::new(),
            })
            .collect();
        let mut sorted = plays.clone();
        sorted.sort();
        assert_eq!(sorted, ["", "1,203"], "{plays:?}");

        // Searching, or anywhere else, has no page.
        panel.search("sine");
        assert_eq!(
            match panel.album_page_value() {
                Value::Struct(page) => page.get_field("shown").cloned(),
                _ => None,
            },
            Some(Value::Bool(false))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_artists_page_has_their_summary_and_most_played_tracks() {
        let dir = std::env::temp_dir().join(format!("freya-artist-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
        for name in ["sine.flac", "sine.ogg"] {
            std::fs::copy(fixtures.join(name), dir.join(name)).unwrap();
        }
        let mut db = Library::open_in_memory().unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
        for _ in 0..3 {
            db.record_play(&dir.join("sine.ogg"), None, SystemTime::now())
                .unwrap();
        }
        let artist = db
            .artists()
            .unwrap()
            .into_iter()
            .next()
            .expect("an artist")
            .name;

        let mut panel = LibraryPanel::new(None, None, None);
        panel.db = Some(db);
        panel.online_bios = false;
        panel.opened = vec![
            Level::Artists,
            Level::ArtistAlbums {
                artist: artist.clone(),
            },
        ];
        panel.reload();
        let Value::Struct(page) = panel.artist_page_value() else {
            panic!("a struct");
        };
        assert_eq!(page.get_field("shown"), Some(&Value::Bool(true)));
        assert_eq!(
            page.get_field("name"),
            Some(&string(artist_name(artist.as_deref())))
        );
        let Some(Value::String(detail)) = page.get_field("detail") else {
            panic!("a detail");
        };
        assert!(detail.contains("1 album · 2 tracks"), "{detail}");
        // Only what was played is popular, numbered from 1, with its plays.
        let tracks = panel.popular_tracks();
        assert_eq!(tracks.len(), 1);
        assert!(tracks[0].path.ends_with("sine.ogg"));
        let Value::Model(popular) = panel.popular_rows.model_value() else {
            panic!("a model");
        };
        use slint::Model;
        let Some(Value::Struct(row)) = popular.row_data(0) else {
            panic!("a row");
        };
        assert_eq!(row.get_field("number"), Some(&string("1")));
        assert_eq!(row.get_field("plays"), Some(&string("3")));

        // Elsewhere, no page.
        panel.opened = vec![Level::Artists];
        panel.reload();
        assert!(panel.popular_tracks().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn albums_are_as_long_as_their_tracks_in_words() {
        assert_eq!(long_time(Duration::from_secs(47 * 60 + 34)), "47 min 34 s");
        assert_eq!(
            long_time(Duration::from_secs(3600 + 5 * 60 + 27)),
            "1 h 5 min"
        );
    }

    #[test]
    fn typing_a_search_is_one_place() {
        let mut panel = LibraryPanel::new(None, None, None);
        let browsing = panel.place();
        panel.search("sho");
        let searching = panel.place();
        panel.search("shores");
        let typed_on = panel.place();
        assert!(
            !browsing.continued_by(&searching),
            "a search starts a place"
        );
        assert!(searching.continued_by(&typed_on));
        panel.search("");
        assert!(
            !typed_on.continued_by(&panel.place()),
            "ending it is another"
        );
        // Back where the search was.
        panel.go_to(&typed_on);
        assert_eq!(panel.place(), typed_on);
    }

    #[test]
    fn sections_have_their_own_layouts() {
        for name in [
            "artists",
            "albums",
            "songs",
            "genres",
            "recently-added",
            "recently-played",
            "most-played",
        ] {
            assert_eq!(Level::section(name).unwrap().section_name(), name);
        }
        assert_eq!(Level::section("podcasts"), None);
        assert_eq!(Level::Albums.layout(), "grid");
        assert_eq!(Level::GenreAlbums { genre: None }.layout(), "grid");
        assert_eq!(Level::Songs.layout(), "table");
        assert_eq!(Level::Genres.layout(), "list");
        assert_eq!(Level::RecentlyAdded.layout(), "grid");
        assert_eq!(Level::MostPlayed.layout(), "table");
    }
}
