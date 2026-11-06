//! Up Next: what is added to it, played from it, and read of its files.

use super::*;

impl App {
    /// Changes the playlist, then keeps the gapless queue and the skin in step.
    pub fn edit(&mut self, change: impl FnOnce(&mut Playlist)) {
        change(&mut self.playlist);
        self.playlist_dirty = true;
        self.queue_next();
    }

    /// Appends the library's tracks of files, folders (all under them) and the files a playlist
    /// file names: music plays only from the library. What it does not have is left out, and
    /// said; a folder it does not have is offered to an admin as a music folder, when the server
    /// is on this computer and sees the folder as Freya does. Streams a playlist file names are
    /// added as they are, as radio is.
    pub fn add_paths(&mut self, paths: &[PathBuf]) {
        let mut asked = Vec::new();
        let mut streams = Vec::new();
        for path in paths {
            // Up Next is saved and restored, maybe from another folder: a path relative to
            // where Freya was started from would not find its file then.
            let path = absolute(path);
            if path.is_file() && playlist_file::is_playlist(&path) {
                match playlist_file::load(&path) {
                    Ok(loaded) => {
                        for entry in loaded {
                            if entry.path.to_string_lossy().contains("://") {
                                streams.push(entry);
                            } else {
                                asked.push(entry.path);
                            }
                        }
                    }
                    Err(e) => log::warn!("could not read {}: {e}", path.display()),
                }
                continue;
            }
            // A sheet's tracks are its audio files', where the library keeps them.
            if path.is_file() && freya_media::cue::is_sheet(&path) {
                match freya_media::cue::read_disc(&path) {
                    Some(disc) => asked.extend(disc.files),
                    None => asked.push(path),
                }
                continue;
            }
            asked.push(path);
        }
        let found = if asked.is_empty() {
            Vec::new()
        } else {
            match self.library.tracks_under(&asked) {
                Ok(found) => found,
                Err(e) => {
                    self.error = format!("Could not ask the library for what was added: {e}");
                    return;
                }
            }
        };
        let missing: Vec<&PathBuf> = asked
            .iter()
            .filter(|path| !found.iter().any(|track| track.path.starts_with(path)))
            .collect();
        let entries: Vec<Entry> = found.iter().map(library::entry).chain(streams).collect();
        if !missing.is_empty() {
            self.left_out(&missing);
        }
        self.edit(|p| {
            p.insert(None, entries);
        });
    }

    /// Says that `missing`, files and folders given to Freya, are not the library's; offers the
    /// first folder among them as a music folder, where that can be.
    fn left_out(&mut self, missing: &[&PathBuf]) {
        let folder = missing.iter().find(|path| path.is_dir());
        if let Some(folder) = folder
            && self.server.is_admin()
            && self.server_here()
        {
            self.edit_folders();
            self.add_draft_folder((*folder).clone());
            self.library.offer_folders();
            return;
        }
        self.error = match (missing, folder) {
            ([one], Some(_)) => format!(
                "{} is not in the library, and Freya plays only the library's music. An admin adds music folders in Settings.",
                one.display()
            ),
            ([one], None) => format!(
                "{} is not in the library, and Freya plays only the library's music.",
                one.display()
            ),
            _ => format!(
                "{} of what was added is not in the library, and Freya plays only the library's music.",
                missing.len()
            ),
        };
    }

    /// Whether the library's server is the one on this computer, which sees its files as
    /// Freya here does.
    pub(super) fn server_here(&self) -> bool {
        self.settings
            .server
            .as_ref()
            .is_some_and(|server| server.address == freya_protocol::LOCAL_ADDRESS)
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
        self.library.record_station(&station.name.clone(), &url);
        self.recents.changed();
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

    /// Plays the radio directory's `index`th station shown, as a saved one plays, and tells the
    /// server, which counts plays for Radio Browser.
    pub fn play_found(&mut self, index: usize) {
        let Some(station) = self.radio.get(index) else {
            return;
        };
        let (url, id) = (station.url.clone(), station.id.clone());
        self.library.record_station(&station.name.clone(), &url);
        self.recents.changed();
        let stream = self.station_streams.get(&url).unwrap_or(&url).clone();
        match self.entry_for(&stream) {
            Some(index) => self.play_index(index),
            None => self.add_url(&url),
        }
        self.radio.played(&id);
    }

    /// Opens what the `row`th row of Recents stands for: an album, a playlist, or a station,
    /// which plays.
    pub fn open_recent(&mut self, row: usize) {
        match self.recents.target(row) {
            Some(recents::Target::Album { artist, album }) => {
                self.library.open_album(artist, album);
                self.show_view("library");
            }
            Some(recents::Target::Playlist(name)) => self.show_playlist_named(&name),
            Some(recents::Target::Station { url, .. }) => {
                let saved = self.stations.iter().position(|s| s.url == url);
                match saved {
                    Some(index) => self.play_station(index),
                    None => self.add_url(&url),
                }
            }
            None => {}
        }
    }

    /// Keeps the radio directory's `index`th station shown among the server's stations, or no
    /// longer when it is kept.
    pub fn save_found(&mut self, index: usize) {
        let Some(station) = self.radio.get(index) else {
            return;
        };
        let (name, url) = (station.name.clone(), station.url.clone());
        let saved = self.stations.iter().position(|s| s.url == url);
        match saved {
            Some(saved) => self.stations.remove(saved),
            None => self.radio_error = self.stations.add(&name, &url).err().unwrap_or_default(),
        }
    }

    /// The address of the stream playing, if one plays.
    pub fn playing_stream(&self) -> Option<String> {
        if self.player.status().state == PlaybackState::Stopped {
            return None;
        }
        let playing = self.playlist.current_entry()?.path.to_string_lossy();
        freya_audio::is_url(&playing).then(|| playing.into_owned())
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
        if !self.library.ready() {
            self.waiting_files = Some(paths.to_vec());
            return;
        }
        self.edit(Playlist::clear);
        self.add_paths(paths);
        self.play_index(0);
    }

    /// Keeps of Up Next, as restored, only the library's tracks, with the library's tags; and
    /// streams, as radio is.
    pub(super) fn check_up_next(&mut self) {
        self.up_next_checked = true;
        let files: Vec<PathBuf> = self
            .playlist
            .entries()
            .iter()
            .filter(|entry| !entry.path.to_string_lossy().contains("://"))
            .map(|entry| entry.path.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if files.is_empty() {
            return;
        }
        let known = match self.library.tracks_under(&files) {
            Ok(tracks) => tracks,
            Err(e) => {
                log::warn!("could not check Up Next against the library: {e}");
                return;
            }
        };
        let known: HashMap<(PathBuf, Option<Part>), Entry> = known
            .iter()
            .map(|track| ((track.path.clone(), track.part), library::entry(track)))
            .collect();
        let before = self.playlist.len();
        self.playlist.retain(|entry| {
            entry.path.to_string_lossy().contains("://")
                || known.contains_key(&(entry.path.clone(), entry.part))
        });
        self.playlist
            .refresh_from(|entry| known.get(&(entry.path.clone(), entry.part)));
        let removed = before - self.playlist.len();
        if removed > 0 {
            log::info!("{removed} entries of Up Next are not the library's: taken out");
        }
        self.playlist_dirty = true;
        self.queue_next();
    }

    /// Reads the cover of what `media` plays, from where it is played.
    pub(super) fn request_cover(&mut self, media: &Media) {
        let remote = media.remote.as_ref().and(self.library.api());
        self.covers.request(&media.path, remote);
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
