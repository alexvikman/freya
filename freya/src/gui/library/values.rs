//! What the skin's `Library` global is given.

use super::*;

impl LibraryPanel {
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
                "radio-directory-online",
                Value::Bool(self.settings.radio_directory_online),
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
}

/// One `LibraryRow`. `album_artist` is set while an album's tracks are shown: track numbers
/// are then shown, and the artist only where it differs from the album's. `played` says when
/// or how often a played track was played.
pub(super) fn row_value(
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
pub(super) fn page_backdrop<'a>(
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
pub(super) fn shown_backdrop<'a>(
    backdrops: &'a HashMap<String, String>,
    edits: &HashMap<String, ArtistEdit>,
    name: &str,
) -> Option<&'a String> {
    let picture = edits
        .get(name)
        .is_some_and(|edit| edit.header == Some(Header::Picture));
    backdrops.get(name).filter(|_| !picture)
}
