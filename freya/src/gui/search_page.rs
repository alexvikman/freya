//! Searching the library: as the user types, a few of the best matches of every kind under the
//! search field; given, a page of them all: the best match of all, the songs, and rows of the
//! artists, albums, playlists and genres, or all of one kind. Names match as wholes first, then
//! at their start, then word by word, without case or accents (see
//! [`freya_library::search`]); the more tracks, the higher among equals.

use std::cmp::Reverse;
use std::time::Instant;

use freya_library::Entry;
use freya_library::search::{Rank, fold, rank};
use freya_library::thumbnails::{ArtistSource, Thumbnails};
use freya_protocol::{Album, Artist, Genre, Track, plain_name};
use slint_interpreter::{Struct, Value};

use super::{Item, Level, LibraryPanel, SEARCH_LIMIT, string};

/// Of each kind, on the page of all of them, and among the matches under the search field.
const ON_PAGE: usize = 12;
const SONGS_ON_PAGE: usize = 4;
const SUGGESTED: usize = 6;

/// Which kind of match the search page shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchFilter {
    #[default]
    All,
    Songs,
    Artists,
    Albums,
    Playlists,
    Genres,
}

impl SearchFilter {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "all" => Self::All,
            "songs" => Self::Songs,
            "artists" => Self::Artists,
            "albums" => Self::Albums,
            "playlists" => Self::Playlists,
            "genres" => Self::Genres,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Songs => "songs",
            Self::Artists => "artists",
            Self::Albums => "albums",
            Self::Playlists => "playlists",
            Self::Genres => "genres",
        }
    }
}

/// A playlist, as a search finds it: its name and how many tracks it has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundPlaylist {
    pub name: String,
    pub tracks: usize,
    /// When it was made, when known.
    pub created: Option<std::time::SystemTime>,
}

/// What a search found of each kind, the best first.
#[derive(Default)]
struct Found {
    artists: Vec<(Rank, Artist)>,
    albums: Vec<(Rank, Album)>,
    songs: Vec<Track>,
    /// How well the best song's title matches.
    song_rank: Option<Rank>,
    playlists: Vec<(Rank, FoundPlaylist)>,
    genres: Vec<(Rank, Genre)>,
}

/// What opening a match does, for the app to do the rest.
pub enum Opened {
    /// Plays `tracks` from the one at the index.
    Play(Vec<Track>, usize),
    /// Shows it in the library.
    Library,
    /// Shows the playlist named.
    Playlist(String),
}

impl LibraryPanel {
    /// What matches `text`: of each kind at most `each`, songs at most `songs`.
    fn find(&self, text: &str, each: usize, songs: usize) -> Found {
        let query = fold(text.trim());
        let Some(db) = &self.db else {
            return Found::default();
        };
        if query.is_empty() {
            return Found::default();
        }
        let mut found = Found::default();
        if let Ok(artists) = db.artists() {
            found.artists = artists
                .into_iter()
                .filter_map(|artist| {
                    let shown = artist
                        .shown_name
                        .clone()
                        .or_else(|| artist.name.as_deref().map(|n| plain_name(n).to_owned()))?;
                    Some((rank(&shown, &query)?, artist))
                })
                .collect();
            found
                .artists
                .sort_by_key(|(rank, artist)| (*rank, Reverse(artist.tracks)));
            found.artists.truncate(each);
        }
        if let Ok(albums) = db.all_albums() {
            found.albums = albums
                .into_iter()
                .filter_map(|album| {
                    let title = album.name.as_deref()?;
                    // By its title, or its title and artist together: "wall pink floyd".
                    let rank = rank(title, &query).or_else(|| {
                        let artist = album.artist.as_deref().map(plain_name).unwrap_or("");
                        rank(&format!("{title} {artist}"), &query).map(|_| Rank::Words)
                    })?;
                    Some((rank, album))
                })
                .collect();
            found
                .albums
                .sort_by_key(|(rank, album)| (*rank, Reverse(album.tracks)));
            found.albums.truncate(each);
        }
        if let Ok(tracks) = db.search(text, SEARCH_LIMIT) {
            // Those whose titles match first, as wholes, starts or words; the rest as found.
            let mut ranked: Vec<(Option<Rank>, usize, Track)> = tracks
                .into_iter()
                .enumerate()
                .map(|(i, track)| {
                    let rank = track.title.as_deref().and_then(|t| rank(t, &query));
                    (rank, i, track)
                })
                .collect();
            ranked.sort_by_key(|(rank, i, _)| (rank.is_none(), *rank, *i));
            found.song_rank = ranked.first().and_then(|(rank, ..)| *rank);
            found.songs = ranked.into_iter().take(songs).map(|(.., t)| t).collect();
        }
        found.playlists = self
            .found_playlists
            .iter()
            .filter_map(|playlist| Some((rank(&playlist.name, &query)?, playlist.clone())))
            .collect();
        found
            .playlists
            .sort_by_key(|(rank, playlist)| (*rank, Reverse(playlist.tracks)));
        found.playlists.truncate(each);
        if let Ok(genres) = db.genres() {
            found.genres = genres
                .into_iter()
                .filter_map(|genre| Some((rank(genre.name.as_deref()?, &query)?, genre)))
                .collect();
            found
                .genres
                .sort_by_key(|(rank, genre)| (*rank, Reverse(genre.tracks)));
            found.genres.truncate(each);
        }
        found
    }

