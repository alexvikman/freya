//! Reaching the library: connecting to its server, and what is said to it and asked of it.

use super::*;

impl LibraryPanel {
    /// Shows the library in a skin window that has the `Library` global. The first time,
    /// connects to the server that keeps it, which brings it in line with its folders.
    pub fn attach(&mut self, ui: &SkinWindow) {
        let _ = ui.set_global_property("Library", "rows", self.rows.model_value());
        let _ = ui.set_global_property("Library", "popular", self.popular_rows.model_value());
        let _ = ui.set_global_property("Library", "letters", self.letter_rows.model_value());
        let _ = ui.set_global_property("Playlists", "entries", self.playlist_rows.model_value());
        let _ = ui.set_global_property(
            "Library",
            "search-sections",
            self.search_section_rows.model_value(),
        );
        let _ =
            ui.set_global_property("Library", "suggestions", self.suggestion_rows.model_value());
        let _ = ui.set_global_property(
            "Library",
            "portrait-choices",
            self.portrait_rows.model_value(),
        );
        let _ = ui.set_global_property(
            "Library",
            "backdrop-choices",
            self.backdrop_rows.model_value(),
        );
        let _ = ui.set_global_property("Library", "artist-matches", self.match_rows.model_value());
        let _ = ui.set_global_property("Library", "album-matches", self.album_rows.model_value());
        self.folders_shown = None;
        self.draft_shown = None;
        if self.db.is_some() || self.connecting.is_some() {
            return;
        }
        let Some(source) = self.source.clone() else {
            return;
        };
        // A server is connected to on a thread of its own: the window shows what it has
        // meanwhile, and the library once it is there.
        #[cfg(test)]
        if let LibrarySource::InProcess { .. } = source {
            let connected = connect(source, self.settings.clone());
            self.connected(connected);
            return;
        }
        let settings = self.settings.clone();
        let (done, connected) = std::sync::mpsc::sync_channel(1);
        let started = std::thread::Builder::new()
            .name("freya-connect".into())
            .spawn(move || {
                let _ = done.send(connect(source, settings));
            });
        match started {
            Ok(_) => self.connecting = Some(connected),
            Err(e) => log::warn!("no library: could not connect: {e}"),
        }
    }

