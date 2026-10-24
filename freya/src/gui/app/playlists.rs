//! The server's playlists, as Freya shows and edits them.

use super::*;

impl App {
    /// The entries of the playlist shown, in the order its page shows them.
    pub fn shown_playlist_entries(&self) -> Vec<Entry> {
        let Some(entries) = self
            .shown_playlist
            .and_then(|i| self.playlists.get(i))
            .map(|p| &p.entries)
        else {
            return Vec::new();
        };
        match self.library.playlist_order() {
            Some(order) if order.len() == entries.len() => {
                order.iter().map(|&i| entries[i].clone()).collect()
            }
            _ => entries.clone(),
        }
    }

    /// Shows the playlist named `name`, as a search found it.
    pub fn show_playlist_named(&mut self, name: &str) {
        if let Some(index) = self.playlists.index_of(name) {
            self.shown_playlist = Some(index);
            self.playlists_error.clear();
            self.show_view("playlist");
        }
    }

    /// Up Next becomes the playlist shown, shuffled.
    pub fn shuffle_playlist(&mut self) {
        let entries = self.shown_playlist_entries();
        if entries.is_empty() {
            return;
        }
        if !self.settings.shuffle {
            self.toggle_shuffle();
        }
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() as usize);
        let start = nanos % entries.len();
        self.play_entries(entries, start);
    }

    /// Adds `entries` to the `index`th playlist, noting when.
    pub fn add_to_playlist(&mut self, index: usize, entries: Vec<Entry>) {
        self.playlists.add(index, entries);
        self.note_playlist_added(index);
    }

    /// Notes when the tracks of the `index`th playlist not noted before were added: now.
    pub(super) fn note_playlist_added(&mut self, index: usize) {
        if let Some(playlist) = self.playlists.get(index) {
            self.library
                .note_playlist_added(&playlist.name, &playlist.entries);
        }
    }

    /// Shows the playlist's page again: its order or what it has changed.
    pub fn resync_playlist(&mut self) {
        self.view.playlists_synced = None;
    }

    /// Keeps the name, description and pictures of the playlist being changed.
    pub fn save_playlist_edit(&mut self, name: &str, description: &str) {
        let Some(index) = self.shown_playlist else {
            self.library.cancel_playlist_edit();
            return;
        };
        let before = self.playlists.get(index).map(|p| p.name.clone());
        let name = name.trim();
        let renamed = before.as_deref().is_some_and(|b| b != name) && !name.is_empty();
        if renamed {
            match self.playlists.rename(index, name) {
                Ok(index) => {
                    self.shown_playlist = Some(index);
                    self.playlists_error.clear();
                }
                Err(e) => {
                    self.library.set_playlist_draft_error(e);
                    return;
                }
            }
        }
        let shown = self
            .shown_playlist
            .and_then(|i| self.playlists.get(i))
            .map(|p| p.name.clone());
        if let Some(shown) = shown {
            self.library.save_playlist_edit(&shown, description);
        }
        self.resync_playlist();
    }

    /// Up Next's selected entries, or all of them when none is selected.
    pub fn queue_selection(&self) -> Vec<Entry> {
        let entries = self.playlist.entries();
        let selected: Vec<Entry> = self
            .playlist
            .selected_indices()
            .map(|i| entries[i].clone())
            .collect();
        if selected.is_empty() {
            entries.to_vec()
        } else {
            selected
        }
    }

    /// Makes a playlist and shows it, or keeps why it could not be made.
    pub fn create_playlist(&mut self, name: &str, entries: Vec<Entry>) {
        match self.playlists.create(name, entries) {
            Ok(index) => {
                self.shown_playlist = Some(index);
                self.playlists_error.clear();
                self.note_playlist_added(index);
            }
            Err(e) => self.playlists_error = e,
        }
    }

    pub fn rename_playlist(&mut self, name: &str) {
        let Some(index) = self.shown_playlist else {
            return;
        };
        match self.playlists.rename(index, name) {
            Ok(index) => {
                self.shown_playlist = Some(index);
                self.playlists_error.clear();
            }
            Err(e) => self.playlists_error = e,
        }
    }

    pub fn delete_playlist(&mut self) {
        if let Some(index) = self.shown_playlist.take() {
            self.playlists.delete(index);
        }
    }
}
