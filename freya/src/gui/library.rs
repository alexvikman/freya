//! The library as the skin shows it: browsing by artist, album, song and genre, what was added
//! and played lately, searching, and the scans that keep it up to date.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};

use freya_library::Entry;
use freya_library::thumbnails::{ArtistSource, Thumbnail, Thumbnails};
use freya_protocol::{
    Album, AlbumKey, AlbumMatch, AlbumOrder, Artist, ArtistEdit, ArtistOrder, CHOSEN, Genre,
    Header, LibraryApi, Part, PictureKind, PictureRow, Played, ScanEvent, Server, ServerSettings,
    ServiceEvent, Track, plain_name, split_artist,
};
#[cfg(test)]
use freya_server::{Service, ServiceConfig};
use slint::{Image, ModelRc, VecModel};
use slint_interpreter::{Struct, Value};

use super::cover_decoder::CoverDecoder;
pub use playlist_page::PlaylistSort;
use playlist_page::{PlaylistDraft, PlaylistPage};

#[path = "genre_page.rs"]
mod genre_page;
#[path = "playlist_page.rs"]
mod playlist_page;
#[path = "search_page.rs"]
mod search_page;
use super::match_finder::{AlbumFinder, MatchFinder};
use super::picture_finder::{Found, Pick, PictureFinder, Request};
use super::skin_state::{ModelRows, format_time, string};
use super::skin_window::SkinWindow;
use genre_page::GenreDraft;
use search_page::FoundPlaylist;
pub use search_page::{Opened, SearchFilter};

/// Search results are cut off here; nobody scrolls through more, and building rows for the
/// whole library on every keystroke would be wasted work.
const SEARCH_LIMIT: usize = 1_000;
/// While a scan runs, the rows are reloaded at most this often.
const RELOAD_INTERVAL: Duration = Duration::from_secs(1);
/// Decoded covers kept: about 270 KB each, 70 MB in all. Those on screen, at most about 90 full
/// screen on a 4K screen, and those decoded ahead of them both ways.
const RECENT_COVERS: usize = 256;
/// Covers decoded ahead of those on screen, each way, in the order the rows are listed.
const DECODE_AHEAD: usize = 64;
/// Covers handed to the decoder each time the library is polled.
const DECODE_AT_ONCE: usize = 24;
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
    Playlists,
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
            "playlists" => Self::Playlists,
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
            Self::Playlists => "playlists",
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
            Self::Genres | Self::Playlists => "grid",
            Self::Artists | Self::AlbumTracks { .. } => "list",
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
#[derive(Clone)]
enum Item {
    Artist(Artist),
    Album(Album),
    Genre(Genre),
    Track(Track),
    /// A track in Recently Played or Most Played.
    Played(Played),
    /// A playlist a search found.
    Playlist(FoundPlaylist),
}

impl Item {
    fn track(&self) -> Option<&Track> {
        match self {
            Self::Track(track) | Self::Played(Played { track, .. }) => Some(track),
            _ => None,
        }
    }
}

/// Where the library comes from.
#[derive(Clone, Debug)]
pub enum LibrarySource {
    /// Freya's server on this computer, at `address`, started from `program` when none answers
    /// there.
    Local {
        address: std::net::SocketAddr,
        program: PathBuf,
    },
    /// A server on another computer, its certificate pinned, called `name`.
    Remote {
        pin: freya_client::Pin,
        name: String,
    },
    /// A library in this process, kept at `database` with its pictures in `pictures`, its
    /// scans run here: as tests have it.
    #[cfg(test)]
    InProcess {
        database: PathBuf,
        pictures: Option<PathBuf>,
    },
}

impl LibrarySource {
    /// Whether the library is on this computer.
    fn here(&self) -> bool {
        !matches!(self, Self::Remote { .. })
    }
}

/// How often a server that could not be reached is tried again.
const RECONNECT_EVERY: Duration = Duration::from_secs(10);

pub struct LibraryPanel {
    /// Where the library comes from; none for none.
    source: Option<LibrarySource>,
    /// The library, opened when a skin first shows it.
    db: Option<Arc<dyn LibraryApi>>,
    /// The server that keeps it, what it tells of its scans, and whether it was connected
    /// before: when connected again, what it did meanwhile is read anew.
    server: Option<Arc<dyn Server>>,
    server_events: Option<Receiver<ServiceEvent>>,
    connected: bool,
    /// How many events the server had told, as far as heard: a client following it again
    /// knows whether it missed any.
    told: Option<u64>,
    /// The server as this client calls it: for its account, and reading tracks from it.
    remote: Option<Arc<freya_protocol::Remote<freya_client::Http>>>,
    /// Where its tracks are read from, and how; made once connected.
    audio: Option<(String, Arc<ureq::Agent>)>,
    /// "connecting", "connected", "unreachable" or "log-in", as `Server.state`; why, and when
    /// it was last tried.
    server_state: &'static str,
    server_error: String,
    tried: Option<Instant>,
    /// The server told that the playlists changed, and how scrobbling goes, since the app
    /// last asked.
    playlists_changed: bool,
    lastfm_status: Option<freya_protocol::LastFmStatus>,
    /// Where thumbnails of covers and pictures are made, once the library is open.
    covers_dir: Option<PathBuf>,
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
    /// Each album's cover as the last scan made it, by the album's key; and each artist's
    /// pictures, square and wide, as kept, by the artist.
    made_covers: HashMap<String, freya_protocol::AlbumCover>,
    portraits: HashMap<String, String>,
    backdrops: HashMap<String, String>,
    recent_covers: RecentCovers,
    /// Covers decoded on threads; the rows' covers' keys in order, with where each is; the
    /// rows whose covers the skin asked for since the last poll, and those it asked for
    /// before, which are on screen; and whether one it asked for was not decoded yet.
    decoder: CoverDecoder,
    cover_order: Vec<String>,
    cover_rows: HashMap<String, usize>,
    covers_asked: Option<(usize, usize)>,
    covers_shown: (usize, usize),
    covers_missed: bool,
    /// Pictures to make ahead of their being shown.
    prepare: VecDeque<Prepare>,
    /// `Library.covers-ready`: goes up as covers are made.
    covers_ready: i32,
    /// How an artist's albums are ordered, and whether they are shown as a list of albums
    /// with their tracks rather than a grid of covers.
    album_sort: AlbumSort,
    album_list: bool,
    /// A scan was started and its end not yet taken in.
    scanning: bool,
    /// How the server keeps the library, as the user set it: its folders, scans, and what may
    /// be asked of the internet.
    settings: ServerSettings,
    /// The folder a scan looks through or reads now, and how far it is with albums' covers
    /// and with asking the internet about artists.
    scan_folder: Option<PathBuf>,
    covers_progress: Option<(usize, usize)>,
    artists_progress: Option<(usize, usize)>,
    genres_progress: Option<(usize, usize)>,
    /// Whether the covers and pictures in the library changed since they were last read.
    pictures_stale: bool,
    /// The track playing, by its file and where in it it starts, and its row, or -1.
    playing: Option<(PathBuf, Duration)>,
    playing_row: i32,
    /// The album artist and album of the track playing, when it is in the library.
    playing_album: Option<(Option<String>, Option<String>)>,
    /// A row for the skin to put the keyboard on and scroll to, each time `reveals` counts
    /// up; -1 to show the list from the top.
    reveal_row: i32,
    reveals: i32,
    /// How artists are ordered, and whether they show as a list rather than a grid of
    /// pictures.
    artist_order: ArtistOrder,
    artist_list: bool,
    /// The artists shown, by their picture's key.
    artist_keys: HashMap<String, Option<String>>,
    /// While an artist's albums are shown, the page over them.
    artist_page: Option<ArtistPage>,
    /// Albums' covers whole, read on a thread.
    full_covers: (Sender<FullCover>, Receiver<FullCover>),
    /// The artist page's pictures a thread made, by the page's picture's key.
    page_pictures: (Sender<MadePage>, Receiver<MadePage>),
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
    matches: Vec<freya_protocol::ArtistMatch>,
    match_pictures: HashMap<String, Image>,
    match_error: String,
    match_rows: ModelRows,
    /// While the user matches an album: which, by the artist it is filed under and its title;
    /// and the albums MusicBrainz found, with covers by their release group.
    album_draft: Option<(Option<String>, Option<String>)>,
    album_finder: AlbumFinder,
    album_candidates: Vec<freya_protocol::AlbumCandidate>,
    album_covers: HashMap<String, Image>,
    album_match_error: String,
    album_rows: ModelRows,
    popular_rows: ModelRows,
    /// The letters to jump to in the artists or albums shown, for `Library.letters`.
    letter_rows: ModelRows,
    /// The playlist shown, its tracks' rows for `Playlists.entries` and how they are ordered;
    /// the one being changed; and each playlist's picture, by its key.
    playlist_page: Option<PlaylistPage>,
    playlist_rows: ModelRows,
    playlist_sort: PlaylistSort,
    playlist_draft: Option<PlaylistDraft>,
    playlist_sources: HashMap<String, ArtistSource>,
    /// What a search shows: one kind or all, the sections of the page of all and its best
    /// match; a few matches under the search field as the user types; the playlists it can
    /// find; and a playlist a match opened, for the app to show.
    /// How the albums are ordered; and the user's favourite albums, and when each was made
    /// one.
    albums_order: AlbumOrder,
    favorites: HashMap<AlbumKey, SystemTime>,
    /// The favourite artists and playlists, by their names; and how the playlists are
    /// ordered: by when they were made or made favourites, or by name.
    artist_favorites: HashMap<String, SystemTime>,
    playlist_favorites: HashMap<String, SystemTime>,
    playlists_by_name: bool,
    /// What the user gave genres, by their names in lower case; the genres shown, by their
    /// pictures' keys; and the one being changed.
    genre_infos: HashMap<String, freya_protocol::GenreInfo>,
    genre_keys: HashMap<String, Option<String>>,
    genre_draft: Option<GenreDraft>,
    search_filter: SearchFilter,
    search_top: Option<usize>,
    search_songs: Option<(usize, usize)>,
    search_section_rows: ModelRows,
    suggestions: Vec<Item>,
    suggestion_rows: ModelRows,
    found_playlists: Vec<FoundPlaylist>,
    open_playlist: Option<String>,
    /// While an album's tracks are shown: how often each was played, and its page.
    play_counts: HashMap<(PathBuf, Option<Duration>), u32>,
    album_page: Option<AlbumPage>,
}