    /// The rows of the search page for the query, as `search_filter` says, with its sections
    /// and its best match of all, when it shows them all.
    pub(super) fn search_items(&mut self) -> Vec<Item> {
        let filter = self.search_filter;
        let all = filter == SearchFilter::All;
        let (each, songs) = if all {
            (ON_PAGE, SONGS_ON_PAGE)
        } else {
            (SEARCH_LIMIT, SEARCH_LIMIT)
        };
        let found = self.find(&self.query.clone(), each, songs);
        let mut items = Vec::new();
        let mut sections = Vec::new();
        let mut section =
            |items: &mut Vec<Item>, kind: &'static str, title: &'static str, found: Vec<Item>| {
                if found.is_empty() {
                    return;
                }
                sections.push((kind, title, items.len(), found.len()));
                items.extend(found);
            };
        let top = all.then(|| top_of(&found)).flatten();
        let show = |kind: SearchFilter| all || filter == kind;
        if show(SearchFilter::Songs) {
            section(
                &mut items,
                "songs",
                "Songs",
                found.songs.into_iter().map(Item::Track).collect(),
            );
        }
        if show(SearchFilter::Artists) {
            let artists = found
                .artists
                .into_iter()
                .map(|(_, a)| Item::Artist(a))
                .collect();
            section(&mut items, "artists", "Artists", artists);
        }
        if show(SearchFilter::Albums) {
            let albums = found
                .albums
                .into_iter()
                .map(|(_, a)| Item::Album(a))
                .collect();
            section(&mut items, "albums", "Albums", albums);
        }
        if show(SearchFilter::Playlists) {
            let playlists = found
                .playlists
                .into_iter()
                .map(|(_, p)| Item::Playlist(p))
                .collect();
            section(&mut items, "playlists", "Playlists", playlists);
        }
        if show(SearchFilter::Genres) {
            let genres = found
                .genres
                .into_iter()
                .map(|(_, g)| Item::Genre(g))
                .collect();
            section(&mut items, "genres", "Genres", genres);
        }
        // The best match of all: the first of its section.
        self.search_top = top.and_then(|kind| {
            sections
                .iter()
                .find(|(k, ..)| *k == kind)
                .map(|(_, _, first, _)| *first)
        });
        // The songs beside the best match; the rest under them.
        self.search_songs = sections
            .iter()
            .find(|(kind, ..)| *kind == "songs")
            .map(|(_, _, first, count)| (*first, *count));
        let rows = sections
            .iter()
            .filter(|(kind, ..)| *kind != "songs")
            .map(|(kind, title, first, count)| {
                Value::Struct(Struct::from_iter([
                    ("kind".to_owned(), string(*kind)),
                    ("title".to_owned(), string(*title)),
                    ("first".to_owned(), Value::Number(*first as f64)),
                    ("count".to_owned(), Value::Number(*count as f64)),
                ]))
            })
            .collect();
        self.search_section_rows.sync(rows);
        items
    }

