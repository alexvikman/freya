//! Favourites, of tracks, albums, artists and playlists.

use super::*;

impl LibraryPanel {
    /// Whether the album `artist` / `album` is a favourite.
    pub(super) fn is_favorite(&self, artist: Option<&str>, album: Option<&str>) -> bool {
        self.favorites
            .contains_key(&(artist.map(str::to_owned), album.map(str::to_owned)))
    }

    /// Whether the album whose page is shown is a favourite.
    pub(super) fn page_favorite(&self) -> bool {
        match self.level() {
            Level::AlbumTracks { artist, album } => {
                self.is_favorite(artist.as_deref(), album.as_deref())
            }
            _ => false,
        }
    }

    /// Makes the album `artist` / `album` a favourite, or not one any longer.
    pub(super) fn set_favorite(&mut self, artist: Option<String>, album: Option<String>) {
        let favorite = !self.is_favorite(artist.as_deref(), album.as_deref());
        let at = SystemTime::now();
        self.change("keep the favourite", move |api| {
            api.set_album_favorite(artist.as_deref(), album.as_deref(), favorite, at)
        });
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
    pub(super) fn set_artist_favorite(&mut self, name: &str) {
        let favorite = !self.artist_favorites.contains_key(name);
        let (name, at) = (name.to_owned(), SystemTime::now());
        self.change("keep the favourite", move |api| {
            api.set_artist_favorite(&name, favorite, at)
        });
    }

    /// Makes the playlist `name` a favourite, or not one any longer.
    pub fn set_playlist_favorite(&mut self, name: &str) {
        let favorite = !self.playlist_favorites.contains_key(name);
        let (name, at) = (name.to_owned(), SystemTime::now());
        self.change("keep the favourite", move |api| {
            api.set_playlist_favorite(&name, favorite, at)
        });
    }

    /// Whether the playlist `name` is a favourite.
    pub fn is_playlist_favorite(&self, name: &str) -> bool {
        self.playlist_favorites.contains_key(name)
    }

    /// The playlists' section: every playlist, by when it was made or made a favourite, the
    /// latest first; or by name.
    pub(super) fn playlist_items(&self) -> Vec<Item> {
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
}