/// What an artist's own page shows over their albums; or a genre's, over its.
struct ArtistPage {
    /// A genre's page rather than an artist's.
    genre: bool,
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
    /// The files of their pictures a thread was last asked to make for the page.
    asked: Option<PageFiles>,
    color: Option<[u8; 3]>,
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
    /// An album's cover, by its checksum.
    Cover(String),
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
fn match_facts(artist: &freya_protocol::ArtistMatch) -> String {
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

/// The picture `picture`, square and cut round with a soft edge, premultiplied, for a skin to
/// show as is: Slint's software renderer clips to rectangles only.
fn round_picture(picture: &image::DynamicImage) -> slint::SharedPixelBuffer<slint::Rgba8Pixel> {
    let square = picture
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
    pixels
}

/// The files an artist's page shows: their wide picture, and their picture with whether it
/// is the large one.
type PageFiles = (Option<PathBuf>, Option<(PathBuf, bool)>);

/// An artist's pictures as their page shows them, made on a thread: decoding them on the
/// window's thread held up the page as it opened.
#[derive(Default)]
struct PagePictures {
    /// Their wide picture, and its color.
    banner: Option<(slint::SharedPixelBuffer<slint::Rgb8Pixel>, Option<[u8; 3]>)>,
    portrait: Option<PagePortrait>,
}

/// An artist's picture as their page shows it.
struct PagePortrait {
    /// Cut round.
    round: slint::SharedPixelBuffer<slint::Rgba8Pixel>,
    /// Whether it is their picture large, rather than the grid's; and then uncut.
    large: bool,
    full: Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>,
    color: [u8; 3],
}

/// An artist page's pictures made, by the page's picture's key.
type MadePage = (String, PagePictures);

impl PagePictures {
    fn make((banner, portrait): &PageFiles) -> Self {
        let color = |picture: &image::DynamicImage| {
            freya_library::cover::page_color(picture.thumbnail(64, 64).to_rgb8().as_raw())
        };
        let rgb = |picture: &image::DynamicImage| {
            let rgb = picture.to_rgb8();
            slint::SharedPixelBuffer::clone_from_slice(rgb.as_raw(), rgb.width(), rgb.height())
        };
        let open = |file: &Path| {
            image::open(file)
                .map_err(|e| log::debug!("could not read {}: {e}", file.display()))
                .ok()
        };
        Self {
            banner: banner
                .as_deref()
                .and_then(open)
                .map(|picture| (rgb(&picture), Some(color(&picture)))),
            portrait: portrait.as_ref().and_then(|(file, large)| {
                let picture = open(file)?;
                Some(PagePortrait {
                    round: round_picture(&picture),
                    large: *large,
                    full: large.then(|| rgb(&picture)),
                    color: color(&picture),
                })
            }),
        }
    }
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
    /// The cover whole and large, once read after the user asked to see it.
    cover_full: Option<Image>,
    /// A track of the album, which its cover is read from.
    track: PathBuf,
}

/// An album's cover whole, read on a thread: the album's cover key, and the picture.
type FullCover = (String, slint::SharedPixelBuffer<slint::Rgb8Pixel>);

/// The longest side of an album's cover shown whole.
const COVER_FULL: u32 = 1400;

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
    /// The library from `source`, with thumbnails of its covers and pictures made in
    /// `covers`.
    pub fn new(source: Option<LibrarySource>, covers: Option<PathBuf>) -> Self {
        Self {
            source,
            db: None,
            server: None,
            server_events: None,
            connected: false,
            told: None,
            remote: None,
            audio: None,
            server_state: "connecting",
            server_error: String::new(),
            tried: None,
            playlists_changed: false,
            lastfm_status: None,
            covers_dir: covers,
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
            thumbnails: None,
            prepare: VecDeque::new(),
            decoder: CoverDecoder::new(),
            cover_order: Vec::new(),
            cover_rows: HashMap::new(),
            covers_asked: None,
            covers_shown: (0, 0),
            covers_missed: false,
            made_covers: HashMap::new(),
            portraits: HashMap::new(),
            backdrops: HashMap::new(),
            recent_covers: RecentCovers::default(),
            covers_ready: 0,
            scanning: false,
            settings: ServerSettings::default(),
            scan_folder: None,
            covers_progress: None,
            artists_progress: None,
            genres_progress: None,
            pictures_stale: true,
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
            artist_keys: HashMap::new(),
            artist_page: None,
            page_pictures: std::sync::mpsc::channel(),
            full_covers: std::sync::mpsc::channel(),
            edits: HashMap::new(),
            draft: None,
            found_biography: String::new(),
            found_biography_from: String::new(),
            finder: PictureFinder::new(None),
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
            popular_rows: ModelRows::new(),
            letter_rows: ModelRows::new(),
            playlist_page: None,
            playlist_rows: ModelRows::new(),
            playlist_sort: PlaylistSort::Custom,
            playlist_draft: None,
            playlist_sources: HashMap::new(),
            albums_order: AlbumOrder::Added,
            favorites: HashMap::new(),
            artist_favorites: HashMap::new(),
            playlist_favorites: HashMap::new(),
            playlists_by_name: false,
            genre_infos: HashMap::new(),
            genre_keys: HashMap::new(),
            genre_draft: None,
            search_filter: SearchFilter::All,
            search_top: None,
            search_songs: None,
            search_section_rows: ModelRows::new(),
            suggestions: Vec::new(),
            suggestion_rows: ModelRows::new(),
            found_playlists: Vec::new(),
            open_playlist: None,
        }
    }

    /// Shows the library in a skin window that has the `Library` global. The first time,
    /// connects to the server that keeps it, which brings it in line with its folders.
    pub fn attach(&mut self, ui: &SkinWindow) {
        let _ = ui.set_global_property("Library", "rows", self.rows.model_value());
        let _ = ui.set_global_property("Library", "popular", self.popular_rows.model_value());
        let _ = ui.set_global_property("Library", "letters", self.letter_rows.model_value());
        let _ = ui.set_global_property("Playlists", "entries", self.playlist_rows.model_value());
        let _ = ui.set_global_property(
            "Library",
            "search-sections",
            self.search_section_rows.model_value(),
        );
        let _ =
            ui.set_global_property("Library", "suggestions", self.suggestion_rows.model_value());
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
        let Some(source) = self.source.clone() else {
            return;
        };
        let (api, server): (Arc<dyn LibraryApi>, Arc<dyn Server>) = match source {
            LibrarySource::Local { address, program } => {
                let (Some(pairing_key), Some(certificate), Some(store)) = (
                    crate::settings::pairing_key_path(),
                    crate::settings::server_certificate_path(),
                    crate::settings::token_store(),
                ) else {
                    log::warn!("no library: no folder for Freya's files");
                    return;
                };
                let connected = freya_client::connect_locally(
                    address,
                    &program,
                    &pairing_key,
                    &certificate,
                    &store,
                    &crate::settings::device_name(),
                );
                let remote = match connected {
                    Ok(remote) => remote,
                    Err(e) => {
                        log::warn!("no library: {e}");
                        self.server_state = "unreachable";
                        self.server_error = e;
                        return;
                    }
                };
                self.remote = Some(Arc::clone(&remote));
                (Arc::clone(&remote) as _, remote)
            }
            LibrarySource::Remote { pin, .. } => {
                let Some(store) = crate::settings::token_store() else {
                    log::warn!("no library: no folder for Freya's files");
                    return;
                };
                let cache = crate::settings::cache_dir().map(|dir| dir.join("pictures"));
                let remote = freya_client::connect_pinned(pin, store, cache);
                match remote.0.account() {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        self.server_state = "log-in";
                        self.server_error =
                            "The server no longer takes this computer: log in again.".into();
                        return;
                    }
                    Err(e) => {
                        log::warn!("could not reach the server: {e}");
                        self.server_state = "unreachable";
                        self.server_error = e.to_string();
                        return;
                    }
                }
                self.remote = Some(Arc::clone(&remote));
                (Arc::clone(&remote) as _, remote)
            }
            #[cfg(test)]
            LibrarySource::InProcess { database, pictures } => {
                let config = ServiceConfig {
                    settings: self.settings.clone(),
                    settings_file: None,
                    scans_allowed: true,
                    lastfm: freya_server::lastfm(),
                    playlists: database.parent().map(|dir| dir.join("playlists")),
                    scrobbles: None,
                };
                match Service::start(database, pictures, config) {
                    Ok(service) => {
                        let service = Arc::new(service);
                        (service.api(), service)
                    }
                    Err(e) => {
                        log::warn!("{e}");
                        return;
                    }
                }
            }
        };
        match server.settings() {
            Ok(settings) => self.settings = settings,
            Err(e) => log::warn!("could not read the server's settings: {e}"),
        }
        match server.events() {
            Ok(events) => self.server_events = Some(events),
            Err(e) => log::warn!("could not follow the server: {e}"),
        }
        self.server = Some(server);
        if let Some(remote) = &self.remote {
            let (audio, agent) = remote.0.audio(freya_audio::REMOTE_TIMEOUT);
            self.audio = Some((audio, Arc::new(agent)));
        }
        self.server_state = "connected";
        self.server_error.clear();
        self.open(api);
    }