    /// Opens the library `connected` gave, or says why there is none.
    pub(super) fn connected(&mut self, connected: Result<Connection, (&'static str, String)>) {
        self.connecting = None;
        let connection = match connected {
            Ok(connection) => connection,
            Err((state, error)) => {
                self.server_state = state;
                self.server_error = error;
                return;
            }
        };
        match &connection.settings {
            Ok(settings) => self.settings = settings.clone(),
            Err(e) => log::warn!("could not read the server's settings: {e}"),
        }
        self.remote.clone_from(&connection.remote);
        self.server = Some(Arc::clone(&connection.server));
        self.audio.clone_from(&connection.audio);
        self.server_state = "connected";
        self.server_error.clear();
        match connection.events {
            Ok(events) => self.server_events = Some(events),
            Err(e) => log::warn!("could not follow the server: {e}"),
        }
        self.found = Some(connection.found);
        self.open(connection.api);
    }

    /// What connecting to the server found for the app, once, when it was connected to.
    pub fn take_connected(&mut self) -> Option<Connected> {
        self.found.take()
    }

    /// Where the server serves the file of the library's track at `path`, when it is to be
    /// played from there: when the server is on another computer, or always with
    /// `FREYA_STREAM` set, to try it on one computer. A path a server elsewhere gives is never
    /// looked for here: it names a file on that computer, and a hostile server could name one
    /// here, or another computer's share for Windows to log in to. Not connected, such a track
    /// is played from nowhere.
    pub fn stream_url(&self, path: &Path) -> Option<freya_audio::RemoteSource> {
        let source = self.source.as_ref()?;
        let elsewhere = !source.here();
        let nowhere = || freya_audio::RemoteSource {
            url: String::new(),
            bearer: None,
            agent: None,
        };
        if freya_audio::is_url(&path.to_string_lossy()) {
            // Played from the web; one a server elsewhere names, only from far away.
            return None;
        }
        let forced = std::env::var_os("FREYA_STREAM").is_some();
        if !elsewhere && !forced {
            return None;
        }
        let connected = self.remote.as_ref().zip(self.audio.as_ref());
        match connected {
            Some((remote, (audio, agent))) => Some(freya_audio::RemoteSource {
                url: freya_client::audio_url_at(audio, path),
                bearer: remote.0.bearer(),
                agent: Some(Arc::clone(agent)),
            }),
            None if elsewhere => Some(nowhere()),
            None => None,
        }
    }

    /// Whether the library is a server's on another computer, which may name streams only
    /// beyond this computer and its network: [`freya_audio::Media::far_only`].
    pub fn elsewhere(&self) -> bool {
        self.source.as_ref().is_some_and(|source| !source.here())
    }

    /// The server as this client calls it, once connected.
    pub fn remote(&self) -> Option<Arc<freya_protocol::Remote<freya_client::Http>>> {
        self.remote.clone()
    }

    /// Where the library is, for `Server.remote`, `name` and `address`.
    pub fn source_shown(&self) -> (bool, String, String) {
        match &self.source {
            Some(LibrarySource::Remote { pin, name }) => {
                (true, name.clone(), pin.address.to_string())
            }
            _ => (false, String::new(), String::new()),
        }
    }

    /// How the connection to the server goes, and why not, for `Server.state` and `error`.
    pub fn server_state(&self) -> (&'static str, &str) {
        (self.server_state, &self.server_error)
    }

    /// Whether to connect again now: the server could not be reached, a while ago.
    pub fn wants_reconnect(&mut self) -> bool {
        // Once connected, the events follow the server again by themselves.
        let due = matches!(self.server_state, "unreachable" | "version")
            && self.db.is_none()
            && self.tried.is_none_or(|at| at.elapsed() >= RECONNECT_EVERY);
        if due {
            self.tried = Some(Instant::now());
        }
        due
    }

    /// Whether clients on other computers may connect to the server, and what they find it as.
    pub fn set_network(&mut self, on: bool) {
        self.settings.network = on;
        self.tell_settings();
    }

    pub fn set_start_at_login(&mut self, on: bool) {
        self.settings.start_at_login = on;
        self.tell_settings();
    }

    pub fn set_server_name(&mut self, name: &str) {
        self.settings.name = name.trim().to_owned();
        self.tell_settings();
    }

    /// The library, once open.
    pub fn api(&self) -> Option<Arc<dyn LibraryApi>> {
        self.db.clone()
    }

    /// How scrobbling goes, when the server told since this was last asked.
    pub fn take_lastfm_status(&mut self) -> Option<freya_protocol::LastFmStatus> {
        self.lastfm_status.take()
    }

    /// Whether the server told that the playlists changed since this was last asked.
    pub fn take_playlists_changed(&mut self) -> bool {
        std::mem::take(&mut self.playlists_changed)
    }

    /// Whether the server told that the radio stations changed since this was last asked.
    pub fn take_stations_changed(&mut self) -> bool {
        std::mem::take(&mut self.stations_changed)
    }

    /// Whether the server told that the radio directory changed since this was last asked.
    pub fn take_radio_changed(&mut self) -> bool {
        std::mem::take(&mut self.radio_changed)
    }

    /// How the server keeps the library, as the user set it; once it is open.
    pub fn server_settings(&self) -> Option<&ServerSettings> {
        self.server.as_ref().map(|_| &self.settings)
    }

    /// Tells the server the settings as they are now.
    pub(super) fn tell_settings(&self) {
        let settings = self.settings.clone();
        self.tell("change the server's settings", move |server| {
            server.set_settings(&settings)
        });
    }

    /// Tells the server to do something: for one reached over the network on the errands'
    /// thread, the window not waiting for it, after what it was told before.
    pub(super) fn tell(
        &self,
        what: &'static str,
        tell: impl FnOnce(&dyn Server) -> freya_protocol::ApiResult<()> + Send + 'static,
    ) {
        let Some(server) = self.server.clone() else {
            return;
        };
        let told = move || {
            if let Err(e) = tell(server.as_ref()) {
                log::warn!("could not {what}: {e}");
            }
        };
        if self.remote.is_some() {
            crate::errands::run(told);
        } else {
            told();
        }
    }

    /// Shows the library `api`: its rows, and its pictures through thumbnails made in the
    /// covers folder.
    pub(super) fn open(&mut self, api: Arc<dyn LibraryApi>) {
        if let Some(covers) = self.covers_dir.clone() {
            let source = Arc::clone(&api);
            let mut thumbnails = Thumbnails::new(
                covers,
                Arc::new(move |checksum| source.picture(checksum).ok().flatten()),
            );
            // A server makes the covers small, so they are fetched so.
            if self.remote.is_some() {
                let source = Arc::clone(&api);
                thumbnails = thumbnails.with_small(Arc::new(move |checksums| {
                    source
                        .small_pictures(checksums)
                        .unwrap_or_else(|_| vec![None; checksums.len()])
                }));
            }
            self.thumbnails = Some(thumbnails);
        }
        self.finder = PictureFinder::new(Some(Arc::clone(&api)));
        self.db = Some(api);
        self.find_playing_album();
        self.clear_older_cache();
        self.prepare_all();
        self.reload();
    }
}

/// What the covers and pictures are read from, to be asked all at once.
pub(super) fn picture_calls() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ahead("album_covers", &()),
        ahead("genre_infos", &()),
        ahead("artist_pictures", &(PictureKind::Portrait,)),
        ahead("artist_pictures", &(PictureKind::Backdrop,)),
        ahead("all_albums", &()),
        ahead("artists", &()),
    ]
}

