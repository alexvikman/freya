//! The music folders and their scans, as the server keeps them, and what the library is doing.

use super::*;

impl LibraryPanel {
    /// The library's folders now: those watched, and scanned later.
    /// The music folders, as the server keeps them; none before it is reached.
    pub fn folders(&self) -> Vec<PathBuf> {
        self.settings.library_folders.clone()
    }

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

    /// Until the scan's end has been taken in by `poll`, not only while its thread runs: a
    /// scan that ends between two polls has news that is not shown yet.
    pub fn is_scanning(&self) -> bool {
        self.scanning
    }

    /// Brings the library in line with `folders`: reads what changed, and asks the internet
    /// for what artists lack.
    pub fn rescan(&mut self, folders: &[PathBuf]) {
        self.set_folders(folders);
        self.tell("scan", |server| server.rescan());
    }

    /// Reads every file in `folders` again and makes every cover again, changed or not.
    pub fn rescan_files(&mut self, folders: &[PathBuf]) {
        self.set_folders(folders);
        self.tell("scan", |server| server.rescan_files());
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
        self.tell("scan", move |server| server.scan_only(&added));
    }

    /// Stops the scan running; what it did so far is kept.
    pub fn stop_scan(&mut self) {
        self.tell("stop the scan", |server| server.stop_scan());
    }

    /// What the scan running is doing, and how far it is.
    pub(super) fn scan_phase(&self) -> Option<String> {
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