    /// Where the server serves the file of the library's track at `path`, when it is to be
    /// played from there: when the server is on another computer, or always with
    /// `FREYA_STREAM` set, to try it on one computer. A path a server elsewhere gives is never
    /// looked for here: it names a file on that computer, and a hostile server could name one
    /// here, or another computer's share for Windows to log in to. Not connected, such a track
    /// is played from nowhere.
    pub fn stream_url(&self, path: &Path) -> Option<freya_audio::RemoteSource> {
        let source = self.source.as_ref()?;
        let elsewhere = !source.here();
        let nowhere = || freya_audio::RemoteSource {
            url: String::new(),
            bearer: None,
            agent: None,
        };
        if freya_audio::is_url(&path.to_string_lossy()) {
            // Played from the web; one a server elsewhere names, only from far away.
            return None;
        }
        let forced = std::env::var_os("FREYA_STREAM").is_some();
        if !elsewhere && !forced {
            return None;
        }
        let connected = self.remote.as_ref().zip(self.audio.as_ref());
        match connected {
            Some((remote, (audio, agent))) => Some(freya_audio::RemoteSource {
                url: freya_client::audio_url_at(audio, path),
                bearer: remote.0.bearer(),
                agent: Some(Arc::clone(agent)),
            }),
            None if elsewhere => Some(nowhere()),
            None => None,
        }
    }

    /// Whether the library is a server's on another computer, which may name streams only
    /// beyond this computer and its network: [`freya_audio::Media::far_only`].
    pub fn elsewhere(&self) -> bool {
        self.source.as_ref().is_some_and(|source| !source.here())
    }

    /// The server as this client calls it, once connected.
    pub fn remote(&self) -> Option<Arc<freya_protocol::Remote<freya_client::Http>>> {
        self.remote.clone()
    }

    /// Where the library is, for `Server.remote`, `name` and `address`.
    pub fn source_shown(&self) -> (bool, String, String) {
        match &self.source {
            Some(LibrarySource::Remote { pin, name }) => {
                (true, name.clone(), pin.address.to_string())
            }
            _ => (false, String::new(), String::new()),
        }
    }

    /// How the connection to the server goes, and why not, for `Server.state` and `error`.
    pub fn server_state(&self) -> (&'static str, &str) {
        (self.server_state, &self.server_error)
    }

    /// Whether to connect again now: the server could not be reached, a while ago.
    pub fn wants_reconnect(&mut self) -> bool {
        // Once connected, the events follow the server again by themselves.
        let due = self.server_state == "unreachable"
            && self.db.is_none()
            && self.tried.is_none_or(|at| at.elapsed() >= RECONNECT_EVERY);
        if due {
            self.tried = Some(Instant::now());
        }
        due
    }

    /// Whether clients on other computers may connect to the server, and what they find it as.
    pub fn set_network(&mut self, on: bool) {
        self.settings.network = on;
        self.tell_settings();
    }

    pub fn set_server_name(&mut self, name: &str) {
        self.settings.name = name.trim().to_owned();
        self.tell_settings();
    }

    /// The library, once open.
    pub fn api(&self) -> Option<Arc<dyn LibraryApi>> {
        self.db.clone()
    }

    /// The server that keeps the library, once connected.
    pub fn server(&self) -> Option<Arc<dyn Server>> {
        self.server.clone()
    }

    /// How scrobbling goes, when the server told since this was last asked.
    pub fn take_lastfm_status(&mut self) -> Option<freya_protocol::LastFmStatus> {
        self.lastfm_status.take()
    }

    /// Whether the server told that the playlists changed since this was last asked.
    pub fn take_playlists_changed(&mut self) -> bool {
        std::mem::take(&mut self.playlists_changed)
    }

    /// How the server keeps the library, as the user set it; once it is open.
    pub fn server_settings(&self) -> Option<&ServerSettings> {
        self.server.as_ref().map(|_| &self.settings)
    }

    /// Tells the server the settings as they are now.
    fn tell_settings(&self) {
        if let Some(server) = &self.server
            && let Err(e) = server.set_settings(&self.settings)
        {
            log::warn!("could not change the server's settings: {e}");
        }
    }

    /// Shows the library `api`: its rows, and its pictures through thumbnails made in the
    /// covers folder.
    fn open(&mut self, api: Arc<dyn LibraryApi>) {
        if let Some(covers) = self.covers_dir.clone() {
            let source = Arc::clone(&api);
            self.thumbnails = Some(Thumbnails::new(
                covers,
                Arc::new(move |checksum| source.picture(checksum).ok().flatten()),
            ));
        }
        self.finder = PictureFinder::new(Some(Arc::clone(&api)));
        self.db = Some(api);
        self.find_playing_album();
        self.clear_older_cache();
        self.prepare_all();
        self.reload();
    }

    /// The library's folders now: those watched, and scanned later.
    pub fn set_folders(&mut self, folders: &[PathBuf]) {
        if self.settings.library_folders != folders {
            self.settings.library_folders = folders.to_vec();
            self.tell_settings();
        }
    }

    /// Whether the folders are watched, and what changes in them scanned at once.
    pub fn set_watch_folders(&mut self, watch: bool) {
        self.settings.watch_folders = watch;
        self.tell_settings();
    }

    /// How often the whole library is scanned, in hours; 0 for never.
    pub fn set_scan_every_hours(&mut self, hours: u32) {
        self.settings.scan_every_hours = hours;
        self.tell_settings();
    }