    /// A few of the best matches of `text`, for under the search field.
    pub fn suggest(&mut self, text: &str) {
        if self.suggested.elapsed() < self.typing_pause() {
            self.typed_suggest = Some(text.to_owned());
            return;
        }
        self.typed_suggest = None;
        self.suggested = Instant::now();
        let found = self.find(text, SUGGESTED, SUGGESTED);
        // Each kind's in turn, by how well they match: the best of all first.
        let mut ranked: Vec<(Rank, usize, Item)> = Vec::new();
        let mut add = |rank: Rank, order: usize, item: Item| ranked.push((rank, order, item));
        for (i, (rank, artist)) in found.artists.into_iter().enumerate() {
            add(rank, i * 5, Item::Artist(artist));
        }
        for (i, (rank, album)) in found.albums.into_iter().enumerate() {
            add(rank, i * 5 + 1, Item::Album(album));
        }
        for (i, (rank, playlist)) in found.playlists.into_iter().enumerate() {
            add(rank, i * 5 + 2, Item::Playlist(playlist));
        }
        for (i, track) in found.songs.into_iter().enumerate() {
            let rank = if i == 0 { found.song_rank } else { None };
            add(rank.unwrap_or(Rank::Words), i * 5 + 3, Item::Track(track));
        }
        for (i, (rank, genre)) in found.genres.into_iter().enumerate() {
            add(rank, i * 5 + 4, Item::Genre(genre));
        }
        ranked.sort_by_key(|(rank, order, _)| (*rank, *order));
        self.suggestions = ranked
            .into_iter()
            .take(SUGGESTED)
            .map(|(.., item)| item)
            .collect();
        let rows = self
            .suggestions
            .iter()
            .map(|item| super::row_value(item, None, |_| String::new(), None))
            .collect();
        self.suggestion_rows.sync(rows);
        for item in &self.suggestions {
            if let Item::Artist(artist) = item {
                self.artist_keys.insert(
                    Thumbnails::artist_key(artist.name.as_deref()),
                    artist.name.clone(),
                );
            }
        }
    }

    /// Opens the `index`th match under the search field.
    pub fn open_suggestion(&mut self, index: usize) -> Option<Opened> {
        let item = self.suggestions.get(index)?.clone();
        self.suggestions.clear();
        self.suggestion_rows.sync(Vec::new());
        self.query.clear();
        let (section, level) = match item {
            Item::Artist(artist) => (
                Level::Artists,
                Level::ArtistAlbums {
                    artist: artist.name,
                },
            ),
            Item::Album(album) => (
                Level::Albums,
                Level::AlbumTracks {
                    artist: album.artist,
                    album: album.name,
                },
            ),
            Item::Genre(genre) => (Level::Genres, Level::GenreAlbums { genre: genre.name }),
            Item::Playlist(playlist) => return Some(Opened::Playlist(playlist.name)),
            Item::Track(track) | Item::Played(freya_protocol::Played { track, .. }) => {
                return Some(Opened::Play(vec![track], 0));
            }
        };
        self.opened = vec![section, level];
        self.reload();
        Some(Opened::Library)
    }

    /// Shows matches of one kind, or of all: see [`SearchFilter`].
    pub fn set_search_filter(&mut self, filter: &str) {
        if let Some(filter) = SearchFilter::from_name(filter)
            && filter != self.search_filter
        {
            self.search_filter = filter;
            self.reload();
        }
    }

    /// The user's playlists, with their tracks and when each was made, for searches and the
    /// playlists' section to find and show.
    pub fn set_playlists(
        &mut self,
        playlists: &[(String, &[Entry], Option<std::time::SystemTime>)],
    ) {
        let before = self.found_playlists.clone();
        self.found_playlists = playlists
            .iter()
            .map(|(name, entries, created)| FoundPlaylist {
                name: name.clone(),
                tracks: entries.len(),
                created: *created,
            })
            .collect();
        if before != self.found_playlists && *self.level() == Level::Playlists {
            self.stale = true;
        }
        // What each playlist's picture is made from, asked for at once.
        if let Some(db) = &self.db {
            let mut each = Vec::new();
            for (name, entries, _) in playlists {
                if self
                    .playlist_sources
                    .contains_key(&Self::playlist_key(name))
                {
                    continue;
                }
                each.push(("playlist_info", freya_protocol::args(&(name,))));
                for entry in &entries[..entries.len().min(40)] {
                    let start = entry.part.map_or(std::time::Duration::ZERO, |p| p.start);
                    each.push(("track_at", freya_protocol::args(&(&entry.path, start))));
                }
            }
            db.prefetch(&each);
        }
        for (name, entries, _) in playlists {
            let key = Self::playlist_key(name);
            if self.playlist_sources.contains_key(&key) {
                continue;
            }
            let picture = self
                .db
                .as_ref()
                .and_then(|db| db.playlist_info(name).ok())
                .and_then(|info| info.picture);
            let covers = self.first_covers(entries);
            self.playlist_sources
                .insert(key, ArtistSource { picture, covers });
        }
    }

