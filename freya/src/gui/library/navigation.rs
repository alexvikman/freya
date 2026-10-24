//! Where the library is: its sections and pages, going back, searching, and what plays.

use super::*;

impl LibraryPanel {
    pub fn search(&mut self, text: &str) {
        if text.trim() == self.query.trim() {
            self.query = text.to_owned();
            self.typed = None;
            return;
        }
        if self.searched.elapsed() < self.typing_pause() {
            self.typed = Some(text.to_owned());
            return;
        }
        self.typed = None;
        self.searched = Instant::now();
        self.query = text.to_owned();
        self.reload();
    }

    /// What was typed fast, searched and suggested for once the typing paused.
    pub(super) fn take_typed(&mut self) {
        if self.searched.elapsed() >= self.typing_pause()
            && let Some(text) = self.typed.take()
        {
            self.search(&text);
        }
        if self.suggested.elapsed() >= self.typing_pause()
            && let Some(text) = self.typed_suggest.take()
        {
            self.suggest(&text);
        }
    }

    /// How long typing pauses before what was typed fast is searched for: for a library on
    /// another computer, [`TYPING_PAUSE`]; on this one, each key is answered at once.
    pub(super) fn typing_pause(&self) -> Duration {
        if self.elsewhere() {
            TYPING_PAUSE
        } else {
            Duration::ZERO
        }
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
            // Its album, from the library: asked of a server on a thread.
            let asked = self
                .playing
                .clone()
                .map(|(path, start)| ahead("track_at", &(path.as_path(), start)));
            self.asking_playing =
                asked.and_then(|call| self.ask_on_thread(move |api| api.prefetch(&[call])));
            if self.asking_playing.is_none() {
                self.find_playing_album();
            }
        }
    }

    pub(super) fn find_playing_album(&mut self) {
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
        self.reveal_playing = Some(album);
        self.reload();
        true
    }

    pub(super) fn find_playing_row(&mut self) {
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

    /// The name an artist is shown by: the one the user gave them, else the tags'.
    pub(super) fn shown_artist(&self, name: Option<&str>) -> String {
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

    pub(super) fn reload_artists(&mut self) {
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

    pub(super) fn reload_albums(&mut self) {
        if matches!(self.level(), Level::ArtistAlbums { .. }) {
            self.reload();
        }
    }

    /// Whether an artist's albums are shown with their tracks, one album after another.
    pub(super) fn discography(&self) -> bool {
        self.album_list && !self.searching() && matches!(self.level(), Level::ArtistAlbums { .. })
    }

    /// The rows of the album `row` is in, in a discography: from its album row to the next
    /// album's. Elsewhere all rows.
    pub(super) fn section_of(&self, row: usize) -> std::ops::Range<usize> {
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
    pub(super) fn level(&self) -> &Level {
        self.opened
            .last()
            .expect("a section's own list is always open")
    }

    pub(super) fn searching(&self) -> bool {
        !self.query.trim().is_empty()
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
