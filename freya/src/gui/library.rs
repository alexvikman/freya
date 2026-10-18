//! The library as the skin shows it: browsing by artist, album, song and genre, what was added
//! and played lately, searching, and the scans that keep it up to date.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use freya_library::cue::Part;
use freya_library::library::{Album, Artist, Genre, Played, ScanEvent, Scanner, Track};
use freya_library::thumbnails::{Thumbnail, Thumbnails};
use freya_library::{Entry, Library};
use slint::{Image, ModelRc, VecModel};
use slint_interpreter::{Struct, Value};

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
    /// `Library.covers-ready`: goes up as covers are made.
    covers_ready: i32,
    /// Plays not yet written: a scan may be writing to the database.
    plays: Vec<(PathBuf, Option<Part>, SystemTime)>,
    /// A scan was started and its end not yet taken in.
    scanning: bool,
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
    /// A library kept at `path`, with album covers made in `covers`.
    pub fn new(path: Option<PathBuf>, covers: Option<PathBuf>) -> Self {
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
            thumbnails: covers.map(Thumbnails::new),
            cover_tracks: HashMap::new(),
            recent_covers: RecentCovers::default(),
            covers_ready: 0,
            plays: Vec::new(),
            scanning: false,
        }
    }

    /// Shows the library in a skin window that has the `Library` global. The first time,
    /// opens the library and scans `folders` for changes.
    /// Opens the library for `ui`, the first time also bringing it in line with `folders`,
    /// unless `scan` is false: when the settings were damaged, a missing folder list would
    /// otherwise remove every track.
    pub fn attach(&mut self, ui: &SkinWindow, folders: &[PathBuf], scan: bool) {
        let _ = ui.set_global_property("Library", "rows", self.rows.model_value());
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
        self.reload();
        if scan {
            self.rescan(folders);
        } else {
            log::warn!("the library is not checked against its folders: the settings were damaged");
        }
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
        if self.thumbnails.as_mut().is_some_and(Thumbnails::poll) {
            self.covers_ready += 1;
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
        // Plays noted while the scan wrote.
        if finished && self.write_plays() && self.shows_plays() {
            self.stale = true;
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
            Item::Track(_) | Item::Played(_) => {
                let tracks: Vec<Track> =
                    self.items.iter().filter_map(Item::track).cloned().collect();
                let start = self.items[..row]
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

    /// Every track of every row shown.
    pub fn all_tracks(&self) -> Vec<Track> {
        (0..self.items.len())
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
        for item in &self.items {
            if let Item::Album(album) = item {
                self.cover_tracks
                    .insert(cover_key(album), album.track.clone());
            }
        }
        let album_artist = match self.level() {
            Level::AlbumTracks { artist, .. } if !self.searching() => Some(artist.as_deref()),
            _ => None,
        };
        let by_count = *self.level() == Level::MostPlayed;
        let now = SystemTime::now();
        let rows: Vec<Value> = self
            .items
            .iter()
            .map(|item| {
                row_value(item, album_artist, |played| {
                    played_text(played, by_count, now)
                })
            })
            .collect();
        self.rows.sync(rows);
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
            Level::Artists => db.artists()?.into_iter().map(Item::Artist).collect(),
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
            Level::ArtistAlbums { artist } => albums(db.albums(artist.as_deref())?),
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
        let albums_and_tracks = format!(
            "{} · {}",
            count(n, "album"),
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
            Level::ArtistAlbums { artist } => (artist_name(artist.as_deref()), albums_and_tracks),
            Level::GenreAlbums { genre } => (genre_name(genre.as_deref()), albums_and_tracks),
            Level::AlbumTracks { artist, album } => {
                let tracks: Vec<&Track> = self.items.iter().filter_map(Item::track).collect();
                let mut parts = vec![artist_name(artist.as_deref())];
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
        let (Some(thumbnails), Some(track)) = (&mut self.thumbnails, self.cover_tracks.get(key))
        else {
            return Image::default();
        };
        let Thumbnail::Ready(file) = thumbnails.get(key, track) else {
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
            ("covers-ready", Value::Number(f64::from(self.covers_ready))),
            ("status", string(status)),
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
            artist_name(a.name.as_deref()),
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
