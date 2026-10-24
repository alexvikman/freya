//! The library as the skin shows it: browsing by artist, album, song and genre, what was added
//! and played lately, searching, and the scans that keep it up to date.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};

use freya_media::Entry;
use freya_media::thumbnails::{ArtistSource, Thumbnail, Thumbnails};
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

mod album_page;
mod artist_page;
mod connection;
mod editing;
mod favorites;
mod genre_page;
mod loading;
mod navigation;
mod pictures;
mod playlist_page;
mod scans;
mod search_page;
mod text;
mod values;
use super::match_finder::{AlbumFinder, MatchFinder};
use super::picture_finder::{Found, Pick, PictureFinder, Request};
use super::skin_state::{ModelRows, format_time, string};
use super::skin_window::SkinWindow;
use album_page::*;
use artist_page::*;
use connection::*;
use editing::*;
use genre_page::GenreDraft;
use pictures::*;
use search_page::FoundPlaylist;
pub use search_page::{Opened, SearchFilter};
use text::*;
use values::*;

/// Search results are cut off here; nobody scrolls through more, and building rows for the
/// whole library on every keystroke would be wasted work.
const SEARCH_LIMIT: usize = 1_000;
/// How long typing pauses before what was typed fast is searched for: each key is not a
/// round trip to the server.
const TYPING_PAUSE: Duration = Duration::from_millis(150);
/// While a scan runs, the rows, covers and pictures are read anew at most this often.
const RELOAD_INTERVAL: Duration = Duration::from_secs(3);
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
    /// Freya Server on this computer, at `address`.
    Local { address: std::net::SocketAddr },
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

