//! The server the library is on: choosing it, its library, and its music folders.

use super::*;

impl App {
    /// Shows the library, if the skin has one.
    pub(super) fn attach_library(&mut self) {
        if self.view.server_out.available {
            let ui = &self.view.ui;
            let _ =
                ui.set_global_property("Server", "devices", self.server.device_rows.model_value());
            let _ = ui.set_global_property("Server", "users", self.server.user_rows.model_value());
            let _ = ui.set_global_property("Server", "found", self.server.found_rows.model_value());
            let _ = ui.set_global_property(
                "Server",
                "browse-folders",
                self.server.browse_rows.model_value(),
            );
        }
        if self.view.library_out.available {
            self.library.attach(&self.view.ui);
            self.take_connected();
        }
    }

    /// Takes what connecting to the server found, once it is connected: the account and the
    /// server, the playlists, how scrobbling goes, and the server's settings.
    pub(super) fn take_connected(&mut self) {
        if let Some(connected) = self.library.take_connected() {
            let remote = self.library.remote();
            self.server
                .connected(remote.as_deref(), connected.account, connected.info);
            self.server.load_users(remote);
            self.playlists
                .connect(Arc::clone(&connected.server), connected.playlists);
            self.lastfm.use_server(connected.server, connected.lastfm);
            // The server's settings, as the settings page shows them.
            if let Some(server) = self.library.server_settings().cloned() {
                let settings = &mut self.settings;
                settings.library_folders = server.library_folders;
                settings.watch_folders = server.watch_folders;
                settings.scan_every_hours = server.scan_every_hours;
                settings.scan_at_start = server.scan_at_start;
                settings.artist_pictures_online = server.artist_pictures_online;
                settings.artist_bios_online = server.artist_bios_online;
                settings.artist_wikipedia_online = server.artist_wikipedia_online;
                settings.fanart_key = server.fanart_key;
            }
        }
    }

    /// Shows the library of the server `chosen`, which the user just logged in to; this
    /// computer keeps its token and certificate.
    pub(super) fn use_remote_server(&mut self, chosen: server_panel::Chosen) {
        if let Some(store) = settings::token_store() {
            store.set(
                &freya_client::token_key(&chosen.pin),
                &chosen.tokens.refresh,
            );
        }
        self.settings.server = Some(settings::SavedServer::new(&chosen.name, &chosen.pin));
        self.settings.save();
        self.server.set_known(Some(chosen.pin.certificate.clone()));
        self.leave_queue();
        self.show_library_of(library::LibrarySource::Remote {
            pin: chosen.pin,
            name: chosen.name,
        });
    }

    /// Logs this computer out of the server the library is on: the server takes its token back,
    /// this computer forgets it, and no server is chosen: the welcome view shows.
    pub fn log_out(&mut self) {
        let remote = self.library.remote();
        self.server.log_out(remote.as_deref());
        let pin = self.settings.server.as_ref().and_then(|s| s.pin());
        if let (Some(pin), Some(store)) = (pin, settings::token_store()) {
            store.remove(&freya_client::token_key(&pin));
        }
        self.settings.server = None;
        self.server.set_known(None);
        self.leave_queue();
        self.settings.save();
        self.show_no_library();
    }

    /// Logs in to Freya Server on this computer, as to any server: adopting it first, while
    /// nobody has.
    pub fn use_this_computer(&mut self) {
        self.server.choose_this_computer();
    }

    /// Looks every few seconds whether Freya Server answers on this computer, for the welcome
    /// view, while no server is chosen.
    pub(super) fn look_for_server_here(&mut self) {
        if self.settings.server.is_some() {
            return;
        }
        if self
            .here_looked
            .is_none_or(|at| at.elapsed() >= LOOK_HERE_EVERY)
        {
            self.here_looked = Some(Instant::now());
            self.server_here = crate::gui::server_here();
        }
    }

    /// Whether a server is chosen; until one is, the skin shows only how to connect to one.
    pub(super) fn server_chosen(&self) -> bool {
        self.settings.server.is_some()
    }

    /// Whose library is shown: the certificate of the server elsewhere, in hex, or none for this
    /// computer's.
    pub(super) fn library_from(&self) -> Option<String> {
        self.settings.server.as_ref().map(|s| s.certificate.clone())
    }

    /// Empties Up Next, as another library is shown: what it holds names the other one's
    /// tracks, which are not looked for in this one.
    pub(super) fn leave_queue(&mut self) {
        self.edit(Playlist::clear);
        self.settings.queue_from = self.library_from();
    }

    /// Shows no library, as no server is chosen.
    pub(super) fn show_no_library(&mut self) {
        let mut library = LibraryPanel::new(None, library::covers_dir(None));
        configure_library(&mut library, &self.settings);
        self.library = library;
        self.attach_library();
    }

    /// Shows the library from `source` in place of the one shown.
    pub(super) fn show_library_of(&mut self, source: library::LibrarySource) {
        let covers = library::covers_dir(Some(&source));
        let mut library = LibraryPanel::new(Some(source), covers);
        configure_library(&mut library, &self.settings);
        self.library = library;
        self.attach_library();
    }

    pub fn add_library_folder(&mut self, folder: PathBuf) {
        if add_folder(&mut self.settings.library_folders, folder.clone()) {
            self.library
                .scan_added(&self.settings.library_folders, vec![folder]);
        }
    }

    pub fn remove_library_folder(&mut self, index: usize) {
        if index < self.settings.library_folders.len() {
            self.settings.library_folders.remove(index);
            self.library.rescan(&self.settings.library_folders);
        }
    }

    /// Starts editing the library folders, from those in use.
    pub fn edit_folders(&mut self) {
        self.folders_draft = Some(self.settings.library_folders.clone());
    }

    pub fn add_draft_folder(&mut self, folder: PathBuf) {
        if let Some(draft) = &mut self.folders_draft {
            add_folder(draft, folder);
        }
    }

    pub fn remove_draft_folder(&mut self, index: usize) {
        if let Some(draft) = &mut self.folders_draft
            && index < draft.len()
        {
            draft.remove(index);
        }
    }

    /// Puts the edited folders in use and reads them, once, when they changed.
    pub fn save_folders(&mut self) {
        let Some(draft) = self.folders_draft.take() else {
            return;
        };
        if draft != self.settings.library_folders {
            let before = std::mem::replace(&mut self.settings.library_folders, draft);
            let folders = &self.settings.library_folders;
            // Only new folders are read; one taken away needs every folder looked over.
            if before.iter().all(|f| folders.contains(f)) {
                let added = folders.iter().filter(|f| !before.contains(f)).cloned();
                self.library.scan_added(folders, added.collect());
            } else {
                self.library.rescan(folders);
            }
        }
    }
}
