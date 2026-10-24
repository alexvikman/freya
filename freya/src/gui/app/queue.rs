//! Up Next: what is added to it, played from it, and read of its files.

use super::*;

impl App {
    /// Changes the playlist, then keeps the gapless queue and the skin in step.
    pub fn edit(&mut self, change: impl FnOnce(&mut Playlist)) {
        change(&mut self.playlist);
        self.playlist_dirty = true;
        self.queue_next();
    }

    /// Appends files, folders (recursively) and the contents of playlist files. A file that a
    /// CUE sheet cuts into tracks, or the sheet itself, adds its tracks.
    pub fn add_paths(&mut self, paths: &[PathBuf]) {
        let mut entries = Vec::new();
        for path in paths {
            // Up Next is saved and restored, maybe from another folder: a path relative to
            // where Freya was started from would not find its file then.
            let path = &absolute(path);
            if path.is_file() && playlist_file::is_playlist(path) {
                match playlist_file::load(path) {
                    Ok(loaded) => entries.extend(loaded),
                    Err(e) => log::warn!("could not read {}: {e}", path.display()),
                }
                continue;
            }
            let found = freya_media::files::collect_with_sheets(std::slice::from_ref(path));
            for item in cue::apply_sheets(found) {
                match item {
                    Found::File(path) => entries.push(Entry::new(path)),
                    Found::Disc(disc) => {
                        entries.extend(disc.tracks.iter().map(|track| {
                            Entry::with_tags(&track.path, Some(track.part), &track.tags)
                        }))
                    }
                }
            }
        }
        self.edit(|p| {
            p.insert(None, entries);
        });
        self.request_missing_tags();
    }

    /// Adds an internet radio stream and plays it. A station playlist from a radio directory
    /// is downloaded first, in the background.
    pub fn add_url(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let url = match radio::stream_url(text) {
            Ok(url) => url,
            Err(e) => {
                self.error = e;
                return;
            }
        };
        if playlist_file::is_playlist_url(&url) {
            let results = self.station_lists.0.clone();
            std::thread::spawn(move || {
                let entries = playlist_file::fetch(&url).map_err(|e| format!("{url}: {e}"));
                let _ = results.send((url, entries));
            });
            return;
        }
        self.add_and_play(vec![Entry::new(url)]);
    }

    /// Plays a saved station: its entry in the playlist when it is there, else added.
    pub fn play_station(&mut self, index: usize) {
        let Some(station) = self.stations.get(index) else {
            return;
        };
        let url = station.url.clone();
        let stream = self.station_streams.get(&url).unwrap_or(&url).clone();
        match self.entry_for(&stream) {
            Some(index) => self.play_index(index),
            None => self.add_url(&url),
        }
    }

    /// The first entry for `url` in Up Next.
    pub(super) fn entry_for(&self, url: &str) -> Option<usize> {
        self.playlist
            .entries()
            .iter()
            .position(|entry| entry.path.to_string_lossy() == url)
    }

    /// The address of the saved station playing, if a stream of one plays.
    pub fn playing_station(&self) -> Option<String> {
        if self.player.status().state == PlaybackState::Stopped {
            return None;
        }
        let playing = self.playlist.current_entry()?.path.to_string_lossy();
        self.stations
            .iter()
            .find(|station| {
                station.url == playing
                    || self
                        .station_streams
                        .get(&station.url)
                        .is_some_and(|stream| *stream == playing)
            })
            .map(|station| station.url.clone())
    }

    /// Appends entries and plays the first of them.
    pub(super) fn add_and_play(&mut self, entries: Vec<Entry>) {
        if entries.is_empty() {
            return;
        }
        let first = self.playlist.len();
        self.edit(|p| {
            p.insert(None, entries);
        });
        self.play_index(first);
    }

    pub fn replace_and_play(&mut self, paths: &[PathBuf]) {
        self.edit(Playlist::clear);
        self.add_paths(paths);
        self.play_index(0);
    }

    /// Reads the cover of what `media` plays, from where it is played.
    pub(super) fn request_cover(&mut self, media: &Media) {
        let remote = media.remote.as_ref().and(self.library.api());
        self.covers.request(&media.path, remote);
    }

    pub(super) fn request_missing_tags(&self) {
        for entry in self.playlist.entries().iter().filter(|e| !e.tags_loaded) {
            // A file played from a server elsewhere is not here to read.
            let remote = self.library.stream_url(&entry.path).is_some();
            if !entry.path.to_string_lossy().contains("://") && !remote {
                self.tags.request(entry.path.clone());
            }
        }
    }

    /// Replaces the playlist with library tracks and plays from `start`.
    pub fn play_tracks(&mut self, tracks: &[Track], start: usize) {
        self.play_entries(tracks.iter().map(library::entry).collect(), start);
    }

    /// Up Next becomes `tracks`, with shuffle on, playing from a random one.
    pub fn shuffle_tracks(&mut self, tracks: &[Track]) {
        if tracks.is_empty() {
            return;
        }
        if !self.settings.shuffle {
            self.toggle_shuffle();
        }
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() as usize);
        self.play_tracks(tracks, nanos % tracks.len());
    }

    pub fn enqueue_tracks(&mut self, tracks: &[Track]) {
        self.enqueue_entries(tracks.iter().map(library::entry).collect());
    }

    /// Up Next becomes `entries`, playing from the one at `start`.
    pub fn play_entries(&mut self, entries: Vec<Entry>, start: usize) {
        let count = entries.len();
        if count == 0 {
            return;
        }
        self.edit(|p| {
            p.clear();
            p.insert(None, entries);
        });
        self.play_index(start.min(count - 1));
    }

    /// Adds `entries` to the end of Up Next.
    pub fn enqueue_entries(&mut self, entries: Vec<Entry>) {
        self.edit(|p| {
            p.insert(None, entries);
        });
    }
}