    pub fn set_scan_at_start(&mut self, scan: bool) {
        self.settings.scan_at_start = scan;
        self.tell_settings();
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

    /// Brings the library in line with `folders`: reads what changed, and asks the internet
    /// for what artists lack.
    pub fn rescan(&mut self, folders: &[PathBuf]) {
        self.set_folders(folders);
        if let Some(server) = &self.server
            && let Err(e) = server.rescan()
        {
            log::warn!("could not scan: {e}");
        }
    }

    /// Reads every file in `folders` again and makes every cover again, changed or not.
    pub fn rescan_files(&mut self, folders: &[PathBuf]) {
        self.set_folders(folders);
        if let Some(server) = &self.server
            && let Err(e) = server.rescan_files()
        {
            log::warn!("could not scan: {e}");
        }
    }

    /// Looks through the `index`th of `folders` only.
    pub fn scan_folder(&mut self, folders: &[PathBuf], index: usize) {
        if let Some(folder) = folders.get(index) {
            self.scan_added(folders, vec![folder.clone()]);
        }
    }

    /// Looks through `added`, folders new in `folders`, only.
    pub fn scan_added(&mut self, folders: &[PathBuf], added: Vec<PathBuf>) {
        self.set_folders(folders);
        if let Some(server) = &self.server
            && let Err(e) = server.scan_only(&added)
        {
            log::warn!("could not scan: {e}");
        }
    }

    /// Stops the scan running; what it did so far is kept.
    pub fn stop_scan(&mut self) {
        if let Some(server) = &self.server
            && let Err(e) = server.stop_scan()
        {
            log::warn!("could not stop the scan: {e}");
        }
    }

    /// Handles scan progress and covers made. Returns true when the rows changed.
    pub fn poll(&mut self) -> bool {
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
        if self.decode_covers() {
            self.covers_ready += 1;
        }
        self.take_page_pictures();
        self.take_full_covers();
        if let Some(thumbnails) = &mut self.thumbnails
            && thumbnails.poll()
        {
            for key in thumbnails.take_remade() {
                self.recent_covers.remove(&key);
            }
            self.covers_ready += 1;
            self.color_album_page();
            self.show_artist_picture();
            self.show_playlist_picture();
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
        let mut finished = false;
        let events: Vec<ServiceEvent> = self
            .server_events
            .as_ref()
            .map(|events| events.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            if !matches!(event, ServiceEvent::Connected { .. } | ServiceEvent::Lost) {
                self.told = self.told.map(|told| told + 1);
            }
            match event {
                ServiceEvent::Connected { scanning, told } => {
                    self.scanning = scanning;
                    self.server_state = "connected";
                    self.server_error.clear();
                    // What changed while the server could not tell is read anew.
                    let missed = self.told.is_some_and(|expected| expected != told);
                    if std::mem::replace(&mut self.connected, true) && missed {
                        self.stale = true;
                        self.pictures_stale = true;
                    }
                    self.told = Some(told);
                }
                ServiceEvent::Lost => {
                    self.server_state = "unreachable";
                    self.server_error = "Its connection was lost.".into();
                }
                ServiceEvent::ScanStarted => self.scanning = true,
                ServiceEvent::Scan(ScanEvent::Progress { done, total }) => {
                    self.progress = Some((done, total));
                    self.stale = true;
                }
                ServiceEvent::Scan(ScanEvent::Looking(folder)) => self.scan_folder = Some(folder),
                ServiceEvent::Scan(ScanEvent::Covers { done, total }) => {
                    self.covers_progress = Some((done, total));
                    self.scan_folder = None;
                    // Shown as they come.
                    self.pictures_stale = true;
                }
                ServiceEvent::Scan(ScanEvent::Artists { done, total }) => {
                    self.artists_progress = Some((done, total));
                    self.scan_folder = None;
                    self.pictures_stale = true;
                }
                ServiceEvent::Scan(ScanEvent::Genres { done, total }) => {
                    self.genres_progress = Some((done, total));
                    self.scan_folder = None;
                    self.pictures_stale = true;
                }
                ServiceEvent::Scan(ScanEvent::Finished(summary)) => {
                    self.progress = None;
                    // A scan that found nothing new leaves the rows as they are.
                    self.stale |= summary.added + summary.updated + summary.removed > 0;
                }
                ServiceEvent::Scan(ScanEvent::Failed(_)) => self.progress = None,
                ServiceEvent::ScanEnded => {
                    self.progress = None;
                    self.scanning = false;
                    finished = true;
                }
                // Plays noted while the scan wrote.
                ServiceEvent::PlaysWritten => self.stale |= self.shows_plays(),
                ServiceEvent::PlaylistsChanged => self.playlists_changed = true,
                ServiceEvent::LastFm(status) => self.lastfm_status = Some(status),
            }
        }
        // Covers and pictures found so far show as they come; and what was said of the artist
        // on the page.
        if self.pictures_stale {
            self.load_pictures();
            self.show_artist_info();
        }
        if !self.scanning {
            self.scan_folder = None;
            self.covers_progress = None;
            self.artists_progress = None;
            self.genres_progress = None;
        }
        if finished {
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
        // While a scan writes, the play waits rather than the window.
        let written = self.server.as_ref().is_some_and(|server| {
            server
                .record_play(&path, part, SystemTime::now())
                .unwrap_or_else(|e| {
                    log::warn!("could not note a play of {}: {e}", path.display());
                    false
                })
        });
        if written && self.shows_plays() {
            self.reload();
        }
    }

    /// Whether the rows shown are made from plays.
    fn shows_plays(&self) -> bool {
        !self.searching() && matches!(self.level(), Level::RecentlyPlayed | Level::MostPlayed)
    }

    /// Writes the plays noted, waiting for a scan if one is running. For when Freya quits.
    pub fn save(&mut self) {
        if let Some(server) = &self.server
            && let Err(e) = server.save()
        {
            log::warn!("could not write the plays: {e}");
        }
    }

    /// Asks the internet what `artists` lack, every artist when none, once no scan runs:
    /// after the user matched them to another artist.
    fn fetch(&mut self, artists: Option<Vec<String>>) {
        if let Some(server) = &self.server
            && let Err(e) = server.fetch(artists.as_deref())
        {
            log::warn!("could not ask the server to look: {e}");
        }
    }

    /// The artist's MusicBrainz id as Freya knows it: the user's match, the tags', or what
    /// MusicBrainz's search found.
    fn known_mbid(&self, name: Option<&str>) -> Option<String> {
        name.and_then(|n| self.edits.get(n)?.mbid.clone())
            .or_else(|| self.found_mbid(name))
    }

    /// The artist's MusicBrainz id but for the user's: the tags', else what MusicBrainz's
    /// search found.
    fn found_mbid(&self, name: Option<&str>) -> Option<String> {
        let db = self.db.as_ref()?;
        db.artist_mbid(name)
            .ok()
            .flatten()
            .or_else(|| db.searched_artist_id(name?).ok()??.answer)
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
            // The app shows it.
            Item::Playlist(playlist) => {
                self.open_playlist = Some(playlist.name.clone());
                return None;
            }
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
        let before = self.album_page.take();
        let (Some(db), Level::AlbumTracks { artist, album }) = (&self.db, self.level().clone())
        else {
            return;
        };
        if self.searching() {
            return;
        }
        self.play_counts = db
            .album_plays(artist.as_deref(), album.as_deref())
            .map(|plays| plays.into_iter().collect())
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
        // Read already, while the same album shows.
        let cover_full = before
            .filter(|page| page.cover == cover)
            .and_then(|page| page.cover_full);
        self.album_page = Some(AlbumPage {
            title: album_name(album.as_deref()),
            artist: self.shown_artist(artist.as_deref()),
            detail: detail.join(" · "),
            track: first.path.clone(),
            cover,
            color: None,
            cover_full,
        });
        self.color_album_page();
    }

    /// The page of the artist whose albums are shown; for anything else, none.
    fn load_artist_page(&mut self) {
        let more = self.artist_page.as_ref().is_some_and(|p| p.more);
        self.artist_page = None;
        if self.searching() {
            self.popular_rows.sync(Vec::new());
            return;
        }
        if let Level::GenreAlbums { genre: Some(genre) } = self.level().clone() {
            self.load_genre_page(genre, more);
            self.show_artist_picture();
            self.show_artist_info();
            self.show_popular();
            return;
        }
        let (Some(db), Level::ArtistAlbums { artist }) = (&self.db, self.level().clone()) else {
            self.popular_rows.sync(Vec::new());
            return;
        };
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
        let picture = Thumbnails::artist_key(artist.as_deref());
        self.artist_keys.insert(picture.clone(), artist.clone());
        self.artist_page = Some(ArtistPage {
            genre: false,
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
            asked: None,
            color: None,
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
        let backdrop =
            page_backdrop(page, &self.backdrops, &self.edits, &self.genre_infos).cloned();
        // Their picture shows only without a wide one.
        let needs_portrait = !page.portrait_large && backdrop.is_none();
        // Made, or asked to be made, as for the grid.
        self.cover(&key);
        let Some(thumbnails) = &mut self.thumbnails else {
            return;
        };
        let banner = match needs_banner.then(|| thumbnails.get_backdrop(&key, backdrop.as_deref()))
        {
            Some(Thumbnail::Ready(file)) => Some(file),
            _ => None,
        };
        // Their picture large, else the grid's while it is made, or of a collage.
        let portrait = match thumbnails.artist_portrait(&key) {
            _ if !needs_portrait => None,
            Some(file) => Some((file, true)),
            None => thumbnails.ready(&key).map(|file| (file.to_owned(), false)),
        };
        let files = (banner, portrait);
        let Some(page) = &mut self.artist_page else {
            return;
        };
        if files == (None, None) || page.asked.as_ref() == Some(&files) {
            return;
        }
        page.asked = Some(files.clone());
        let made = self.page_pictures.0.clone();
        let started = std::thread::Builder::new()
            .name("freya-artist-page".into())
            .spawn(move || {
                let _ = made.send((key, PagePictures::make(&files)));
            });
        if let Err(e) = started {
            log::warn!("could not make the artist page's pictures: {e}");
        }
    }

    /// Shows the page's pictures a thread made, if they are of the artist shown still.
    fn take_page_pictures(&mut self) {
        while let Ok((key, made)) = self.page_pictures.1.try_recv() {
            let Some(made) = self.take_playlist_pictures(&key, made) else {
                continue;
            };
            let Some(page) = self.artist_page.as_mut().filter(|page| page.picture == key) else {
                continue;
            };
            if let Some((banner, color)) = made.banner {
                page.banner = Some(Image::from_rgb8(banner));
                page.color = color.or(page.color);
            }
            if let Some(portrait) = made.portrait {
                page.portrait = Some(Image::from_rgba8_premultiplied(portrait.round));
                page.portrait_large = portrait.large;
                page.picture_full = portrait.full.map(Image::from_rgb8);
                // Without a wide picture: the color of their picture.
                let wide =
                    page_backdrop(page, &self.backdrops, &self.edits, &self.genre_infos).is_some();
                if !wide {
                    page.color = Some(portrait.color);
                }
            }
        }
    }

    /// What is said of the artist on the page, as kept.
    fn show_artist_info(&mut self) {
        let Some(page) = &self.artist_page else {
            return;
        };
        let Some(name) = page.name.clone() else {
            return;
        };
        let ((summary, biography, tags, from), own) = if page.genre {
            let (summary, text, from, own) = self.genre_text(&name);
            ((summary, text, String::new(), from), own)
        } else {
            let own = self.edits.get(&name).is_some_and(|e| e.biography.is_some());
            (self.biography_of(&name), own)
        };
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
    /// before what Last.fm says; each as a scan kept it.
    fn biography_of(&self, name: &str) -> (String, String, String, String) {
        if let Some(own) = self.edits.get(name).and_then(|e| e.biography.clone()) {
            let summary = own.split("\n\n").next().unwrap_or(&own).trim().to_owned();
            return (summary, own, String::new(), String::new());
        }
        let Some(db) = &self.db else {
            return Default::default();
        };
        // Of the artist they are now: found by their id now.
        let mbid = self.known_mbid(Some(name));
        let article = db
            .artist_article(name)
            .ok()
            .flatten()
            .filter(|(id, _)| Some(id) == mbid.as_ref())
            .and_then(|(_, kept)| kept.answer);
        let info = db.artist_text(name).ok().flatten().map(|kept| kept.answer);
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

    /// The user's personal fanart.tv key, or none: with a new one, fanart.tv is asked for
    /// the pictures artists lack.
    pub fn set_fanart_key(&mut self, key: Option<String>) {
        if self.settings.fanart_key != key {
            self.settings.fanart_key = key;
            self.tell_settings();
        }
    }

    /// Whether a scan may ask Wikipedia for articles on artists: allowed, it is asked now for
    /// those who lack one.
    pub fn set_online_wikipedia(&mut self, online: bool) {
        self.settings.artist_wikipedia_online = online;
        self.tell_settings();
    }

    /// Whether a scan may ask Last.fm what it says of artists; as for Wikipedia.
    pub fn set_online_bios(&mut self, online: bool) {
        self.settings.artist_bios_online = online;
        self.tell_settings();
    }

    /// Takes the album page's color from its cover, once the cover's thumbnail is made.
    fn color_album_page(&mut self) {
        let Some(page) = &self.album_page else {
            return;
        };
        if page.color.is_some() {
            return;
        }
        // As the scan made it; no cover, or none made yet: grey, as for a cover of grey.
        let color = self
            .made_covers
            .get(&page.cover)
            .and_then(|made| made.color)
            .unwrap_or_else(|| freya_library::cover::page_color(&[]));
        if let Some(page) = &mut self.album_page {
            page.color = Some(color);
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
            let (_, biography, _, from) = self.biography_of(&name);
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
        if let Some(name) = self.artist_page.as_ref().and_then(|p| p.name.clone()) {
            let (request, picked) = self.picture_request(&name);
            self.picked_before.clone_from(&picked);
            self.picked = picked;
            self.finder.find(request);
        }
        self.show_choices();
    }

    /// What to look for to choose an artist's pictures from, and what is chosen now.
    fn picture_request(&self, name: &str) -> (Request, HashMap<PictureKind, Pick>) {
        let mut picked = HashMap::new();
        let mut kept = Vec::new();
        for kind in [PictureKind::Portrait, PictureKind::Backdrop] {
            let row = self
                .db
                .as_ref()
                .and_then(|db| db.artist_picture(name, kind).ok().flatten());
            let pick = match &row {
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
                    freya_protocol::FOLDER => "Folder",
                    _ => "Chosen before",
                };
                kept.push((kind, checksum, source.to_owned()));
            }
            picked.insert(kind, pick);
        }
        let request = Request {
            name: freya_protocol::plain_name(name).to_owned(),
            deezer: self.settings.artist_pictures_online,
            mbid: self.known_mbid(Some(name)),
            search: true,
            fanart_key: self.settings.fanart_key.clone(),
            kept,
            covers: self.artist_source(Some(name)).covers,
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
            .or_else(|| self.found_mbid(Some(&tagged)));
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
        // Its cover from the Cover Art Archive, when its music has none.
        self.fetch(None);
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
            self.match_finder
                .find(name, self.settings.fanart_key.clone());
        }
    }

    /// Matches the artist being matched to the one with the MusicBrainz id `mbid`; empty, or
    /// Freya's own match, lets Freya match them again.
    pub fn save_artist_match(&mut self, mbid: &str) {
        let Some(Some(artist)) = self.artist_match.clone() else {
            return;
        };
        let found = self.found_mbid(Some(&artist));
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
        let found = self.found_mbid(artist.as_deref());
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
            .find(query.to_owned(), self.settings.fanart_key.clone());
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
        let found_mbid = self.found_mbid(artist.as_deref()).unwrap_or_default();
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
                text(artist.map(|a| freya_protocol::plain_name(&a).to_owned())),
            ),
            ("name".to_owned(), text(edit.name)),
            ("sort-name".to_owned(), text(edit.sort_name)),
            ("mbid".to_owned(), text(edit.mbid)),
            ("found-mbid".to_owned(), string(found_mbid)),
            ("biography".to_owned(), string(biography)),
            (
                "header".to_owned(),
                string(edit.header.map_or("auto", Header::name)),
            ),
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
        // Shown by `reload`, which reads the pictures the artist has now.
        if self.keep_picked(&artist) {
            self.pictures_stale = true;
        }
        self.choices.clear();
        self.show_choices();
        if kept.mbid != before.mbid {
            self.forget_found(&artist);
        }
        // The top of their page shows the other picture: made again.
        if kept.header != before.header
            && let Some(page) = &mut self.artist_page
        {
            page.banner = None;
            page.portrait_large = false;
            page.color = None;
            page.asked = None;
        }
        self.reload();
    }

    /// Forgets what the internet gave for `artist`, and asks it again: after they were
    /// matched to another artist of the name. What the user chose, and the pictures in their
    /// folder, stay.
    fn forget_found(&mut self, artist: &str) {
        if let Some(db) = &self.db {
            for kind in [PictureKind::Portrait, PictureKind::Backdrop] {
                let found = db
                    .artist_picture(artist, kind)
                    .ok()
                    .flatten()
                    .is_some_and(|row| row.from_internet());
                if found && let Err(e) = db.forget_artist_picture(artist, kind) {
                    log::warn!("could not forget the pictures of {artist}: {e}");
                }
            }
            if let Err(e) = db.forget_artist_text(artist) {
                log::warn!("could not forget what Last.fm said of {artist}: {e}");
            }
        }
        self.pictures_stale = true;
        self.fetch(Some(vec![artist.to_owned()]));
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
        self.settings.artist_pictures_online = online;
        self.tell_settings();
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
            // The app has its tracks.
            Some(Item::Playlist(_)) | None => Ok(Vec::new()),
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
        self.load_pictures();
        self.reloaded = Instant::now();
        let Some(db) = &self.db else {
            return;
        };
        let started = Instant::now();
        self.edits = db.artist_edits().unwrap_or_else(|e| {
            log::warn!("could not read what was changed about artists: {e}");
            HashMap::new()
        });
        self.favorites = db
            .album_favorites()
            .map(|f| f.into_iter().collect())
            .unwrap_or_else(|e| {
                log::warn!("could not read the favourite albums: {e}");
                HashMap::new()
            });
        self.artist_favorites = db.artist_favorites().unwrap_or_default();
        self.playlist_favorites = db.playlist_favorites().unwrap_or_default();
        let result = if self.searching() {
            Ok(self.search_items())
        } else {
            self.search_section_rows.sync(Vec::new());
            self.search_top = None;
            self.search_songs = None;
            let Some(db) = &self.db else {
                return;
            };
            self.load(db.as_ref())
        };
        let Some(db) = &self.db else {
            return;
        };
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
        self.artist_keys.clear();
        self.cover_order.clear();
        for item in &self.items {
            match item {
                Item::Artist(artist) => {
                    let key = Thumbnails::artist_key(artist.name.as_deref());
                    self.artist_keys.insert(key.clone(), artist.name.clone());
                    self.cover_order.push(key);
                }
                Item::Album(album) => self.cover_order.push(cover_key(album)),
                Item::Playlist(playlist) => {
                    self.cover_order.push(Self::playlist_key(&playlist.name))
                }
                Item::Genre(genre) => {
                    let key = Self::genre_key(genre.name.as_deref());
                    self.genre_keys.insert(key.clone(), genre.name.clone());
                    self.cover_order.push(key);
                }
                _ => {}
            }
        }
        self.cover_rows = (self.cover_order.iter().cloned())
            .enumerate()
            .map(|(i, key)| (key, i))
            .collect();
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
        for (row, item) in rows.iter_mut().zip(&self.items) {
            let favorite = match item {
                Item::Album(album) => {
                    self.is_favorite(album.artist.as_deref(), album.name.as_deref())
                }
                Item::Artist(artist) => artist
                    .name
                    .as_ref()
                    .is_some_and(|n| self.artist_favorites.contains_key(n)),
                Item::Playlist(playlist) => self.playlist_favorites.contains_key(&playlist.name),
                _ => false,
            };
            if let Value::Struct(row) = row
                && favorite
            {
                row.set_field("favorite".into(), Value::Bool(true));
            }
        }
        self.rows.sync(rows);
        self.letter_rows.sync(self.letters());
        self.find_playing_row();
        log::debug!("library rows loaded in {:?}", started.elapsed());
    }

    /// The letters to jump to in the list shown, when it is in the order of its names: each
    /// with the first row it starts, or the next letter's when none does.
    fn letters(&self) -> Vec<Value> {
        let alphabetical = !self.searching()
            && match self.level() {
                Level::Artists => self.artist_order == ArtistOrder::Name,
                Level::Albums | Level::Genres => true,
                _ => false,
            };
        if !alphabetical {
            return Vec::new();
        }
        let names = self.items.iter().map(|item| match item {
            // As the library sorts them: by the name the user sorts them by, else the one shown.
            Item::Artist(artist) => artist
                .name
                .as_deref()
                .and_then(|n| {
                    let edit = self.edits.get(n)?;
                    edit.sort_name.as_deref().or(edit.name.as_deref())
                })
                .or(artist.name.as_deref())
                .map(plain_name),
            Item::Album(album) => album.name.as_deref(),
            Item::Genre(genre) => genre.name.as_deref(),
            _ => None,
        });
        letter_jumps(names)
            .into_iter()
            .map(|(letter, row, present)| {
                Value::Struct(Struct::from_iter([
                    ("letter".to_owned(), string(letter)),
                    ("row".to_owned(), Value::Number(row as f64)),
                    ("present".to_owned(), Value::Bool(present)),
                ]))
            })
            .collect()
    }

    fn load(&self, db: &dyn LibraryApi) -> freya_protocol::ApiResult<Vec<Item>> {
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
            Level::Albums => albums(db.albums_by(self.albums_order)?),
            Level::Songs => tracks(db.all_tracks()?),
            Level::Genres => db.genres()?.into_iter().map(Item::Genre).collect(),
            Level::Playlists => self.playlist_items(),
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
            let subtitle = match self.search_filter {
                SearchFilter::All => String::new(),
                SearchFilter::Songs if n >= SEARCH_LIMIT => {
                    format!("The first {} matching tracks", thousands(n))
                }
                SearchFilter::Songs => count(n, "matching track"),
                _ => count(n, "match"),
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
            Level::Playlists => ("Playlists".to_owned(), count(n, "playlist")),
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
        if let Some(&row) = self.cover_rows.get(key) {
            self.covers_asked = Some(match self.covers_asked {
                Some((first, last)) => (first.min(row), last.max(row)),
                None => (row, row),
            });
        }
        if let Some(image) = self.recent_covers.get(key) {
            return image;
        }
        // Shown once decoded, soon.
        if self.decoder.is_decoding(key) {
            self.covers_missed = true;
            return Image::default();
        }
        let Thumbnail::Ready(file) = self.cover_state(key) else {
            return Image::default();
        };
        // Not decoded ahead: scrolled to faster than the threads went, or the first screen.
        self.decoder.decode(key, file);
        self.covers_missed = true;
        Image::default()
    }

    /// Whether the cover for `key` is made, or being made, or none: made, or asked to be
    /// made, as soon as it is asked for.
    fn cover_state(&mut self, key: &str) -> Thumbnail {
        // Known already, made or not: asked for again on every scroll and every new picture,
        // without the library being read for it.
        if let Some(known) = self.thumbnails.as_ref().and_then(|t| t.known(key)) {
            return known;
        }
        // A genre's picture, or its albums' covers.
        if let Some(name) = self.genre_keys.get(key).cloned() {
            let source = self.genre_source(name.as_deref());
            return match &mut self.thumbnails {
                Some(thumbnails) => thumbnails.get_artist(key, &source),
                None => Thumbnail::None,
            };
        }
        // A playlist's picture, or its covers.
        if let Some(source) = self.playlist_sources.get(key) {
            return match &mut self.thumbnails {
                Some(thumbnails) => thumbnails.get_artist(key, source),
                None => Thumbnail::None,
            };
        }
        match (&self.db, self.artist_keys.get(key)) {
            // An artist's picture.
            (Some(_), Some(name)) => {
                let source = self.artist_source(name.as_deref());
                match &mut self.thumbnails {
                    Some(thumbnails) => thumbnails.get_artist(key, &source),
                    None => Thumbnail::None,
                }
            }
            // An album's cover, as the scan made it; none made yet shows none.
            _ => {
                let (Some(thumbnails), Some(checksum)) = (
                    &mut self.thumbnails,
                    self.made_covers
                        .get(key)
                        .and_then(|made| made.cover.as_deref()),
                ) else {
                    return Thumbnail::None;
                };
                thumbnails.get_picture(checksum)
            }
        }
    }

    /// Takes in the covers decoded, and decodes those around the ones on screen ahead of
    /// their being scrolled to. Returns whether one the skin asked for came.
    fn decode_covers(&mut self) -> bool {
        let mut came = false;
        for (key, decoded) in self.decoder.take() {
            let image = decoded.map(Image::from_rgb8).unwrap_or_default();
            self.recent_covers.put(&key, image);
            came = true;
        }
        if let Some(asked) = self.covers_asked.take() {
            self.covers_shown = asked;
        }
        let (first, last) = self.covers_shown;
        // Those on screen first, then outwards both ways.
        let around = (1..=DECODE_AHEAD)
            .flat_map(|d| [last.checked_add(d), first.checked_sub(d)])
            .flatten();
        let mut handed = 0;
        for row in (first..=last).chain(around) {
            if handed == DECODE_AT_ONCE {
                break;
            }
            let Some(key) = self.cover_order.get(row).cloned() else {
                continue;
            };
            if self.recent_covers.images.contains_key(&key) || self.decoder.is_decoding(&key) {
                continue;
            }
            if let Thumbnail::Ready(file) = self.cover_state(&key) {
                self.decoder.decode(&key, file);
                handed += 1;
            }
        }
        came && std::mem::take(&mut self.covers_missed)
    }

    /// Reads the albums' covers and the artists' pictures from the library, when a scan or the
    /// user changed them; what was shown of those that changed is made and read anew.
    fn load_pictures(&mut self) {
        if !std::mem::take(&mut self.pictures_stale) {
            return;
        }
        let Some(db) = &self.db else {
            return;
        };
        let covers = db.album_covers().unwrap_or_else(|e| {
            log::warn!("could not read the albums' covers: {e}");
            HashMap::new()
        });
        self.load_genre_infos();
        let Some(db) = &self.db else {
            return;
        };
        for (key, made) in &covers {
            if self.made_covers.get(key).map(|old| &old.cover) != Some(&made.cover) {
                self.recent_covers.remove(key);
            }
        }
        self.made_covers = covers;
        let read = |kind| {
            db.artist_pictures(kind).unwrap_or_else(|e| {
                log::warn!("could not read the artists' pictures: {e}");
                HashMap::new()
            })
        };
        let (portraits, backdrops) = (read(PictureKind::Portrait), read(PictureKind::Backdrop));
        let changed: Vec<String> = portraits
            .iter()
            .filter(|(artist, picture)| self.portraits.get(*artist) != Some(*picture))
            .chain(
                backdrops
                    .iter()
                    .filter(|(artist, picture)| self.backdrops.get(*artist) != Some(*picture)),
            )
            .map(|(artist, _)| artist.clone())
            .chain(
                self.portraits
                    .keys()
                    .filter(|a| !portraits.contains_key(*a))
                    .cloned(),
            )
            .chain(
                self.backdrops
                    .keys()
                    .filter(|a| !backdrops.contains_key(*a))
                    .cloned(),
            )
            .collect();
        self.portraits = portraits;
        self.backdrops = backdrops;
        let shown = self.artist_page.as_ref().and_then(|p| p.name.clone());
        for artist in changed {
            let key = Thumbnails::artist_key(Some(&artist));
            if let Some(thumbnails) = &mut self.thumbnails {
                thumbnails.forget_artist(&key);
            }
            self.recent_covers.remove(&key);
            if shown.as_ref() == Some(&artist)
                && let Some(page) = &mut self.artist_page
            {
                page.banner = None;
                page.portrait_large = false;
                page.color = None;
                page.asked = None;
            }
        }
        self.covers_ready += 1;
        self.color_album_page();
        self.show_artist_picture();
        self.show_playlist_picture();
    }

    /// What is kept of the artist `name`'s picture: theirs, and their albums' covers.
    fn artist_source(&self, name: Option<&str>) -> ArtistSource {
        let covers = self
            .db
            .as_ref()
            .and_then(|db| db.albums(name).ok())
            .unwrap_or_default()
            .iter()
            .filter_map(|album| self.made_covers.get(&cover_key(album))?.cover.clone())
            .collect();
        ArtistSource {
            picture: name.and_then(|name| self.portraits.get(name)).cloned(),
            covers,
        }
    }

    /// Lines up every artist's and album's picture to be made ahead of its being shown.
    fn prepare_all(&mut self) {
        self.pictures_stale = true;
        self.load_pictures();
        let Some(db) = &self.db else {
            return;
        };
        self.prepare.clear();
        if let Ok(artists) = db.artists() {
            self.prepare
                .extend(artists.into_iter().map(|a| Prepare::Artist(a.name)));
        }
        let covers = self
            .made_covers
            .values()
            .filter_map(|made| made.cover.clone())
            .map(Prepare::Cover);
        self.prepare.extend(covers);
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
                Prepare::Cover(checksum) => {
                    if let Some(thumbnails) = &mut self.thumbnails {
                        thumbnails.prepare_picture(&checksum);
                    }
                }
            }
        }
    }

    /// What the scan running is doing, and how far it is.
    fn scan_phase(&self) -> Option<String> {
        if !self.scanning {
            return None;
        }
        if let Some((done, total)) = self.genres_progress {
            return Some(format!(
                "Asking the internet about genres: {} of {}",
                thousands(done),
                thousands(total)
            ));
        }
        Some(
            match (self.artists_progress, self.covers_progress, self.progress) {
                (Some((done, total)), ..) => format!(
                    "Asking the internet about artists: {} of {}",
                    thousands(done),
                    thousands(total)
                ),
                (None, Some((done, total)), _) => format!(
                    "Making album covers: {} of {}",
                    thousands(done),
                    thousands(total)
                ),
                (None, None, Some((done, total))) => {
                    format!("Reading {} of {} files", thousands(done), thousands(total))
                }
                (None, None, None) => "Checking the music folders for changes".to_owned(),
            },
        )
    }

    /// What the library is doing in the background, for the user to see: what, and of what.
    pub fn activities(&self) -> Vec<(String, String)> {
        let mut activities = Vec::new();
        if let Some(what) = self.scan_phase() {
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
        let status = self
            .scan_phase()
            .unwrap_or_else(|| self.stats.clone().unwrap_or_default());
        let layout = if self.searching() {
            match self.search_filter {
                SearchFilter::All => "search",
                SearchFilter::Songs => "table",
                _ => "grid",
            }
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
            ("search-shown", Value::Bool(self.searching())),
            ("search-filter", string(self.search_filter.name())),
            (
                "search-songs",
                Value::Struct(Struct::from_iter([
                    ("kind".to_owned(), string("songs")),
                    ("title".to_owned(), string("Songs")),
                    (
                        "first".to_owned(),
                        Value::Number(self.search_songs.map_or(0.0, |(f, _)| f as f64)),
                    ),
                    (
                        "count".to_owned(),
                        Value::Number(self.search_songs.map_or(0.0, |(_, c)| c as f64)),
                    ),
                ])),
            ),
            (
                "search-top",
                Value::Number(self.search_top.map_or(-1.0, |row| row as f64)),
            ),
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
            (
                "albums-options",
                Value::Bool(!self.searching() && *self.level() == Level::Albums),
            ),
            ("albums-sort", string(albums_order_name(self.albums_order))),
            (
                "playlists-options",
                Value::Bool(!self.searching() && *self.level() == Level::Playlists),
            ),
            (
                "playlists-sort",
                string(if self.playlists_by_name {
                    "name"
                } else {
                    "added"
                }),
            ),
            ("playing-row", Value::Number(f64::from(self.playing_row))),
            ("playing-known", Value::Bool(self.playing_album.is_some())),
            ("reveal-row", Value::Number(f64::from(self.reveal_row))),
            ("reveals", Value::Number(f64::from(self.reveals))),
            ("album-page", self.album_page_value()),
            ("artist-page", self.artist_page_value()),
            (
                "artist-bios-online",
                Value::Bool(self.settings.artist_bios_online),
            ),
            (
                "artist-wikipedia-online",
                Value::Bool(self.settings.artist_wikipedia_online),
            ),
            (
                "fanart-key",
                string(self.settings.fanart_key.clone().unwrap_or_default()),
            ),
            ("artist-draft", self.draft_value()),
            ("genre-draft", self.genre_draft_value()),
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
            (
                "artist-pictures-online",
                Value::Bool(self.settings.artist_pictures_online),
            ),
            ("watch-folders", Value::Bool(self.settings.watch_folders)),
            (
                "scan-every-hours",
                Value::Number(f64::from(self.settings.scan_every_hours)),
            ),
            ("scan-at-start", Value::Bool(self.settings.scan_at_start)),
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
            (
                "kind".to_owned(),
                string(if page.is_some_and(|p| p.genre) {
                    "genre"
                } else {
                    "artist"
                }),
            ),
            ("name".to_owned(), text(|p| p.shown_name.clone())),
            (
                "favorite".to_owned(),
                Value::Bool(page.is_some_and(|p| {
                    !p.genre
                        && p.name
                            .as_ref()
                            .is_some_and(|n| self.artist_favorites.contains_key(n))
                })),
            ),
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
                "has-banner".to_owned(),
                Value::Bool(page.is_some_and(|p| {
                    p.banner.is_some()
                        || page_backdrop(p, &self.backdrops, &self.edits, &self.genre_infos)
                            .is_some()
                })),
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
            cover_full: None,
            track: PathBuf::new(),
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
            ("favorite".to_owned(), Value::Bool(self.page_favorite())),
            (
                "picture-full".to_owned(),
                Value::Image(page.cover_full.unwrap_or_default()),
            ),
        ]))
    }

    /// Reads the cover of the album whose page is shown whole, from its music, on a thread:
    /// the user asked to see it.
    pub fn show_album_cover(&mut self) {
        let Some(page) = self.album_page.as_ref().filter(|p| p.cover_full.is_none()) else {
            return;
        };
        let Some(api) = self.db.clone() else {
            return;
        };
        let (key, track) = (page.cover.clone(), page.track.clone());
        let read = self.full_covers.0.clone();
        let started = std::thread::Builder::new()
            .name("freya-album-cover".into())
            .spawn(move || {
                let picture = api
                    .album_art(&track, COVER_FULL)
                    .ok()
                    .flatten()
                    .and_then(|bytes| crate::gui::decode_picture(&bytes).ok());
                if let Some(picture) = picture {
                    let rgb = picture.to_rgb8();
                    let buffer = slint::SharedPixelBuffer::clone_from_slice(
                        rgb.as_raw(),
                        rgb.width(),
                        rgb.height(),
                    );
                    let _ = read.send((key, buffer));
                }
            });
        if let Err(e) = started {
            log::warn!("could not read the album's cover: {e}");
        }
    }

    /// Shows an album's cover whole, once read, if its page shows still.
    fn take_full_covers(&mut self) {
        while let Ok((key, buffer)) = self.full_covers.1.try_recv() {
            if let Some(page) = self.album_page.as_mut().filter(|p| p.cover == key) {
                page.cover_full = Some(Image::from_rgb8(buffer));
            }
        }
    }

    /// Whether the album `artist` / `album` is a favourite.
    fn is_favorite(&self, artist: Option<&str>, album: Option<&str>) -> bool {
        self.favorites
            .contains_key(&(artist.map(str::to_owned), album.map(str::to_owned)))
    }

    /// Whether the album whose page is shown is a favourite.
    fn page_favorite(&self) -> bool {
        match self.level() {
            Level::AlbumTracks { artist, album } => {
                self.is_favorite(artist.as_deref(), album.as_deref())
            }
            _ => false,
        }
    }

    /// Makes the album `artist` / `album` a favourite, or not one any longer.
    fn set_favorite(&mut self, artist: Option<String>, album: Option<String>) {
        let Some(db) = &self.db else {
            return;
        };
        let favorite = !self.is_favorite(artist.as_deref(), album.as_deref());
        let kept = db.set_album_favorite(
            artist.as_deref(),
            album.as_deref(),
            favorite,
            SystemTime::now(),
        );
        if let Err(e) = kept {
            log::warn!("could not keep the favourite: {e}");
        }
        self.reload();
    }

    /// Makes the album, artist or playlist in the `row`th row a favourite, or not one any
    /// longer.
    pub fn toggle_favorite(&mut self, row: usize) {
        match self.items.get(row).cloned() {
            Some(Item::Album(album)) => self.set_favorite(album.artist, album.name),
            Some(Item::Artist(Artist {
                name: Some(name), ..
            })) => self.set_artist_favorite(&name),
            Some(Item::Playlist(playlist)) => self.set_playlist_favorite(&playlist.name),
            _ => {}
        }
    }

    /// Makes the album or artist whose page is shown a favourite, or not one any longer.
    pub fn toggle_page_favorite(&mut self) {
        match self.level().clone() {
            Level::AlbumTracks { artist, album } => self.set_favorite(artist, album),
            Level::ArtistAlbums {
                artist: Some(artist),
            } => self.set_artist_favorite(&artist),
            _ => {}
        }
    }

    /// Makes the artist `name` a favourite, or not one any longer.
    fn set_artist_favorite(&mut self, name: &str) {
        let favorite = !self.artist_favorites.contains_key(name);
        if let Some(db) = &self.db
            && let Err(e) = db.set_artist_favorite(name, favorite, SystemTime::now())
        {
            log::warn!("could not keep the favourite: {e}");
        }
        self.reload();
    }

    /// Makes the playlist `name` a favourite, or not one any longer.
    pub fn set_playlist_favorite(&mut self, name: &str) {
        let favorite = !self.playlist_favorites.contains_key(name);
        if let Some(db) = &self.db
            && let Err(e) = db.set_playlist_favorite(name, favorite, SystemTime::now())
        {
            log::warn!("could not keep the favourite: {e}");
        }
        self.reload();
    }

    /// Whether the playlist `name` is a favourite.
    pub fn is_playlist_favorite(&self, name: &str) -> bool {
        self.playlist_favorites.contains_key(name)
    }

    /// The playlists' section: every playlist, by when it was made or made a favourite, the
    /// latest first; or by name.
    fn playlist_items(&self) -> Vec<Item> {
        let mut playlists = self.found_playlists.clone();
        if self.playlists_by_name {
            playlists.sort_by_key(|p| p.name.to_lowercase());
        } else {
            playlists.sort_by_key(|p| {
                std::cmp::Reverse(self.playlist_favorites.get(&p.name).copied().or(p.created))
            });
        }
        playlists.into_iter().map(Item::Playlist).collect()
    }

    /// Orders the playlists: "added" or "name".
    pub fn set_playlists_order(&mut self, name: &str) {
        let by_name = name == "name";
        if by_name != self.playlists_by_name {
            self.playlists_by_name = by_name;
            self.reload();
        }
    }

    /// Orders the albums: "added", "played", "name" or "artist".
    pub fn set_albums_order(&mut self, name: &str) {
        if let Some(order) = albums_order(name)
            && order != self.albums_order
        {
            self.albums_order = order;
            self.reload();
        }
    }
}

/// An albums order by its name in the skin API and settings.
pub fn albums_order(name: &str) -> Option<AlbumOrder> {
    Some(match name {
        "added" => AlbumOrder::Added,
        "played" => AlbumOrder::Played,
        "name" => AlbumOrder::Name,
        "artist" => AlbumOrder::Artist,
        _ => return None,
    })
}

fn albums_order_name(order: AlbumOrder) -> &'static str {
    match order {
        AlbumOrder::Added => "added",
        AlbumOrder::Played => "played",
        AlbumOrder::Name => "name",
        AlbumOrder::Artist => "artist",
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
        Item::Playlist(p) => (
            "playlist",
            {
                cover = LibraryPanel::playlist_key(&p.name);
                p.name.clone()
            },
            format!("Playlist · {}", count(p.tracks, "track")),
        ),
        Item::Genre(g) => (
            "genre",
            {
                cover = LibraryPanel::genre_key(g.name.as_deref());
                genre_name(g.name.as_deref())
            },
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
        ("favorite".to_owned(), Value::Bool(false)),
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
/// The wide picture across the top of `page`: an artist's or a genre's.
fn page_backdrop<'a>(
    page: &ArtistPage,
    backdrops: &'a HashMap<String, String>,
    edits: &HashMap<String, ArtistEdit>,
    genre_infos: &'a HashMap<String, freya_protocol::GenreInfo>,
) -> Option<&'a String> {
    let name = page.name.as_deref()?;
    if page.genre {
        return genre_infos.get(&name.to_lowercase())?.backdrop.as_ref();
    }
    shown_backdrop(backdrops, edits, name)
}

/// The wide picture across the top of the artist `name`'s page: theirs, unless the user
/// wants their picture there.
fn shown_backdrop<'a>(
    backdrops: &'a HashMap<String, String>,
    edits: &HashMap<String, ArtistEdit>,
    name: &str,
) -> Option<&'a String> {
    let picture = edits
        .get(name)
        .is_some_and(|edit| edit.header == Some(Header::Picture));
    backdrops.get(name).filter(|_| !picture)
}

/// The letter a name is jumped to by: its first letter, A to Z, or another one as it is, such
/// as Å; "#" for one that starts with a digit or a sign.
fn letter_of(name: &str) -> String {
    match name.chars().find(|c| !c.is_whitespace()) {
        Some(c) if c.is_alphabetic() => c.to_uppercase().collect(),
        _ => "#".to_owned(),
    }
}

/// The letters to jump to in a list of `names` in order: "#" and A to Z always, then other
/// letters the names start with, in the order they come. Each with the first row it starts,
/// else the next letter's (the last row after the last), and whether any name starts with it.
fn letter_jumps<'a>(names: impl Iterator<Item = Option<&'a str>>) -> Vec<(String, usize, bool)> {
    let mut first: Vec<(String, usize)> = Vec::new();
    let mut count = 0;
    for (row, name) in names.enumerate() {
        count = row + 1;
        let letter = letter_of(name.unwrap_or(""));
        if !first.iter().any(|(l, _)| *l == letter) {
            first.push((letter, row));
        }
    }
    if count == 0 {
        return Vec::new();
    }
    let mut letters: Vec<String> = std::iter::once("#".to_owned())
        .chain(('A'..='Z').map(String::from))
        .collect();
    for (letter, _) in &first {
        if !letters.contains(letter) {
            letters.push(letter.clone());
        }
    }
    let row_of = |letter: &str| first.iter().find(|(l, _)| l == letter).map(|(_, r)| *r);
    let mut jumps: Vec<(String, usize, bool)> = Vec::new();
    // From the last: an absent letter jumps where the next present one starts.
    let mut next = count - 1;
    for letter in letters.into_iter().rev() {
        let row = row_of(&letter);
        if let Some(row) = row {
            next = row;
        }
        jumps.push((letter, next, row.is_some()));
    }
    jumps.reverse();
    jumps
}

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
    name.map_or("Unknown artist", freya_protocol::plain_name)
        .to_owned()
}

