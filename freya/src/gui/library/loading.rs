//! Loading what the library shows: its rows, the calls asked ahead, and writes to it.

use super::*;

impl LibraryPanel {
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
    /// Asks a server reached over the network for what `ask` asks of it, on a thread of its
    /// own: told when it was. None for a library asked here and now.
    pub(super) fn ask_on_thread(
        &self,
        ask: impl FnOnce(&dyn LibraryApi) + Send + 'static,
    ) -> Option<Receiver<()>> {
        let api = Arc::clone(self.db.as_ref().filter(|_| self.remote.is_some())?);
        let (done, answered) = std::sync::mpsc::sync_channel(1);
        // On the errands' thread, after what the window changed: read as it is now.
        crate::errands::run(move || {
            ask(api.as_ref());
            let _ = done.send(());
        });
        Some(answered)
    }

    /// Writes to the library with `write`: for a server reached over the network on the
    /// errands' thread, the window not waiting for it, before what is read after.
    pub(super) fn write(
        &self,
        what: &'static str,
        write: impl FnOnce(&dyn LibraryApi) -> freya_protocol::ApiResult<()> + Send + 'static,
    ) {
        let Some(db) = self.db.clone() else {
            return;
        };
        let written = move || {
            if let Err(e) = write(db.as_ref()) {
                log::warn!("could not {what}: {e}");
            }
        };
        if self.remote.is_some() {
            crate::errands::run(written);
        } else {
            written();
        }
    }

    /// Changes the library with `write`, then reads the rows anew: for a server reached over
    /// the network, both on the errands' thread, the window not waiting for them.
    pub(super) fn change(
        &mut self,
        what: &'static str,
        write: impl FnOnce(&dyn LibraryApi) -> freya_protocol::ApiResult<()> + Send + 'static,
    ) {
        let Some(db) = self.db.clone() else {
            return;
        };
        let written = move |api: &dyn LibraryApi| {
            if let Err(e) = write(api) {
                log::warn!("could not {what}: {e}");
            }
        };
        if self.remote.is_none() {
            written(db.as_ref());
            self.reload();
            return;
        }
        let calls = self.calls_ahead();
        let level = self.level().clone();
        let (list, searching) = (self.album_list, self.searching());
        self.asking = self.ask_on_thread(move |api| {
            written(api);
            api.prefetch(&calls);
            if !searching {
                api.prefetch(&calls_after(api, &level, list));
            }
        });
    }

    /// Reads the rows anew. A server is asked for all they need on a thread of its own, the
    /// window not waiting for it, and they are read once its answers come.
    pub(super) fn reload(&mut self) {
        let calls = self.calls_ahead();
        let level = self.level().clone();
        let (list, searching) = (self.album_list, self.searching());
        self.asking = self.ask_on_thread(move |api| {
            api.prefetch(&calls);
            if !searching {
                api.prefetch(&calls_after(api, &level, list));
            }
        });
        if self.asking.is_none() {
            self.reload_now();
        }
    }

    /// Reads the rows anew now, from what was asked ahead.
    pub(super) fn reload_now(&mut self) {
        self.asking = None;
        if std::mem::take(&mut self.stale) {
            self.stats = None;
        }
        if let Some(db) = &self.db {
            db.prefetch(&self.calls_ahead());
        }
        self.load_pictures_now();
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
        // The genres' covers, shown with them.
        if *self.level() == Level::Genres && !self.searching() {
            let genres = self.items.iter().filter_map(|item| match item {
                Item::Genre(genre) => Some(ahead("genre_albums", &(genre.name.as_deref(),))),
                _ => None,
            });
            db.prefetch(&genres.collect::<Vec<_>>());
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
        if let Some(album) = self.reveal_playing.take() {
            self.reveal_row = if album { self.playing_row } else { -1 };
            self.reveals += 1;
        }
        log::debug!("library rows loaded in {:?}", started.elapsed());
    }

    /// The letters to jump to in the list shown, when it is in the order of its names: each
    /// with the first row it starts, or the next letter's when none does.
    pub(super) fn letters(&self) -> Vec<Value> {
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

    /// What [`Self::reload`] reads next, to be asked all at once: one round trip to the
    /// server, not one a call.
    pub(super) fn calls_ahead(&self) -> Vec<(&'static str, Vec<u8>)> {
        let mut calls = vec![
            ahead("artist_edits", &()),
            ahead("album_favorites", &()),
            ahead("artist_favorites", &()),
            ahead("playlist_favorites", &()),
            ahead("stats", &()),
        ];
        if self.pictures_stale {
            calls.extend(picture_calls());
        }
        if self.searching() {
            calls.extend([
                ahead("search", &(self.query.as_str(), SEARCH_LIMIT)),
                ahead("artists", &()),
                ahead("all_albums", &()),
                ahead("genres", &()),
            ]);
            return calls;
        }
        match self.level() {
            Level::Artists => calls.push(ahead("artists_by", &(self.artist_order,))),
            Level::Albums => calls.push(ahead("albums_by", &(self.albums_order,))),
            Level::Songs => calls.push(ahead("all_tracks", &())),
            Level::Genres => calls.push(ahead("genres", &())),
            Level::Playlists => {}
            Level::RecentlyAdded => calls.push(ahead("recently_added", &(RECENT,))),
            Level::RecentlyPlayed => calls.push(ahead("recently_played", &(RECENT,))),
            Level::MostPlayed => calls.push(ahead("most_played", &(RECENT,))),
            Level::ArtistAlbums { artist } => {
                let artist = artist.as_deref();
                calls.extend([
                    ahead("albums", &(artist,)),
                    ahead("artist_summary", &(artist, 3usize)),
                    ahead("artist_popular", &(artist, POPULAR_MORE)),
                    ahead("artist_mbid", &(artist,)),
                ]);
                if let Some(name) = artist {
                    calls.extend([
                        ahead("searched_artist_id", &(name,)),
                        ahead("artist_article", &(name,)),
                        ahead("artist_text", &(name,)),
                    ]);
                }
            }
            Level::GenreAlbums { genre } => {
                calls.push(ahead("genre_albums", &(genre.as_deref(),)));
                if let Some(name) = genre {
                    calls.extend([
                        ahead("genre_tracks", &(Some(name.as_str()),)),
                        ahead("genre_popular", &(name.as_str(), POPULAR_MORE)),
                        ahead("genre_article", &(name.as_str(),)),
                    ]);
                }
            }
            Level::AlbumTracks { artist, album } => calls.extend([
                ahead("album_tracks", &(artist.as_deref(), album.as_deref())),
                ahead("album_plays", &(artist.as_deref(), album.as_deref())),
            ]),
        }
        calls
    }

    pub(super) fn load(&self, db: &dyn LibraryApi) -> freya_protocol::ApiResult<Vec<Item>> {
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
                let each: Vec<_> = list
                    .iter()
                    .map(|album| {
                        ahead(
                            "album_tracks",
                            &(album.artist.as_deref(), album.name.as_deref()),
                        )
                    })
                    .collect();
                db.prefetch(&each);
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

    pub(super) fn heading(&self) -> (String, String) {
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
}