/// Where the thumbnails of `source`'s pictures are kept: a server elsewhere's apart from the
/// others, as they come from it as it made them.
pub fn covers_dir(source: Option<&LibrarySource>) -> Option<PathBuf> {
    let cache = crate::settings::cache_dir()?;
    Some(match source {
        Some(LibrarySource::Remote { pin, .. }) => {
            let server = freya_media::pictures::Pictures::checksum(&pin.certificate);
            cache.join(format!("covers-{}", &server[..16]))
        }
        _ => cache.join("covers"),
    })
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
    /// Told when what the rows need was asked of the server, on a thread of its own: they are
    /// read then.
    asking: Option<std::sync::mpsc::Receiver<()>>,
    /// Told whether each play noted on the errands' thread was written.
    plays_written: (Sender<bool>, Receiver<bool>),
    /// What connecting to the server gives, on a thread of its own; and what it found for the
    /// app, until the app takes it.
    connecting: Option<Receiver<Result<Connection, (&'static str, String)>>>,
    found: Option<Connected>,
    /// The same for the covers and pictures, and whether they are lined up to be made then.
    asking_pictures: Option<std::sync::mpsc::Receiver<()>>,
    prepare_after: bool,
    /// The same for the album of the track playing, and for the playlist shown: its name and
    /// entries.
    asking_playing: Option<Receiver<()>>,
    asking_playlist: Option<(Receiver<()>, String, Vec<Entry>)>,
    /// Whether the row of the track playing is to be shown once the rows are read, and in its
    /// album.
    reveal_playing: Option<bool>,
    /// Text typed while searching or suggesting, waiting for a pause in the typing, and when
    /// each was last done: a key pressed after a pause is answered at once, those typed fast
    /// after it once the typing pauses.
    typed: Option<String>,
    searched: Instant,
    typed_suggest: Option<String>,
    suggested: Instant,
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
    /// Each artist's albums, as the library lists them, read with the covers: in one call,
    /// not one an artist.
    artist_albums: HashMap<Option<String>, Vec<Album>>,
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
    /// Whether the covers and pictures in the library changed since they were last read, and
    /// when they were.
    pictures_stale: bool,
    pictures_loaded: Instant,
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

/// Popular tracks shown on an artist's page, or all of them with "more".
const POPULAR: usize = 5;
const POPULAR_MORE: usize = 10;

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
            asking: None,
            plays_written: std::sync::mpsc::channel(),
            connecting: None,
            found: None,
            asking_pictures: None,
            prepare_after: false,
            asking_playing: None,
            asking_playlist: None,
            reveal_playing: None,
            typed: None,
            searched: Instant::now() - TYPING_PAUSE,
            typed_suggest: None,
            suggested: Instant::now() - TYPING_PAUSE,
            items: Vec::new(),
            rows: ModelRows::new(),
            title: String::new(),
            subtitle: String::new(),
            stats: None,
            progress: None,
            stale: false,
            reloaded: Instant::now(),
            pictures_loaded: Instant::now(),
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
            artist_albums: HashMap::new(),
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

    /// Handles scan progress and covers made. Returns true when the rows changed.
    pub fn poll(&mut self) -> bool {
        if let Some(connecting) = &self.connecting {
            match connecting.try_recv() {
                Ok(connected) => self.connected(connected),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.connecting = None,
            }
        }
        self.take_typed();
        if self
            .plays_written
            .1
            .try_iter()
            .fold(false, |any, written| any | written)
            && self.shows_plays()
        {
            self.reload();
        }
        let answered = |asking: &Option<std::sync::mpsc::Receiver<()>>| {
            asking.as_ref().is_some_and(|asking| {
                !matches!(asking.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty))
            })
        };
        if answered(&self.asking_pictures) {
            self.load_pictures_now();
            if std::mem::take(&mut self.prepare_after) {
                self.prepare_pictures();
            }
            self.show_artist_info();
        }
        if answered(&self.asking) {
            self.reload_now();
        }
        if answered(&self.asking_playing) {
            self.asking_playing = None;
            self.find_playing_album();
        }
        let playlist_answered = self.asking_playlist.as_ref().is_some_and(|(asking, ..)| {
            !matches!(asking.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty))
        });
        if playlist_answered && let Some((_, name, entries)) = self.asking_playlist.take() {
            self.show_playlist_now(&name, &entries);
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
                ServiceEvent::Connected { scanning, told, .. } => {
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
                // The answers kept are forgotten by the client; the rows are read anew when
                // the events above say what changed them.
                ServiceEvent::Changed => {}
            }
        }
        // Covers and pictures found so far show as they come, every few seconds; and what was
        // said of the artist on the page.
        if self.pictures_stale
            && (!self.scanning || finished || self.pictures_loaded.elapsed() >= RELOAD_INTERVAL)
        {
            self.pictures_loaded = Instant::now();
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
        if self.stale
            && self.asking.is_none()
            && (finished || self.reloaded.elapsed() >= RELOAD_INTERVAL)
        {
            self.reload();
            return true;
        }
        finished
    }

    // ----- History -----------------------------------------------------------------------------

    /// Notes that a track was played just now.
    pub fn record_play(&mut self, path: PathBuf, part: Option<Part>) {
        let Some(server) = self.server.clone() else {
            return;
        };
        let at = SystemTime::now();
        let note = move || {
            // While a scan writes, the play waits rather than the window.
            server.record_play(&path, part, at).unwrap_or_else(|e| {
                log::warn!("could not note a play of {}: {e}", path.display());
                false
            })
        };
        // A server reached over the network is told on the errands' thread, and the rows made
        // of plays read again when it was.
        if self.remote.is_some() {
            let written = self.plays_written.0.clone();
            crate::errands::run(move || {
                let _ = written.send(note());
            });
            return;
        }
        if note() && self.shows_plays() {
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
        self.tell("ask the server to look", move |server| {
            server.fetch(artists.as_deref())
        });
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
}

impl LibraryPanel {}

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

/// A playlist entry for a library track, with its tags already known.
pub fn entry(track: &Track) -> Entry {
    Entry::with_tags(&track.path, track.part, &track.tags())
}

/// `albums` by their artist, each artist's in the order the library lists them: those with a
/// year by it, then by name, as `albums(artist)` has them.
fn artist_albums(albums: Vec<Album>) -> HashMap<Option<String>, Vec<Album>> {
    let mut by_artist: HashMap<Option<String>, Vec<Album>> = HashMap::new();
    for album in albums {
        by_artist
            .entry(album.artist.clone())
            .or_default()
            .push(album);
    }
    for albums in by_artist.values_mut() {
        albums.sort_by_cached_key(|album| {
            (
                album.name.is_none(),
                album.year.is_none(),
                album.year,
                album.name.as_deref().map(str::to_ascii_lowercase),
            )
        });
    }
    by_artist
}

#[cfg(test)]
mod tests;