fn album_name(name: Option<&str>) -> String {
    name.unwrap_or("Unknown album").to_owned()
}

fn genre_name(name: Option<&str>) -> String {
    name.unwrap_or("Unknown genre").to_owned()
}

fn stats_text(stats: &freya_protocol::Stats) -> String {
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
    use freya_library::Library;
    use freya_library::pictures::Pictures;

    use super::*;

    /// `db` as a library to show, with its pictures in `pictures`.
    pub(super) fn local(db: Library, pictures: Option<PathBuf>) -> Arc<dyn LibraryApi> {
        Arc::new(freya_server::LocalLibrary::new(
            db,
            pictures.map(Pictures::new),
        ))
    }

    #[test]
    fn shows_what_a_scan_kept_of_an_artist() {
        let mut panel = LibraryPanel::new(None, None);
        let db = Library::open_in_memory().unwrap();
        db.set_artist_picture(
            "Shining",
            PictureKind::Portrait,
            "deezer",
            Some("square"),
            SystemTime::now(),
        )
        .unwrap();
        db.set_artist_picture(
            "Shining",
            PictureKind::Backdrop,
            "fanart.tv",
            None,
            SystemTime::now(),
        )
        .unwrap();
        db.set_artist_edit(
            "Shining",
            &ArtistEdit {
                mbid: Some("swedish".into()),
                ..ArtistEdit::default()
            },
        )
        .unwrap();
        let article = freya_protocol::Article {
            title: "Shining (Swedish band)".into(),
            summary: "A Swedish band.".into(),
            biography: "A Swedish band.".into(),
            url: String::new(),
        };
        db.set_artist_article("Shining", "swedish", Some(&article), SystemTime::now())
            .unwrap();
        panel.open(local(db, None));
        panel.reload();
        let source = panel.artist_source(Some("Shining"));
        assert_eq!(source.picture.as_deref(), Some("square"));
        assert!(panel.backdrops.is_empty(), "fanart.tv had none");
        let (summary, _, _, from) = panel.biography_of("Shining");
        assert_eq!(
            (summary.as_str(), from.as_str()),
            ("A Swedish band.", "Wikipedia")
        );

        // Matched to another artist of the name: not theirs.
        let db = panel.db.as_ref().unwrap();
        db.set_artist_edit(
            "Shining",
            &ArtistEdit {
                mbid: Some("norwegian".into()),
                ..ArtistEdit::default()
            },
        )
        .unwrap();
        panel.reload();
        assert_eq!(panel.biography_of("Shining").3, "");
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
        let mut panel = LibraryPanel::new(None, None);
        panel.open(local(db, None));
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

        let mut panel = LibraryPanel::new(None, None);
        panel.open(local(db, None));
        panel.settings.artist_bios_online = false;
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
            panel.known_mbid(page.name.as_deref()).as_deref(),
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
        let mut panel = LibraryPanel::new(None, None);
        panel.open(local(db, None));
        panel.settings.artist_bios_online = false;
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
        assert_eq!(
            panel.known_mbid(page.name.as_deref()).as_deref(),
            Some("own-id")
        );
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
        let options = freya_library::library::ScanOptions {
            pictures: Some(Pictures::new(dir.join("pictures"))),
            ..freya_library::library::ScanOptions::default()
        };
        freya_library::library::scan_with(&mut db, &[dir.join("music")], &options, &cancel, |_| {})
            .unwrap();
        let folder = db
            .artist_picture("Freya", PictureKind::Backdrop)
            .unwrap()
            .and_then(|row| row.picture)
            .expect("the folder's, taken in by the scan");
        let mut panel = LibraryPanel::new(None, None);
        panel.open(local(db, Some(dir.join("pictures"))));
        panel.settings.artist_pictures_online = false;
        panel.settings.artist_bios_online = false;
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
        // The folder's wide picture, as the scan took it in; none square.
        assert_eq!(panel.picked[&PictureKind::Backdrop], Pick::Kept(folder));
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
        // Shown at once, not only after Freya starts again.
        assert_eq!(panel.artist_source(Some("Freya")).picture, square.picture);
        assert!(!panel.backdrops.contains_key("Freya"));

        // Chosen: it stays.
        panel.edit_artist();
        finish(&mut panel);
        assert_eq!(panel.picked[&PictureKind::Backdrop], Pick::Nothing);
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
        let mut panel = LibraryPanel::new(None, None);
        panel.open(local(db, None));
        panel.settings.artist_bios_online = false;
        let candidate = |mbid: &str| freya_protocol::AlbumCandidate {
            release_group: format!("rg-{mbid}"),
            title: "Sines".into(),
            artist: freya_protocol::ArtistMatch {
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
        let mut panel = LibraryPanel::new(None, None);
        panel.open(local(db, None));
        panel.settings.artist_bios_online = false;
        panel.opened = vec![
            Level::Artists,
            Level::ArtistAlbums {
                artist: Some("Freya".into()),
            },
        ];
        panel.reload();
        panel.edit_artist();
        panel.matches = vec![freya_protocol::ArtistMatch {
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
    fn tells_what_sets_an_artist_apart() {
        let artist = |kind: &str, area: &str, begin: &str, end: &str| freya_protocol::ArtistMatch {
            kind: kind.into(),
            area: area.into(),
            begin: begin.into(),
            end: end.into(),
            ..Default::default()
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
    fn an_artists_page_shows_the_picture_the_user_wants_at_its_top() {
        let backdrops = HashMap::from([("Dido".to_owned(), "wide".to_owned())]);
        let mut edits = HashMap::new();
        assert_eq!(
            shown_backdrop(&backdrops, &edits, "Dido").map(String::as_str),
            Some("wide")
        );
        for (header, shown) in [(Header::Wide, Some("wide")), (Header::Picture, None)] {
            edits.insert(
                "Dido".to_owned(),
                ArtistEdit {
                    header: Some(header),
                    ..ArtistEdit::default()
                },
            );
            assert_eq!(
                shown_backdrop(&backdrops, &edits, "Dido").map(String::as_str),
                shown
            );
        }
        assert_eq!(shown_backdrop(&backdrops, &edits, "Air"), None);
    }

    #[test]
    fn an_album_is_made_a_favourite_and_ordered_as_the_user_picks() {
        let dir = std::env::temp_dir().join(format!("freya-favourite-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/freya-audio/tests/fixtures");
        std::fs::copy(fixtures.join("sine.mp3"), dir.join("sine.mp3")).unwrap();
        let mut db = Library::open_in_memory().unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        freya_library::library::scan(&mut db, std::slice::from_ref(&dir), &cancel, |_| {}).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let mut panel = LibraryPanel::new(None, None);
        panel.open(local(db, None));
        panel.opened = vec![Level::Albums];
        panel.reload();
        let favorite = |panel: &LibraryPanel| {
            let Value::Model(rows) = panel.rows.model_value() else {
                panic!("a model");
            };
            match slint::Model::row_data(&rows, 0) {
                Some(Value::Struct(row)) => row.get_field("favorite") == Some(&Value::Bool(true)),
                _ => false,
            }
        };
        assert!(!favorite(&panel));
        panel.toggle_favorite(0);
        assert!(favorite(&panel), "shown at once");
        assert_eq!(panel.favorites.len(), 1);
        panel.toggle_favorite(0);
        assert!(!favorite(&panel));

        panel.set_albums_order("played");
        assert_eq!(panel.albums_order, AlbumOrder::Played);
        panel.set_albums_order("nonsense");
        assert_eq!(panel.albums_order, AlbumOrder::Played);
    }

    #[test]
    fn jumps_to_the_first_row_of_each_letter() {
        let names = ["2Pac", "ABBA", "Air", "Cream", "Ólafur Arnalds", "Åsa"];
        let jumps = letter_jumps(names.iter().map(|n| Some(*n)));
        let at = |letter: &str| {
            let (_, row, present) = jumps.iter().find(|(l, ..)| l == letter).unwrap();
            (*row, *present)
        };
        assert_eq!(at("#"), (0, true));
        assert_eq!(at("A"), (1, true));
        assert_eq!(at("B"), (3, false), "absent: where C starts");
        assert_eq!(at("C"), (3, true));
        assert_eq!(at("Z"), (4, false), "absent: where the next letter starts");
        assert_eq!(at("Ó"), (4, true));
        assert_eq!(at("Å"), (5, true));
        assert_eq!(jumps.len(), 29);
        assert!(letter_jumps(std::iter::empty()).is_empty());
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
        let mut panel = LibraryPanel::new(None, None);
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
        let mut panel = LibraryPanel::new(None, None);
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

        let mut panel = LibraryPanel::new(None, None);
        panel.open(local(db, None));
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

        let mut panel = LibraryPanel::new(None, None);
        panel.open(local(db, None));
        panel.settings.artist_bios_online = false;
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
        let mut panel = LibraryPanel::new(None, None);
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
        assert_eq!(Level::Genres.layout(), "grid");
        assert_eq!(Level::RecentlyAdded.layout(), "grid");
        assert_eq!(Level::MostPlayed.layout(), "table");
    }
}