/// What is read after the rows of `level` come, to be asked all at once: each genre's albums,
/// for its picture; each album's tracks of an artist's list.
pub(super) fn calls_after(
    api: &dyn LibraryApi,
    level: &Level,
    album_list: bool,
) -> Vec<(&'static str, Vec<u8>)> {
    match level {
        Level::Genres => api
            .genres()
            .unwrap_or_default()
            .iter()
            .map(|genre| ahead("genre_albums", &(genre.name.as_deref(),)))
            .collect(),
        Level::ArtistAlbums { artist } if album_list => api
            .albums(artist.as_deref())
            .unwrap_or_default()
            .iter()
            .map(|album| {
                ahead(
                    "album_tracks",
                    &(album.artist.as_deref(), album.name.as_deref()),
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// What connecting to a server gave: the library, the server, and what the window and the
/// app read of them first, all asked on the thread that connects.
pub(super) struct Connection {
    pub(super) api: Arc<dyn LibraryApi>,
    pub(super) server: Arc<dyn Server>,
    pub(super) remote: Option<Arc<freya_protocol::Remote<freya_client::Http>>>,
    pub(super) settings: freya_protocol::ApiResult<ServerSettings>,
    pub(super) events: freya_protocol::ApiResult<Receiver<ServiceEvent>>,
    pub(super) audio: Option<(String, Arc<ureq::Agent>)>,
    pub(super) found: Connected,
}

/// What connecting to a server found for the app: the account and the server as they say
/// themselves, the playlists, the radio stations, and how scrobbling goes.
pub struct Connected {
    pub server: Arc<dyn Server>,
    pub account: Option<freya_protocol::AccountInfo>,
    pub info: Option<freya_protocol::ServerFound>,
    pub playlists: freya_protocol::ApiResult<Vec<freya_protocol::SavedPlaylist>>,
    pub stations: freya_protocol::ApiResult<Vec<freya_protocol::RadioStation>>,
    pub lastfm: freya_protocol::ApiResult<freya_protocol::LastFmStatus>,
}

/// Connects to the library `source` names, as this computer's device, `settings` the
/// window's for a library in this process: what it gave, or the state to show and why not.
pub(super) fn connect(
    source: LibrarySource,
    settings: ServerSettings,
) -> Result<Connection, (&'static str, String)> {
    #[cfg(not(test))]
    let _ = settings;
    let no_folder = || {
        (
            "unreachable",
            "There is no folder for Freya's files.".to_owned(),
        )
    };
    let (api, server, remote): (Arc<dyn LibraryApi>, Arc<dyn Server>, _) = match source {
        LibrarySource::Remote { pin, .. } => {
            let store = crate::settings::token_store().ok_or_else(no_folder)?;
            let remote = freya_client::connect_pinned(pin, store, crate::settings::cache_dir());
            match remote.0.account() {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Err((
                        "log-in",
                        "The server no longer takes this computer: log in again.".into(),
                    ));
                }
                Err(e) => {
                    log::warn!("could not reach the server: {e}");
                    return Err(("unreachable", e.to_string()));
                }
            }
            (
                Arc::clone(&remote) as _,
                Arc::clone(&remote) as _,
                Some(remote),
            )
        }
        #[cfg(test)]
        LibrarySource::InProcess { database, pictures } => {
            let config = ServiceConfig {
                settings,
                settings_file: None,
                scans_allowed: true,
                lastfm: freya_server::lastfm(),
                playlists: database.parent().map(|dir| dir.join("playlists")),
                stations: database.parent().map(|dir| dir.join("stations.m3u8")),
                radio_directory: None,
                scrobbles: None,
            };
            let service = Service::start(database, pictures, config).map_err(|e| {
                log::warn!("{e}");
                ("unreachable", e)
            })?;
            let service = Arc::new(service);
            (service.api(), service as _, None)
        }
    };
    // For now only a server of Freya's own version: the API still changes between versions.
    if let Some(remote) = &remote {
        let version = remote
            .0
            .version()
            .map_err(|e| ("unreachable", e.to_string()))?;
        freya_client::same_version(&version).map_err(|e| ("version", e))?;
    }
    let audio = remote.as_ref().map(|remote| {
        let (audio, agent) = remote.0.audio();
        (audio, Arc::new(agent))
    });
    let found = Connected {
        server: Arc::clone(&server),
        account: remote.as_ref().and_then(|r| r.0.account().ok().flatten()),
        info: remote.as_ref().and_then(|r| r.0.info().ok()),
        playlists: server.playlists(),
        stations: server.stations(),
        lastfm: server.lastfm_status(),
    };
    Ok(Connection {
        settings: server.settings(),
        events: server.events(),
        api,
        server,
        remote,
        audio,
        found,
    })
}

/// A call to ask for ahead, with its arguments, as [`LibraryApi::prefetch`] takes it.
pub(super) fn ahead(method: &'static str, args: &impl serde::Serialize) -> (&'static str, Vec<u8>) {
    (method, freya_protocol::args(args))
}
