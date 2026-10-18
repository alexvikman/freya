//! The library as the skin shows it: browsing by artist, album, song and genre, what was added
//! and played lately, searching, and the scans that keep it up to date.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use freya_library::cue::Part;
use freya_library::library::{
    Album, AlbumMatch, Artist, ArtistEdit, ArtistOrder, CHOSEN, Genre, PictureKind, PictureRow,
    Played, ScanEvent, Scanner, Track, plain_name, split_artist,
};
use freya_library::pictures::Pictures;
use freya_library::thumbnails::{
    ArtistSource, BackdropSource, Downloaded, Fetched, Thumbnail, Thumbnails,
};
use freya_library::{Entry, Library};
use slint::{Image, ModelRc, VecModel};
use slint_interpreter::{Struct, Value};

use super::articles::Articles;
use super::artist_info::ArtistInfos;
use super::match_finder::{AlbumFinder, MatchFinder};
use super::picture_finder::{Found, Pick, PictureFinder, Request};
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
    /// Pictures to make ahead of their being shown.
    prepare: VecDeque<Prepare>,
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
    /// What Wikipedia says of artists, if it may be asked.
    articles: Articles,
    online_wikipedia: bool,
    /// What the user changed about artists, by the tags' name for them.
    edits: HashMap<String, ArtistEdit>,
    /// While the user changes an artist: who, by the tags' name, and what the internet says
    /// of them and where, which their biography starts from.
    draft: Option<Option<String>>,
    found_biography: String,
    found_biography_from: String,
    /// And the pictures they can choose from, what was chosen when they began, and what is
    /// chosen now.
    finder: PictureFinder,
    choices: Vec<Choice>,
    /// What none looks like: their album covers, for a square picture.
    none_shown: HashMap<PictureKind, Image>,
    picked_before: HashMap<PictureKind, Pick>,
    picked: HashMap<PictureKind, Pick>,
    picture_error: String,
    portrait_rows: ModelRows,
    backdrop_rows: ModelRows,
    /// While the user matches an artist: who; and the artists MusicBrainz found by their name,
    /// with pictures by their id.
    artist_match: Option<Option<String>>,
    match_finder: MatchFinder,
    matches: Vec<freya_library::artist_image::ArtistMatch>,
    match_pictures: HashMap<String, Image>,
    match_error: String,
    match_rows: ModelRows,
    /// While the user matches an album: which, by the artist it is filed under and its title;
    /// and the albums MusicBrainz found, with covers by their release group.
    album_draft: Option<(Option<String>, Option<String>)>,
    album_finder: AlbumFinder,
    album_candidates: Vec<freya_library::artist_image::AlbumCandidate>,
    album_covers: HashMap<String, Image>,
    album_match_error: String,
    album_rows: ModelRows,
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
    /// Whether the biography is the user's own, rather than Last.fm's; and where it is from
    /// when not: "Wikipedia" or "Last.fm".
    own_biography: bool,
    biography_from: String,
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

/// A picture to make ahead of its being shown.
enum Prepare {
    Artist(Option<String>),
    Album(Album),
}

/// Pictures handed to the threads to make ahead, each time the library is polled.
const PREPARE_AT_ONCE: usize = 8;

/// A picture the user can choose for an artist.
struct Choice {
    kind: PictureKind,
    pick: Pick,
    source: String,
    size: String,
    image: Image,
    /// Where it goes among those found.
    order: usize,
}