    /// The playlist a search's match opens, once: the app shows it.
    pub fn take_open_playlist(&mut self) -> Option<String> {
        self.open_playlist.take()
    }
}

/// The kind of the best match of all: an artist, album, playlist or genre named as typed, or
/// whose name starts with it, before a song; else the first song, else the first match.
fn top_of(found: &Found) -> Option<&'static str> {
    let firsts = [
        ("artists", found.artists.first().map(|(r, _)| *r)),
        ("albums", found.albums.first().map(|(r, _)| *r)),
        ("playlists", found.playlists.first().map(|(r, _)| *r)),
        ("genres", found.genres.first().map(|(r, _)| *r)),
    ];
    let best = firsts
        .iter()
        .filter_map(|(kind, rank)| Some((rank.as_ref()?, *kind)))
        .min_by_key(|(rank, _)| **rank);
    let song = found
        .songs
        .first()
        .map(|_| found.song_rank.unwrap_or(Rank::Words));
    match (best, song) {
        (Some((rank, kind)), Some(song)) if *rank <= song => Some(kind),
        (_, Some(_)) => Some("songs"),
        (Some((_, kind)), None) => Some(kind),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use freya_library::Library;

    use super::*;

    /// A panel over a library of the test fixture: the artist Freya.
    fn panel() -> LibraryPanel {
        // One for each test, as they run at once.
        static MADE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let made = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("freya-search-{}-{made}", std::process::id()));
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
        panel.open(crate::gui::library::tests::local(db, None));
        panel.set_playlists(&[("Freya's favourites".to_owned(), &[][..], None)]);
        panel
    }

    #[test]
    fn a_search_shows_the_best_match_and_each_kind() {
        let mut panel = panel();
        panel.search("freya");
        assert_eq!(panel.search_filter, SearchFilter::All);
        let top = panel.search_top.expect("a best match");
        assert!(matches!(&panel.items[top], Item::Artist(a) if a.name.as_deref() == Some("Freya")));
        assert!(
            panel
                .items
                .iter()
                .any(|item| matches!(item, Item::Playlist(p) if p.name == "Freya's favourites"))
        );

        // Only one kind.
        panel.set_search_filter("playlists");
        assert_eq!(panel.items.len(), 1);
        assert_eq!(panel.search_top, None);

        // Nothing matches a word that starts no word.
        panel.set_search_filter("all");
        panel.search("reya");
        assert!(
            !panel
                .items
                .iter()
                .any(|item| matches!(item, Item::Artist(_))),
            "not the start of a word"
        );
    }

    #[test]
    fn the_playlists_section_lists_favourites_by_when_they_were_made_ones() {
        let mut panel = panel();
        let at =
            |s: u64| Some(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(s));
        panel.set_playlists(&[
            ("Old".to_owned(), &[][..], at(100)),
            ("New".to_owned(), &[][..], at(200)),
        ]);
        panel.show("playlists");
        let names = |panel: &LibraryPanel| -> Vec<String> {
            panel
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::Playlist(p) => Some(p.name.clone()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(names(&panel), ["New", "Old"]);
        panel.toggle_favorite(1);
        assert_eq!(names(&panel), ["Old", "New"], "made a favourite now");
        assert!(panel.is_playlist_favorite("Old"));
        panel.set_playlists_order("name");
        assert_eq!(names(&panel), ["New", "Old"]);
        // A playlist opens as its own page.
        assert!(panel.activate(0).is_none());
        assert_eq!(panel.take_open_playlist().as_deref(), Some("New"));
    }

    #[test]
    fn the_best_matches_show_as_typed_and_open() {
        let mut panel = panel();
        panel.suggest("fre");
        assert!(matches!(panel.suggestions.first(), Some(Item::Artist(_))));
        assert!(
            panel.query.is_empty(),
            "what is shown stays as it was while typing"
        );
        assert!(matches!(panel.open_suggestion(0), Some(Opened::Library)));
        assert!(matches!(panel.level(), Level::ArtistAlbums { .. }));
        assert_eq!(panel.opened.len(), 2, "back goes to the artists");

        panel.suggest("favourites");
        let playlist = panel
            .suggestions
            .iter()
            .position(|item| matches!(item, Item::Playlist(_)))
            .unwrap();
        assert!(
            matches!(panel.open_suggestion(playlist), Some(Opened::Playlist(name)) if name == "Freya's favourites")
        );
    }
}