/// What tells an artist MusicBrainz knows from others: "Group · Norway · 1999–".
fn match_facts(artist: &freya_library::artist_image::ArtistMatch) -> String {
    let years = match (artist.begin.get(..4), artist.end.get(..4)) {
        (Some(begin), Some(end)) if begin == end => begin.to_owned(),
        (Some(begin), end) => format!("{begin}–{}", end.unwrap_or("")),
        (None, _) => String::new(),
    };
    [artist.kind.as_str(), artist.area.as_str(), years.as_str()]
        .into_iter()
        .filter(|fact| !fact.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A kind of picture by its name in the skin API.
fn picture_kind(name: &str) -> Option<PictureKind> {
    match name {
        "portrait" => Some(PictureKind::Portrait),
        "backdrop" => Some(PictureKind::Backdrop),
        _ => None,
    }
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

    fn remove(&mut self, key: &str) {
        self.order.retain(|k| k != key);
        self.images.remove(key);
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
                .zip(pictures.clone())
                .map(|(covers, pictures)| Thumbnails::new(covers, Pictures::new(pictures))),
            fetched: Vec::new(),
            prepare: VecDeque::new(),
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
            articles: Articles::new(),
            online_wikipedia: true,
            edits: HashMap::new(),
            draft: None,
            found_biography: String::new(),
            found_biography_from: String::new(),
            finder: PictureFinder::new(pictures.clone().map(Pictures::new)),
            choices: Vec::new(),
            none_shown: HashMap::new(),
            picked_before: HashMap::new(),
            picked: HashMap::new(),
            picture_error: String::new(),
            portrait_rows: ModelRows::new(),
            backdrop_rows: ModelRows::new(),
            artist_match: None,
            match_finder: MatchFinder::new(),
            matches: Vec::new(),
            match_pictures: HashMap::new(),
            match_error: String::new(),
            match_rows: ModelRows::new(),
            album_draft: None,
            album_finder: AlbumFinder::new(),
            album_candidates: Vec::new(),
            album_covers: HashMap::new(),
            album_match_error: String::new(),
            album_rows: ModelRows::new(),
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
        let _ = ui.set_global_property(
            "Library",
            "portrait-choices",
            self.portrait_rows.model_value(),
        );
        let _ = ui.set_global_property(
            "Library",
            "backdrop-choices",
            self.backdrop_rows.model_value(),
        );
        let _ = ui.set_global_property("Library", "artist-matches", self.match_rows.model_value());
        let _ = ui.set_global_property("Library", "album-matches", self.album_rows.model_value());
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
        self.prepare_all();
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
        if let Some(thumbnails) = &mut self.thumbnails {
            thumbnails.save_index(false);
        }
        // Made again: what was read of them is old.
        for key in self
            .thumbnails
            .as_mut()
            .map(Thumbnails::take_remade)
            .unwrap_or_default()
        {
            self.recent_covers.remove(&key);
        }
        if !self.scanning {
            self.prepare_some();
        }
        if let Some(thumbnails) = &mut self.thumbnails
            && thumbnails.poll()
        {
            for key in thumbnails.take_remade() {
                self.recent_covers.remove(&key);
            }
            self.fetched.extend(thumbnails.take_fetched());
            if !self.scanning {
                self.write_fetched();
            }
            self.covers_ready += 1;
            self.color_album_page();
            self.show_artist_picture();
        }
        if self.album_draft.is_some() {
            let found = self.album_finder.poll();
            if let Some(failed) = found.failed {
                self.album_match_error = failed;
            }
            let changed = found.albums.is_some() || !found.covers.is_empty();
            if let Some(albums) = found.albums {
                self.album_candidates = albums;
            }
            for (group, cover) in found.covers {
                self.album_covers.insert(group, Image::from_rgba8(cover));
            }
            if changed {
                self.show_album_matches();
            }
        }
        if self.artist_match.is_some() {
            let found = self.match_finder.poll();
            if let Some(failed) = found.failed {
                self.match_error = failed;
            }
            let changed = found.matches.is_some() || !found.pictures.is_empty();
            if let Some(matches) = found.matches {
                self.matches = matches;
            }
            for (mbid, picture) in found.pictures {
                self.match_pictures.insert(mbid, Image::from_rgba8(picture));
            }
            if changed {
                self.show_matches();
            }
        }
        if self.draft.is_some() {
            let (found, failed) = self.finder.poll();
            if let Some(failed) = failed {
                self.picture_error = failed;
            }
            if !found.is_empty() {
                for found in found {
                    self.add_choice(found);
                }
                self.show_choices();
            }
        }
        if self.articles.poll() | self.artist_infos.poll() {
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
            self.prepare_all();
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
        if let Some(thumbnails) = &mut self.thumbnails {
            thumbnails.save_index(true);
        }
    }

    /// Keeps what the internet said of artists.
    fn write_fetched(&mut self) {
        let Some(db) = &self.db else {
            return;
        };
        self.artist_infos.save(db);
        self.articles.save(db);
        let now = SystemTime::now();
        for fetched in std::mem::take(&mut self.fetched) {
            let Some(db) = &self.db else {
                return;
            };
            let written = match &fetched {
                // Asked by an id the artist no longer has: of another artist.
                Fetched::Picture { artist, mbid, .. } if self.stale(artist, mbid) => continue,
                Fetched::Picture {
                    artist,
                    kind,
                    source,
                    picture,
                    ..
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

    /// Whether an answer about `artist` asked by the MusicBrainz id `mbid` is of another
    /// artist than theirs now.
    fn stale(&self, artist: &str, mbid: &Option<String>) -> bool {
        mbid.is_some() && self.known_mbid(Some(artist)) != *mbid
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
                mbid,
                ..
            } if a == artist && *k == kind && !self.stale(a, mbid) => Some(picture.clone()),
            _ => None,
        });
        let kept = || {
            let row = self.db.as_ref()?.artist_picture(artist, kind).ok()??;
            match row.picture {
                // The user chose one, or none: the internet is not asked.
                _ if row.chosen() => Some(None),
                Some(checksum) => Some(Some(checksum)),
                None if row.recent() => Some(None),
                None => None,
            }
        };
        match noted.or_else(kept) {
            Some(Some(checksum)) => Downloaded::Kept(checksum),
            Some(None) => Downloaded::Missing,
            None => Downloaded::Unknown,
        }
    }

    /// The picture the user chose for the artist, or that they chose none.
    fn chosen(&self, artist: Option<&str>, kind: PictureKind) -> Option<Downloaded> {
        let row = self.db.as_ref()?.artist_picture(artist?, kind).ok()??;
        if !row.chosen() {
            return None;
        }
        Some(match row.picture {
            Some(checksum) => Downloaded::Kept(checksum),
            None => Downloaded::Missing,
        })
    }

    /// Where the wide picture of the artist on the page can come from, as the library knows it
    /// now.
    fn backdrop_source(&self, page: &ArtistPage) -> BackdropSource {
        let name = page.name.as_deref();
        let edited = name.and_then(|n| self.edits.get(n)?.mbid.clone());
        let (mbid, search) = match edited {
            Some(mbid) => (Some(mbid), false),
            None => self.found_mbid(name),
        };
        BackdropSource {
            name: page.name.clone(),
            album_tracks: page.album_tracks.clone(),
            mbid,
            search,
            fanart_key: self.fanart_key.clone(),
            fanart: self.downloaded(name, PictureKind::Backdrop),
            chosen: self.chosen(name, PictureKind::Backdrop),
        }
    }

    /// The artist's MusicBrainz id as Freya knows it: the user's match, the tags', or what
    /// MusicBrainz's search found.
    fn known_mbid(&self, name: Option<&str>) -> Option<String> {
        name.and_then(|n| self.edits.get(n)?.mbid.clone())
            .or_else(|| self.found_mbid(name).0)
    }

    /// The artist's MusicBrainz id but for the user's: the tags', else what MusicBrainz's
    /// search found; and whether to search for it, when it found none long ago or was never
    /// asked.
    fn found_mbid(&self, name: Option<&str>) -> (Option<String>, bool) {
        let tagged = self
            .db
            .as_ref()
            .and_then(|db| db.artist_mbid(name).ok().flatten());
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
        match (tagged, searched) {
            (Some(mbid), _) => (Some(mbid), false),
            (None, Some((Some(mbid), _))) => (Some(mbid), false),
            // MusicBrainz found none lately: not searched again yet.
            (None, Some((None, recent))) => (None, !recent),
            (None, None) => (None, true),
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
            biography_from: String::new(),
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
        let Some(name) = self.artist_page.as_ref().and_then(|p| p.name.clone()) else {
            return;
        };
        let (summary, biography, tags, from) = self.biography_of(&name, true);
        let own = self.edits.get(&name).is_some_and(|e| e.biography.is_some());
        if let Some(page) = &mut self.artist_page {
            page.own_biography = own;
            page.summary = summary;
            page.biography = biography;
            page.tags = tags;
            page.biography_from = from;
        }
    }

    /// What is said of the artist `name`: their summary, whole biography, tags, and where
    /// from. The user's own goes before Wikipedia's article, found by their id, which goes
    /// before what Last.fm says; those not known are asked for when `ask` and allowed.
    fn biography_of(&mut self, name: &str, ask: bool) -> (String, String, String, String) {
        // The user's own replaces the internet's, which is then not asked.
        if let Some(own) = self.edits.get(name).and_then(|e| e.biography.clone()) {
            let summary = own.split("\n\n").next().unwrap_or(&own).trim().to_owned();
            return (summary, own, String::new(), String::new());
        }
        // Which artist of the name: the user's match, else the tags'; Wikipedia also takes
        // what MusicBrainz's search found.
        let matched = self
            .edits
            .get(name)
            .and_then(|e| e.mbid.clone())
            .or_else(|| self.db.as_ref()?.artist_mbid(Some(name)).ok()?);
        let known = self.known_mbid(Some(name));
        let article = known.and_then(|mbid| {
            let online = ask && self.online_wikipedia;
            self.articles
                .get(name, &mbid, online, self.db.as_ref())
                .cloned()
        });
        let info = self
            .artist_infos
            .get(
                name,
                matched.as_deref(),
                ask && self.online_bios,
                self.db.as_ref(),
            )
            .cloned();
        let tags = info
            .as_ref()
            .map(|info| {
                info.tags
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        match (article, info) {
            (Some(article), _) => (article.summary, article.biography, tags, "Wikipedia".into()),
            (None, Some(info)) => (info.summary, info.biography, tags, "Last.fm".into()),
            (None, None) => Default::default(),
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

    /// Whether Wikipedia may be asked for articles on artists.
    pub fn set_online_wikipedia(&mut self, online: bool) {
        self.online_wikipedia = online;
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

    /// Starts changing the artist whose page is shown: the form gets what was changed before.
    pub fn edit_artist(&mut self) {
        self.draft = self.artist_page.as_ref().map(|page| page.name.clone());
        // What the internet says, which their own starts from.
        let found = self.draft.clone().flatten().map(|name| {
            let own = self
                .edits
                .get_mut(&name)
                .and_then(|edit| edit.biography.take());
            let (_, biography, _, from) = self.biography_of(&name, false);
            if let Some(edit) = self.edits.get_mut(&name) {
                edit.biography = own;
            }
            (biography, from)
        });
        (self.found_biography, self.found_biography_from) = found.unwrap_or_default();
        self.choices.clear();
        self.none_shown.clear();
        self.forget_matches();
        self.picked.clear();
        self.picked_before.clear();
        self.picture_error.clear();
        self.finder.stop();
        if let Some(page) = &self.artist_page
            && let Some(name) = page.name.clone()
        {
            let (request, picked) = self.picture_request(page, &name);
            self.picked_before.clone_from(&picked);
            self.picked = picked;
            self.finder.find(request);
        }
        self.show_choices();
    }

    /// What to look for to choose an artist's pictures from, and what is chosen now.
    fn picture_request(
        &self,
        page: &ArtistPage,
        name: &str,
    ) -> (Request, HashMap<PictureKind, Pick>) {
        let mut picked = HashMap::new();
        let mut folder = Vec::new();
        if let Some(file) = freya_library::artist_image::local(&page.album_tracks) {
            folder.push((PictureKind::Portrait, file));
        }
        if let Some(file) = freya_library::artist_image::local_backdrop(&page.album_tracks) {
            folder.push((PictureKind::Backdrop, file));
        }
        let mut kept = Vec::new();
        for kind in [PictureKind::Portrait, PictureKind::Backdrop] {
            let row = self
                .db
                .as_ref()
                .and_then(|db| db.artist_picture(name, kind).ok().flatten());
            let in_folder = folder.iter().any(|(k, _)| *k == kind);
            // As the pictures are made: the user's choice, else the folder's, else the
            // internet's.
            let pick = match &row {
                Some(row) if row.chosen() => match &row.picture {
                    Some(checksum) => Pick::Kept(checksum.clone()),
                    None => Pick::Nothing,
                },
                _ if in_folder => Pick::Folder,
                Some(PictureRow {
                    picture: Some(checksum),
                    ..
                }) => Pick::Kept(checksum.clone()),
                _ => Pick::Nothing,
            };
            if let Some(PictureRow {
                picture: Some(checksum),
                source,
                ..
            }) = row
            {
                let source = match source.as_str() {
                    "deezer" => "Deezer",
                    "fanart.tv" => "fanart.tv",
                    _ => "Chosen before",
                };
                kept.push((kind, checksum, source.to_owned()));
            }
            picked.insert(kind, pick);
        }
        let backdrop = self.backdrop_source(page);
        let request = Request {
            name: freya_library::library::plain_name(name).to_owned(),
            deezer: self.online_pictures,
            mbid: backdrop.mbid,
            search: backdrop.search,
            fanart_key: self.fanart_key.clone(),
            folder,
            kept,
            album_tracks: page.album_tracks.clone(),
        };
        (request, picked)
    }

    /// Leaves the artist being changed as they were.
    pub fn cancel_artist(&mut self) {
        self.draft = None;
        self.finder.stop();
        self.choices.clear();
        self.show_choices();
        self.forget_matches();
    }

    /// Starts matching the album shown, searching MusicBrainz by its title and artist.
    pub fn start_album_match(&mut self) {
        let Level::AlbumTracks { artist, album } = self.level().clone() else {
            return;
        };
        self.album_draft = Some((artist.clone(), album.clone()));
        self.album_candidates.clear();
        self.album_covers.clear();
        self.album_match_error.clear();
        self.show_album_matches();
        if let Some(title) = album {
            let artist = artist.as_deref().map(plain_name).unwrap_or_default();
            self.album_finder.find(title, artist.to_owned());
        }
    }

    /// Searches MusicBrainz again for the album being matched, by `title` and `artist`.
    pub fn find_album_matches(&mut self, title: &str, artist: &str) {
        if self.album_draft.is_none() || title.trim().is_empty() {
            return;
        }
        self.album_match_error.clear();
        self.album_candidates.clear();
        self.show_album_matches();
        self.album_finder
            .find(title.trim().to_owned(), artist.trim().to_owned());
    }

    pub fn cancel_album_match(&mut self) {
        self.album_draft = None;
        self.album_finder.stop();
        self.album_candidates.clear();
        self.album_covers.clear();
        self.show_album_matches();
    }

    /// Matches the album being matched to the one found of the release group `group`; empty
    /// files it as its tags say again.
    pub fn save_album_match(&mut self, group: &str) {
        let group = group.trim();
        if group.is_empty() {
            self.unmatch_album();
            return;
        }
        match self
            .album_candidates
            .iter()
            .position(|album| album.release_group == group)
        {
            Some(index) => self.choose_album_match(index),
            // The match it had: nothing changes.
            None => self.cancel_album_match(),
        }
    }

    /// Matches the album being matched to the `index`th album found. Made by another artist
    /// than the one it is filed under, even one of the same name, it is filed under that
    /// artist, who is given their name, id and MusicBrainz's words for them.
    fn choose_album_match(&mut self, index: usize) {
        let (Some((Some(filed), Some(album))), Some(chosen)) = (
            self.album_draft.clone(),
            self.album_candidates.get(index).cloned(),
        ) else {
            return;
        };
        let Some(db) = &self.db else {
            return;
        };
        // As the tags name it.
        let tagged = db
            .album_match(&filed, &album)
            .ok()
            .flatten()
            .map_or_else(|| filed.clone(), |m| m.album_artist);
        let mbid = &chosen.artist.mbid;
        let theirs = self
            .edits
            .get(&tagged)
            .and_then(|e| e.mbid.clone())
            .or_else(|| self.found_mbid(Some(&tagged)).0);
        // An artist without an id yet is the one matched; one with another, another artist.
        let key = theirs
            .as_deref()
            .is_some_and(|theirs| theirs != mbid)
            .then(|| split_artist(&tagged, mbid));
        let matched = AlbumMatch {
            album_artist: tagged.clone(),
            album: album.clone(),
            release_group: Some(chosen.release_group.clone()),
            artist_mbid: mbid.clone(),
            artist_key: key.clone(),
        };
        let Some(db) = &mut self.db else {
            return;
        };
        if let Err(e) = db.match_album(&matched) {
            log::warn!("could not match {album}: {e}");
            return;
        }
        let named = match &key {
            Some(key) if !self.edits.contains_key(key) => Some((
                key.clone(),
                ArtistEdit {
                    name: Some(chosen.artist.name.clone()),
                    ..ArtistEdit::default()
                },
            )),
            None if theirs.is_none() => Some((
                tagged.clone(),
                self.edits.get(&tagged).cloned().unwrap_or_default(),
            )),
            _ => None,
        };
        if let Some((artist, edit)) = named {
            let edit = ArtistEdit {
                mbid: Some(mbid.clone()),
                note: Some(chosen.artist.disambiguation.clone()),
                ..edit
            };
            if let Err(e) = db.set_artist_edit(&artist, &edit) {
                log::warn!("could not keep who made {album}: {e}");
            }
        }
        self.cancel_album_match();
        self.show_album(Some(key.unwrap_or(tagged)), Some(album));
    }

    /// Files the album being matched as its tags say again.
    fn unmatch_album(&mut self) {
        let Some((Some(filed), Some(album))) = self.album_draft.clone() else {
            return;
        };
        let Some(db) = &mut self.db else {
            return;
        };
        let tagged = match db.album_match(&filed, &album) {
            Ok(Some(matched)) => matched.album_artist,
            _ => filed,
        };
        if let Err(e) = db.unmatch_album(&tagged, &album) {
            log::warn!("could not unmatch {album}: {e}");
        }
        self.cancel_album_match();
        self.show_album(Some(tagged), Some(album));
    }

    /// Opens the album `album` filed under `artist`, under their page.
    fn show_album(&mut self, artist: Option<String>, album: Option<String>) {
        self.opened = vec![
            Level::Artists,
            Level::ArtistAlbums {
                artist: artist.clone(),
            },
            Level::AlbumTracks { artist, album },
        ];
        self.query.clear();
        self.reload();
    }

    /// The album being matched, as `album-draft` gives it.
    fn album_draft_value(&self) -> Value {
        let (artist, title) = self.album_draft.clone().unwrap_or_default();
        let matched = match (&self.db, &artist, &title) {
            (Some(db), Some(artist), Some(title)) => db.album_match(artist, title).ok().flatten(),
            _ => None,
        };
        let folder = self
            .items
            .iter()
            .find_map(Item::track)
            .and_then(|t| t.path.parent())
            .map(|p| p.display().to_string())
            .filter(|_| self.album_draft.is_some())
            .unwrap_or_default();
        Value::Struct(Struct::from_iter([
            ("title".to_owned(), string(title.unwrap_or_default())),
            (
                "artist".to_owned(),
                string(self.shown_artist(artist.as_deref())),
            ),
            ("folder".to_owned(), string(folder)),
            (
                "release-group".to_owned(),
                string(matched.and_then(|m| m.release_group).unwrap_or_default()),
            ),
        ]))
    }

    /// Gives the skin the albums MusicBrainz found.
    fn show_album_matches(&mut self) {
        let rows = self
            .album_candidates
            .iter()
            .map(|album| {
                let year = album.date.get(..4).unwrap_or_default();
                let facts = [album.kind.as_str(), year]
                    .into_iter()
                    .filter(|f| !f.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ");
                Value::Struct(Struct::from_iter([
                    ("release-group".to_owned(), string(&album.release_group)),
                    ("title".to_owned(), string(&album.title)),
                    ("facts".to_owned(), string(facts)),
                    ("artist".to_owned(), string(&album.artist.name)),
                    ("detail".to_owned(), string(&album.artist.disambiguation)),
                    (
                        "picture".to_owned(),
                        Value::Image(
                            self.album_covers
                                .get(&album.release_group)
                                .cloned()
                                .unwrap_or_default(),
                        ),
                    ),
                ]))
            })
            .collect();
        self.album_rows.sync(rows);
    }

    /// Starts matching the artist whose page is shown, searching MusicBrainz by their name.
    pub fn start_artist_match(&mut self) {
        self.forget_matches();
        self.artist_match = self.artist_page.as_ref().map(|page| page.name.clone());
        if let Some(Some(name)) = &self.artist_match {
            let name = plain_name(name).to_owned();
            self.match_finder.find(name, self.fanart_key.clone());
        }
    }

    /// Matches the artist being matched to the one with the MusicBrainz id `mbid`; empty, or
    /// Freya's own match, lets Freya match them again.
    pub fn save_artist_match(&mut self, mbid: &str) {
        let Some(Some(artist)) = self.artist_match.clone() else {
            return;
        };
        let found = self.found_mbid(Some(&artist)).0;
        let mbid = Some(mbid.trim())
            .filter(|mbid| !mbid.is_empty() && found.as_deref() != Some(*mbid))
            .map(str::to_owned);
        let before = self.edits.get(&artist).cloned().unwrap_or_default();
        // MusicBrainz's words for them: from the search, else as kept.
        let note = mbid.as_ref().and_then(|mbid| {
            let searched = self.matches.iter().find(|m| m.mbid == *mbid);
            match searched {
                Some(found) => Some(found.disambiguation.clone()),
                None => before
                    .note
                    .clone()
                    .filter(|_| before.mbid.as_ref() == Some(mbid)),
            }
        });
        self.set_artist_mbid(&artist, mbid, note);
    }

    pub fn cancel_artist_match(&mut self) {
        self.artist_match = None;
        self.forget_matches();
    }

    /// Keeps the user's match of `artist`, ending the matching, and shows what comes of it.
    fn set_artist_mbid(&mut self, artist: &str, mbid: Option<String>, note: Option<String>) {
        self.cancel_artist_match();
        let Some(db) = &self.db else {
            return;
        };
        let before = self.edits.get(artist).cloned().unwrap_or_default();
        let edit = ArtistEdit {
            mbid,
            note,
            ..before.clone()
        };
        if let Err(e) = db.set_artist_edit(artist, &edit) {
            log::warn!("could not keep the match of {artist}: {e}");
            return;
        }
        if edit.mbid != before.mbid {
            self.forget_found(artist);
        }
        self.reload();
    }

    /// The artist being matched, as `artist-match` gives it.
    fn artist_match_value(&self) -> Value {
        let artist = self.artist_match.clone().flatten();
        let mbid = artist
            .as_deref()
            .and_then(|a| self.edits.get(a)?.mbid.clone());
        let found = self.found_mbid(artist.as_deref()).0;
        Value::Struct(Struct::from_iter([
            (
                "artist".to_owned(),
                string(artist.as_deref().map(plain_name).unwrap_or_default()),
            ),
            ("mbid".to_owned(), string(mbid.unwrap_or_default())),
            ("found-mbid".to_owned(), string(found.unwrap_or_default())),
        ]))
    }

    /// Searches MusicBrainz for the artists named `query`, to say which is the one matched.
    pub fn find_matches(&mut self, query: &str) {
        let query = query.trim();
        if self.artist_match.is_none() || query.is_empty() {
            return;
        }
        self.match_error.clear();
        self.matches.clear();
        self.show_matches();
        self.match_finder
            .find(query.to_owned(), self.fanart_key.clone());
    }

    fn forget_matches(&mut self) {
        self.match_finder.stop();
        self.matches.clear();
        self.match_pictures.clear();
        self.match_error.clear();
        self.show_matches();
    }

    /// Gives the skin the artists MusicBrainz found.
    fn show_matches(&mut self) {
        let rows = self
            .matches
            .iter()
            .map(|m| {
                Value::Struct(Struct::from_iter([
                    ("mbid".to_owned(), string(&m.mbid)),
                    ("name".to_owned(), string(&m.name)),
                    ("detail".to_owned(), string(&m.disambiguation)),
                    ("facts".to_owned(), string(match_facts(m))),
                    (
                        "picture".to_owned(),
                        Value::Image(
                            self.match_pictures
                                .get(&m.mbid)
                                .cloned()
                                .unwrap_or_default(),
                        ),
                    ),
                ]))
            })
            .collect();
        self.match_rows.sync(rows);
    }

    /// Chooses the `index`th picture of `kind` ("portrait" or "backdrop"); the first is none.
    pub fn choose_picture(&mut self, kind: &str, index: usize) {
        let Some(kind) = picture_kind(kind) else {
            return;
        };
        let pick = match index {
            0 => Some(Pick::Nothing),
            i => self
                .choices
                .iter()
                .filter(|c| c.kind == kind)
                .nth(i - 1)
                .map(|c| c.pick.clone()),
        };
        if let Some(pick) = pick {
            self.picked.insert(kind, pick);
            self.show_choices();
        }
    }

    /// Adds the picture in `file` as one of `kind` to choose, and chooses it.
    pub fn add_picture_file(&mut self, kind: &str, file: &Path) {
        let Some(kind) = picture_kind(kind) else {
            return;
        };
        let added = std::fs::read(file)
            .map_err(|e| e.to_string())
            .and_then(|bytes| self.finder.add(kind, &bytes));
        match added {
            Ok(found) => {
                self.picture_error.clear();
                self.add_choice(found);
            }
            Err(e) => self.picture_error = format!("Could not use that file: {e}"),
        }
        self.show_choices();
    }

    /// Downloads the picture at `url` as one of `kind` to choose; it is chosen when it comes.
    pub fn picture_from_url(&mut self, kind: &str, url: &str) {
        let (Some(kind), url) = (picture_kind(kind), url.trim()) else {
            return;
        };
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            self.picture_error = "An address starts with https://".to_owned();
            return;
        }
        self.picture_error.clear();
        self.finder.download(kind, url.to_owned());
    }

    fn add_choice(&mut self, found: Found) {
        if found.choose {
            self.picked.insert(found.kind, found.pick.clone());
        }
        if found.pick == Pick::Nothing {
            self.none_shown
                .insert(found.kind, Image::from_rgba8(found.preview));
            return;
        }
        // The same picture once, as it was found first.
        if self
            .choices
            .iter()
            .any(|c| c.kind == found.kind && c.pick == found.pick)
        {
            return;
        }
        let at = self.choices.partition_point(|c| c.order <= found.order);
        self.choices.insert(
            at,
            Choice {
                kind: found.kind,
                pick: found.pick,
                source: found.source,
                size: format!("{} × {}", found.width, found.height),
                image: Image::from_rgba8(found.preview),
                order: found.order,
            },
        );
    }

    /// Gives the skin the pictures to choose from.
    fn show_choices(&mut self) {
        let rows = |kind: PictureKind| -> Vec<Value> {
            let picked = self.picked.get(&kind);
            let row = |image: Image, source: &str, size: &str, chosen: bool| {
                Value::Struct(Struct::from_iter([
                    ("picture".to_owned(), Value::Image(image)),
                    ("source".to_owned(), string(source)),
                    ("size".to_owned(), string(size)),
                    ("chosen".to_owned(), Value::Bool(chosen)),
                ]))
            };
            if self.draft.is_none() {
                return Vec::new();
            }
            // None, shown as what it means: their album covers, when there are any.
            let none = self.none_shown.get(&kind).cloned();
            let label = if none.is_some() {
                "Album covers"
            } else {
                "None"
            };
            std::iter::once(row(
                none.unwrap_or_default(),
                label,
                "",
                picked == Some(&Pick::Nothing),
            ))
            .chain(
                self.choices
                    .iter()
                    .filter(|c| c.kind == kind)
                    .map(|c| row(c.image.clone(), &c.source, &c.size, picked == Some(&c.pick))),
            )
            .collect()
        };
        let portraits = rows(PictureKind::Portrait);
        let backdrops = rows(PictureKind::Backdrop);
        self.portrait_rows.sync(portraits);
        self.backdrop_rows.sync(backdrops);
    }

    /// Keeps the pictures the user chose for `artist`. Returns whether any changed.
    fn keep_picked(&mut self, artist: &str) -> bool {
        let Some(db) = &self.db else {
            return false;
        };
        let mut changed = false;
        for kind in [PictureKind::Portrait, PictureKind::Backdrop] {
            let (Some(picked), Some(before)) =
                (self.picked.get(&kind), self.picked_before.get(&kind))
            else {
                continue;
            };
            if picked == before {
                continue;
            }
            let kept = match picked {
                // No choice of theirs: the folder's comes first then.
                Pick::Folder => db.forget_artist_picture(artist, kind),
                Pick::Nothing => {
                    db.set_artist_picture(artist, kind, CHOSEN, None, SystemTime::now())
                }
                Pick::Kept(checksum) => {
                    db.set_artist_picture(artist, kind, CHOSEN, Some(checksum), SystemTime::now())
                }
            };
            match kept {
                Ok(()) => changed = true,
                Err(e) => log::warn!("could not keep the picture chosen for {artist}: {e}"),
            }
        }
        changed
    }

    /// The form for changing an artist, as `artist-draft` gives it.
    fn draft_value(&self) -> Value {
        let artist = self
            .artist_page
            .as_ref()
            .filter(|_| self.draft.is_some())
            .and_then(|p| p.name.clone());
        let edit = artist
            .as_deref()
            .and_then(|a| self.edits.get(a).cloned())
            .unwrap_or_default();
        // Without the user's: the tags', else what MusicBrainz's search found.
        let found_mbid = self.found_mbid(artist.as_deref()).0.unwrap_or_default();
        let text = |value: Option<String>| string(value.unwrap_or_default());
        // Theirs, else Last.fm's to start from.
        let found = edit.biography.is_none() && !self.found_biography.is_empty();
        let biography = edit
            .biography
            .clone()
            .unwrap_or_else(|| self.found_biography.clone());
        Value::Struct(Struct::from_iter([
            (
                "artist".to_owned(),
                text(artist.map(|a| freya_library::library::plain_name(&a).to_owned())),
            ),
            ("name".to_owned(), text(edit.name)),
            ("sort-name".to_owned(), text(edit.sort_name)),
            ("mbid".to_owned(), text(edit.mbid)),
            ("found-mbid".to_owned(), string(found_mbid)),
            ("biography".to_owned(), string(biography)),
            (
                "biography-from".to_owned(),
                string(if found {
                    self.found_biography_from.as_str()
                } else {
                    ""
                }),
            ),
        ]))
    }

    /// Keeps what the user changed about the artist being changed, and shows it. A new
    /// MusicBrainz id looks again for their wide picture.
    pub fn save_artist(&mut self, edit: &ArtistEdit) {
        let (Some(Some(artist)), Some(db)) = (self.draft.take(), &self.db) else {
            return;
        };
        let before = self.edits.get(&artist).cloned().unwrap_or_default();
        // Last.fm's, saved unchanged, stays Last.fm's: it is not the user's own.
        let mut edit = edit.clone();
        // MusicBrainz's words for the artist matched, kept with the id.
        let mbid = edit.mbid.as_deref().map(str::trim);
        edit.note = if mbid == before.mbid.as_deref() {
            before.note.clone()
        } else {
            self.matches
                .iter()
                .find(|m| Some(m.mbid.as_str()) == mbid)
                .map(|m| m.disambiguation.clone())
        };
        if edit.biography.as_deref().map(str::trim) == Some(self.found_biography.trim()) {
            edit.biography = None;
        }
        if let Err(e) = db.set_artist_edit(&artist, &edit) {
            log::warn!("could not keep what was changed about {artist}: {e}");
            return;
        }
        let kept = db.artist_edit(&artist).unwrap_or_default();
        self.finder.stop();
        self.forget_matches();
        if self.keep_picked(&artist) {
            let key = Thumbnails::artist_key(Some(&artist));
            if let Some(thumbnails) = &mut self.thumbnails {
                thumbnails.forget_artist(&key);
            }
            self.recent_covers.remove(&key);
            self.covers_ready += 1;
        }
        self.choices.clear();
        self.show_choices();
        if kept.mbid != before.mbid {
            self.forget_found(&artist);
        }
        self.reload();
    }

    /// Forgets what the internet gave for `artist`, but for the pictures the user chose, so
    /// it is asked again: after they were matched to another artist of the name.
    fn forget_found(&mut self, artist: &str) {
        if let Some(db) = &self.db {
            for kind in [PictureKind::Portrait, PictureKind::Backdrop] {
                let chosen = db
                    .artist_picture(artist, kind)
                    .ok()
                    .flatten()
                    .is_some_and(|row| row.chosen());
                if !chosen && let Err(e) = db.forget_artist_picture(artist, kind) {
                    log::warn!("could not forget the pictures of {artist}: {e}");
                }
            }
        }
        let key = Thumbnails::artist_key(Some(artist));
        if let Some(thumbnails) = &mut self.thumbnails {
            thumbnails.forget_artist(&key);
        }
        self.recent_covers.remove(&key);
        self.covers_ready += 1;
        // And Last.fm is asked about the artist now matched.
        self.artist_infos.forget(artist, self.db.as_ref());
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
        // Known already, made or not: asked for again on every scroll and every new picture,
        // without the library being read for it.
        let known = self.thumbnails.as_ref().and_then(|t| t.known(key));
        let state = match (known, &self.db, self.artist_keys.get(key)) {
            (Some(known), ..) => known,
            // An artist's picture, from a track of each of their albums.
            (None, Some(_), Some(name)) => {
                let source = self.artist_source(name.as_deref());
                match &mut self.thumbnails {
                    Some(thumbnails) => thumbnails.get_artist(key, &source),
                    None => return Image::default(),
                }
            }
            (None, ..) => {
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

    /// Where the artist `name`'s picture can come from, as the library knows it now.
    fn artist_source(&self, name: Option<&str>) -> ArtistSource {
        let album_tracks = self
            .db
            .as_ref()
            .and_then(|db| db.albums(name).ok())
            .map(|albums| albums.into_iter().map(|a| a.track).collect())
            .unwrap_or_default();
        // By their id, when Freya knows it, without searching for it.
        let fanart = self
            .fanart_key
            .clone()
            .and_then(|key| Some((self.known_mbid(name)?, key)));
        ArtistSource {
            name: name.map(str::to_owned),
            album_tracks,
            online: self.online_pictures,
            fanart,
            deezer: self.downloaded(name, PictureKind::Portrait),
            chosen: self.chosen(name, PictureKind::Portrait),
        }
    }

    /// Lines up every artist's and album's picture to be made ahead of its being shown.
    fn prepare_all(&mut self) {
        let Some(db) = &self.db else {
            return;
        };
        self.prepare.clear();
        if let Ok(artists) = db.artists() {
            self.prepare
                .extend(artists.into_iter().map(|a| Prepare::Artist(a.name)));
        }
        if let Ok(albums) = db.all_albums() {
            self.prepare.extend(albums.into_iter().map(Prepare::Album));
        }
    }

    /// Hands a few of the pictures lined up to the threads: the window is not held up.
    fn prepare_some(&mut self) {
        for _ in 0..PREPARE_AT_ONCE {
            let Some(next) = self.prepare.pop_front() else {
                return;
            };
            match next {
                Prepare::Artist(name) => {
                    let key = Thumbnails::artist_key(name.as_deref());
                    let source = self.artist_source(name.as_deref());
                    if let Some(thumbnails) = &mut self.thumbnails {
                        thumbnails.prepare_artist(&key, &source);
                    }
                }
                Prepare::Album(album) => {
                    if let Some(thumbnails) = &mut self.thumbnails {
                        thumbnails.prepare(&cover_key(&album), &album.track);
                    }
                }
            }
        }
    }

    /// What the library is doing in the background, for the user to see: what, and of what.
    pub fn activities(&self) -> Vec<(String, String)> {
        let mut activities = Vec::new();
        if self.scanning {
            let what = match self.progress {
                Some((done, total)) => {
                    format!("Reading {} of {} files", thousands(done), thousands(total))
                }
                None => "Checking the music folders for changes".to_owned(),
            };
            let folder = self.scan_folder.as_deref().map(folder_name);
            activities.push((what, folder.unwrap_or_default()));
        }
        let pending = self.thumbnails.as_ref().map_or(0, Thumbnails::pending);
        if pending > 0 {
            activities.push((
                "Making covers and pictures".to_owned(),
                count(pending, "picture"),
            ));
        }
        for (source, waiting) in [
            ("Last.fm", self.artist_infos.waiting().collect::<Vec<_>>()),
            ("Wikipedia", self.articles.waiting().collect()),
        ] {
            let mut asked: Vec<&str> = waiting.into_iter().map(plain_name).collect();
            if !asked.is_empty() {
                asked.sort_unstable();
                activities.push((format!("Asking {source} about artists"), asked.join(", ")));
            }
        }
        let page_name = || {
            self.artist_page
                .as_ref()
                .map(|p| p.shown_name.clone())
                .unwrap_or_default()
        };
        if self.draft.is_some() && self.finder.searching() {
            activities.push(("Looking for pictures".to_owned(), page_name()));
        }
        if self.match_finder.searching() {
            activities.push(("Asking MusicBrainz about an artist".to_owned(), page_name()));
        }
        if self.album_finder.searching() {
            let title = self
                .album_draft
                .clone()
                .and_then(|(_, album)| album)
                .unwrap_or_default();
            activities.push(("Asking MusicBrainz about an album".to_owned(), title));
        }
        activities
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
                "artist-wikipedia-online",
                Value::Bool(self.online_wikipedia),
            ),
            (
                "fanart-key",
                string(self.fanart_key.clone().unwrap_or_default()),
            ),
            ("artist-draft", self.draft_value()),
            ("finding-pictures", Value::Bool(self.finder.searching())),
            ("picture-error", string(&self.picture_error)),
            (
                "finding-matches",
                Value::Bool(self.match_finder.searching()),
            ),
            ("match-error", string(&self.match_error)),
            ("album-draft", self.album_draft_value()),
            ("artist-match", self.artist_match_value()),
            (
                "finding-album-matches",
                Value::Bool(self.album_finder.searching()),
            ),
            ("album-match-error", string(&self.album_match_error)),
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
            (
                "biography-from".to_owned(),
                text(|p| p.biography_from.clone()),
            ),
            (
                "note".to_owned(),
                string(
                    page.and_then(|p| self.edits.get(p.name.as_deref()?)?.note.clone())
                        .unwrap_or_default(),
                ),
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
    name.map_or("Unknown artist", freya_library::library::plain_name)
        .to_owned()
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
            mbid: None,
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
            biography_from: String::new(),
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
    fn changes_an_artist_from_the_form() {
        let dir = std::env::temp_dir().join(format!("freya-form-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
        std::fs::copy(fixtures.join("sine.mp3"), dir.join("sine.mp3")).unwrap();
        let mut db = Library::open_in_memory().unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
        db.set_artist_picture(
            "Freya",
            PictureKind::Backdrop,
            "fanart.tv",
            None,
            SystemTime::now(),
        )
        .unwrap();
        db.set_searched_artist_id("Freya", Some("found-id"), SystemTime::now())
            .unwrap();
        let mut panel = LibraryPanel::new(None, None, None);
        panel.db = Some(db);
        panel.online_bios = false;
        panel.opened = vec![
            Level::Artists,
            Level::ArtistAlbums {
                artist: Some("Freya".into()),
            },
        ];
        panel.reload();

        // Nothing to save without the form open.
        panel.save_artist(&ArtistEdit::default());
        panel.edit_artist();
        let Value::Struct(draft) = panel.draft_value() else {
            panic!("a struct");
        };
        assert_eq!(draft.get_field("artist"), Some(&string("Freya")));
        assert_eq!(draft.get_field("name"), Some(&string("")));
        assert_eq!(draft.get_field("found-mbid"), Some(&string("found-id")));

        panel.save_artist(&ArtistEdit {
            name: Some("Freya Herself".into()),
            mbid: Some("own-id".into()),
            ..ArtistEdit::default()
        });
        assert_eq!(panel.title, "Freya Herself");
        assert!(panel.draft.is_none(), "the form is done");
        // Another id: fanart.tv is asked again.
        let db = panel.db.as_ref().unwrap();
        assert_eq!(
            db.artist_picture("Freya", PictureKind::Backdrop).unwrap(),
            None
        );
        let page = panel.artist_page.as_ref().unwrap();
        assert_eq!(panel.backdrop_source(page).mbid.as_deref(), Some("own-id"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chooses_and_keeps_an_artists_pictures() {
        let dir = std::env::temp_dir().join(format!("freya-choose-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let album = dir.join("music").join("Freya").join("Sines");
        std::fs::create_dir_all(&album).unwrap();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
        std::fs::copy(fixtures.join("sine.mp3"), album.join("sine.mp3")).unwrap();
        image::RgbImage::from_pixel(1920, 1080, image::Rgb([0, 0, 200]))
            .save(dir.join("music").join("Freya").join("fanart.jpg"))
            .unwrap();
        let mut db = Library::open_in_memory().unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        freya_library::library::scan(&mut db, &[dir.join("music")], &cancel, |_| {}).unwrap();
        let mut panel = LibraryPanel::new(None, None, Some(dir.join("pictures")));
        panel.db = Some(db);
        panel.online_pictures = false;
        panel.online_bios = false;
        panel.opened = vec![
            Level::Artists,
            Level::ArtistAlbums {
                artist: Some("Freya".into()),
            },
        ];
        panel.reload();

        let finish = |panel: &mut LibraryPanel| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while panel.finder.searching() {
                assert!(Instant::now() < deadline, "the pictures were not found");
                panel.poll();
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        panel.edit_artist();
        finish(&mut panel);
        // The folder's wide picture, chosen as Freya takes it first; none square.
        assert_eq!(panel.picked[&PictureKind::Backdrop], Pick::Folder);
        assert_eq!(panel.picked[&PictureKind::Portrait], Pick::Nothing);
        let chosen = |rows: &ModelRows| -> Vec<bool> {
            let Value::Model(model) = rows.model_value() else {
                panic!("a model");
            };
            (0..slint::Model::row_count(&model))
                .map(|i| match slint::Model::row_data(&model, i) {
                    Some(Value::Struct(row)) => row.get_field("chosen") == Some(&Value::Bool(true)),
                    _ => false,
                })
                .collect()
        };
        assert_eq!(
            chosen(&panel.backdrop_rows),
            [false, true],
            "None, then the folder's"
        );

        // None wide, and a picture of the user's own square.
        panel.choose_picture("backdrop", 0);
        let own = dir.join("own.png");
        image::RgbImage::from_pixel(500, 500, image::Rgb([0, 200, 0]))
            .save(&own)
            .unwrap();
        panel.add_picture_file("portrait", &own);
        assert!(matches!(
            panel.picked[&PictureKind::Portrait],
            Pick::Kept(_)
        ));
        assert_eq!(chosen(&panel.portrait_rows), [false, true]);
        panel.save_artist(&ArtistEdit::default());

        let db = panel.db.as_ref().unwrap();
        let wide = db
            .artist_picture("Freya", PictureKind::Backdrop)
            .unwrap()
            .unwrap();
        assert!(wide.chosen() && wide.picture.is_none());
        let square = db
            .artist_picture("Freya", PictureKind::Portrait)
            .unwrap()
            .unwrap();
        assert!(square.chosen() && square.picture.is_some());
        assert!(panel.choices.is_empty(), "the dialog is done");

        // The folder's again: no choice of the user's.
        panel.edit_artist();
        finish(&mut panel);
        assert_eq!(panel.picked[&PictureKind::Backdrop], Pick::Nothing);
        panel.choose_picture("backdrop", 1);
        panel.save_artist(&ArtistEdit::default());
        let db = panel.db.as_ref().unwrap();
        assert_eq!(
            db.artist_picture("Freya", PictureKind::Backdrop).unwrap(),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_an_album_under_the_artist_it_is_matched_to() {
        let dir = std::env::temp_dir().join(format!("freya-album-match-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
        std::fs::copy(fixtures.join("sine.mp3"), dir.join("sine.mp3")).unwrap();
        let mut db = Library::open_in_memory().unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
        let album = db.all_albums().unwrap()[0].name.clone();
        let mut panel = LibraryPanel::new(None, None, None);
        panel.db = Some(db);
        panel.online_bios = false;
        let candidate = |mbid: &str| freya_library::artist_image::AlbumCandidate {
            release_group: format!("rg-{mbid}"),
            title: "Sines".into(),
            artist: freya_library::artist_image::ArtistMatch {
                mbid: mbid.into(),
                name: "Freya".into(),
                disambiguation: format!("the {mbid} one"),
                ..Default::default()
            },
            ..Default::default()
        };
        let matching = |panel: &mut LibraryPanel, artist: &str, mbid: &str| {
            panel.show_album(Some(artist.to_owned()), album.clone());
            panel.album_draft = Some((Some(artist.to_owned()), album.clone()));
            panel.album_candidates = vec![candidate(mbid)];
            panel.choose_album_match(0);
        };

        // Freya had no id for the artist: the first match is theirs, and nothing splits.
        matching(&mut panel, "Freya", "first");
        assert_eq!(panel.edits["Freya"].mbid.as_deref(), Some("first"));
        assert_eq!(panel.edits["Freya"].note.as_deref(), Some("the first one"));
        assert!(
            matches!(panel.level(), Level::AlbumTracks { artist: Some(a), .. } if a == "Freya")
        );
        assert!(panel.album_draft.is_none(), "matching is done");

        // Matched to another artist of the name: filed under them, shown as "Freya".
        matching(&mut panel, "Freya", "other");
        let split = split_artist("Freya", "other");
        assert!(matches!(panel.level(), Level::AlbumTracks { artist: Some(a), .. } if *a == split));
        assert_eq!(panel.edits[&split].name.as_deref(), Some("Freya"));
        assert_eq!(panel.edits[&split].note.as_deref(), Some("the other one"));
        assert_eq!(panel.title, album_name(album.as_deref()));
        assert_eq!(panel.album_page.as_ref().unwrap().artist, "Freya");

        // Back as the tags say.
        panel.album_draft = Some((Some(split.clone()), album.clone()));
        panel.unmatch_album();
        assert!(
            matches!(panel.level(), Level::AlbumTracks { artist: Some(a), .. } if a == "Freya")
        );
        let db = panel.db.as_ref().unwrap();
        assert_eq!(db.albums(Some(&split)).unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn matches_an_artist_from_the_form() {
        let dir = std::env::temp_dir().join(format!("freya-artist-match-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
        std::fs::copy(fixtures.join("sine.mp3"), dir.join("sine.mp3")).unwrap();
        let mut db = Library::open_in_memory().unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
        db.set_searched_artist_id("Freya", Some("se"), SystemTime::now())
            .unwrap();
        let mut panel = LibraryPanel::new(None, None, None);
        panel.db = Some(db);
        panel.online_bios = false;
        panel.opened = vec![
            Level::Artists,
            Level::ArtistAlbums {
                artist: Some("Freya".into()),
            },
        ];
        panel.reload();
        panel.edit_artist();
        panel.matches = vec![freya_library::artist_image::ArtistMatch {
            mbid: "no".into(),
            name: "Freya".into(),
            disambiguation: "Norwegian group".into(),
            ..Default::default()
        }];
        panel.save_artist(&ArtistEdit {
            mbid: Some("no".into()),
            ..ArtistEdit::default()
        });
        assert_eq!(panel.edits["Freya"].mbid.as_deref(), Some("no"));
        assert_eq!(
            panel.edits["Freya"].note.as_deref(),
            Some("Norwegian group")
        );
        let Value::Struct(page) = panel.artist_page_value() else {
            panic!("a struct");
        };
        assert_eq!(page.get_field("note"), Some(&string("Norwegian group")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forgets_a_late_answer_about_another_artist_of_the_name() {
        let mut panel = LibraryPanel::new(None, None, None);
        let db = Library::open_in_memory().unwrap();
        db.set_artist_edit(
            "Shining",
            &ArtistEdit {
                mbid: Some("swedish".into()),
                ..ArtistEdit::default()
            },
        )
        .unwrap();
        panel.db = Some(db);
        panel.reload();
        let answer = |mbid: &str| Fetched::Picture {
            artist: "Shining".into(),
            kind: PictureKind::Backdrop,
            source: "fanart.tv",
            picture: Some(format!("{mbid}-picture")),
            mbid: Some(mbid.into()),
        };
        // Asked while they were matched to the Norwegian band, answered after.
        panel.fetched.push(answer("norwegian"));
        assert_eq!(
            panel.downloaded(Some("Shining"), PictureKind::Backdrop),
            Downloaded::Unknown
        );
        panel.write_fetched();
        let db = panel.db.as_ref().unwrap();
        assert_eq!(
            db.artist_picture("Shining", PictureKind::Backdrop).unwrap(),
            None
        );

        panel.fetched.push(answer("swedish"));
        panel.write_fetched();
        let db = panel.db.as_ref().unwrap();
        let row = db
            .artist_picture("Shining", PictureKind::Backdrop)
            .unwrap()
            .unwrap();
        assert_eq!(row.picture.as_deref(), Some("swedish-picture"));
    }

    #[test]
    fn tells_what_sets_an_artist_apart() {
        let artist = |kind: &str, area: &str, begin: &str, end: &str| {
            freya_library::artist_image::ArtistMatch {
                kind: kind.into(),
                area: area.into(),
                begin: begin.into(),
                end: end.into(),
                ..Default::default()
            }
        };
        assert_eq!(
            match_facts(&artist("Group", "Norway", "1999", "")),
            "Group · Norway · 1999–"
        );
        assert_eq!(
            match_facts(&artist("Person", "", "1943-02-25", "2001-11-29")),
            "Person · 1943–2001"
        );
        assert_eq!(match_facts(&artist("", "Sweden", "", "")), "Sweden");
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
