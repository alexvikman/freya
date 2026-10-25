//! The app's state, and the skin window it is shown in.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime};

use freya_audio::{Event, Media, MediaTags, PlaybackState, Player, TrackInfo};
use freya_media::cover::{Cover, cover_from, read_cover};
use freya_media::cue::{self, Found};
use freya_media::{Entry, TagLoader, playlist_file};
use freya_protocol::{LibraryApi, Part, Track};
use freya_skin::SkinSource;
use slint::winit_030::WinitWindowAccessor;
use slint::{ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use slint_interpreter::{ComponentHandle, ComponentInstance, Value};

use super::app_icon;
use super::equalizer::{self, EqualizerSettings};
use super::history::{History, Place};
use super::library::{self, AlbumSort, LibraryPanel};
use super::listening::Listening;
use super::playlists::Playlists;
use super::radio::{self, Stations};
use super::server_panel::{self, ServerPanel};
use super::skin_state::{self, GlobalWriter, ModelRows, Rows};
use super::skin_window::SkinWindow;
use super::system_media::{MediaRequest, SystemMedia};
use super::title_bar::TitleBar;
use super::visualizer::{self, Visualizer};
use super::window_events::{WindowEvent, WindowEvents};
use super::{radio_directory, radio_logos};
use crate::lastfm::{self, LastFm};
use crate::playlist::Playlist;
use crate::settings::{self, Settings};
use crate::update::{self, Updater};

mod covers;
mod navigation;
mod playback;
mod playlists;
mod queue;
mod scrobbling;
mod server;
mod skin;
mod updates;
mod view;

use covers::*;

/// How often the files of a skin in use are checked for changes.
const SKIN_WATCH_INTERVAL: Duration = Duration::from_secs(1);
/// How often Freya looks whether Freya Server answers on this computer, while its library is
/// not the one shown.
const LOOK_HERE_EVERY: Duration = Duration::from_secs(2);

/// A downloaded station playlist, or why it could not be read.
/// `path` from the root, as the working folder makes it; addresses are kept as they are.
fn absolute(path: &Path) -> PathBuf {
    if freya_audio::is_url(&path.to_string_lossy()) {
        return path.to_owned();
    }
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
}

/// Adds `folder` to `folders`, kept sorted. A folder inside one already there adds nothing;
/// one that holds folders already there replaces them. Returns whether `folders` changed.
fn add_folder(folders: &mut Vec<PathBuf>, folder: PathBuf) -> bool {
    if folders.iter().any(|f| folder.starts_with(f)) {
        return false;
    }
    folders.retain(|f| !f.starts_with(&folder));
    folders.push(folder);
    folders.sort();
    true
}

/// A station list downloaded, by its address: the streams it lists, or why it could not be read.
type StationList = (String, Result<Vec<Entry>, String>);

/// Everything tied to one skin window. Replaced as a whole when the skin changes.
/// How long after starting Freya first looks for an update on its own.
const UPDATE_CHECK_DELAY: Duration = Duration::from_secs(10);

/// What the radio directory's rows were made of: the directory's and the logos' versions, the
/// stations the server keeps, and the stream playing.
type DirectoryShown = ((u64, u64), Vec<String>, Option<String>);

pub struct View {
    pub ui: SkinWindow,
    rows: Rows,
    player_out: GlobalWriter,
    playlist_out: GlobalWriter,
    app_out: GlobalWriter,
    eq_out: GlobalWriter,
    library_out: GlobalWriter,
    radio_out: GlobalWriter,
    station_rows: ModelRows,
    /// The radio directory's rows: the stations shown, the first page's rows, the tags to
    /// choose and those chosen; and what they were made of when last given to the skin.
    directory_rows: [ModelRows; 4],
    directory_shown: Option<DirectoryShown>,
    playlists_out: GlobalWriter,
    updates_out: GlobalWriter,
    lastfm_out: GlobalWriter,
    server_out: GlobalWriter,
    playlist_list_rows: ModelRows,
    playlist_track_rows: ModelRows,
    activity_rows: ModelRows,
    /// What was logged, as last given to the skin: the entries, at which level, and how many
    /// had been logged then.
    log_rows: ModelRows,
    log_shown: Option<(log::Level, u64)>,
    /// The playlists' version and the playlist shown when their rows were last given to the
    /// skin; rows are only rebuilt when either changes.
    playlists_synced: Option<(u64, Option<usize>)>,
    /// Band gains last given to the skin. A new model is set whenever they change, so the
    /// skin's bindings that read the whole list (like the curve) are re-evaluated.
    eq_bands_shown: Option<[f32; 10]>,
    pub visualizer: Visualizer,
    title_bar: TitleBar,
    /// Whether the window has Freya's icon yet.
    icon_set: bool,
}

impl View {
    pub fn new(ui: SkinWindow) -> Self {
        let mut view = Self {
            rows: Rows::new(),
            player_out: GlobalWriter::new(&ui, "Player"),
            playlist_out: GlobalWriter::new(&ui, "Playlist"),
            app_out: GlobalWriter::new(&ui, "App"),
            eq_out: GlobalWriter::new(&ui, "Equalizer"),
            library_out: GlobalWriter::new(&ui, "Library"),
            radio_out: GlobalWriter::new(&ui, "Radio"),
            station_rows: ModelRows::new(),
            directory_rows: [
                ModelRows::new(),
                ModelRows::new(),
                ModelRows::new(),
                ModelRows::new(),
            ],
            directory_shown: None,
            playlists_out: GlobalWriter::new(&ui, "Playlists"),
            updates_out: GlobalWriter::new(&ui, "Updates"),
            lastfm_out: GlobalWriter::new(&ui, "LastFm"),
            server_out: GlobalWriter::new(&ui, "Server"),
            playlist_list_rows: ModelRows::new(),
            playlist_track_rows: ModelRows::new(),
            activity_rows: ModelRows::new(),
            log_rows: ModelRows::new(),
            log_shown: None,
            playlists_synced: None,
            eq_bands_shown: None,
            visualizer: Visualizer::new(&ui),
            ui,
            title_bar: TitleBar::default(),
            icon_set: false,
        };
        watch_frames(&view.ui, view.title_bar.after_frame());
        if view.playlist_out.available {
            let _ = view
                .ui
                .set_global_property("Playlist", "rows", view.rows.model_value());
        }
        if view.radio_out.available {
            let _ =
                view.ui
                    .set_global_property("Radio", "stations", view.station_rows.model_value());
            for (name, rows) in ["found", "shelves", "tags", "chosen"]
                .into_iter()
                .zip(&view.directory_rows)
            {
                let _ = view
                    .ui
                    .set_global_property("Radio", name, rows.model_value());
            }
        }
        if view.playlists_out.available {
            let _ = view.ui.set_global_property(
                "Playlists",
                "playlists",
                view.playlist_list_rows.model_value(),
            );
            let _ = view.ui.set_global_property(
                "Playlists",
                "tracks",
                view.playlist_track_rows.model_value(),
            );
        }
        if view.app_out.available {
            let _ =
                view.ui
                    .set_global_property("App", "activities", view.activity_rows.model_value());
            let _ = view
                .ui
                .set_global_property("App", "log", view.log_rows.model_value());
        }
        let version = skin_state::string(env!("CARGO_PKG_VERSION"));
        let built = skin_state::string(skin_state::build_date().unwrap_or_default());
        view.app_out
            .write(&view.ui, vec![("version", version), ("build-date", built)]);
        if view.eq_out.available {
            // Lists that never change are set once.
            let strings = |items: Vec<String>| {
                let values: Vec<Value> = items.iter().map(skin_state::string).collect();
                Value::Model(ModelRc::new(VecModel::from(values)))
            };
            let _ = view.ui.set_global_property(
                "Equalizer",
                "band-labels",
                strings(equalizer::band_labels()),
            );
            let _ = view.ui.set_global_property(
                "Equalizer",
                "presets",
                strings(equalizer::preset_names()),
            );
        }
        view
    }
}

/// When Freya started, for the time to its first frame.
pub static STARTED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// After each frame of `ui`'s window: `after_frame`, and with debug logging, how many frames
/// are drawn; after the first frame of all, how long Freya took to show it.
fn watch_frames(ui: &SkinWindow, after_frame: impl Fn(&slint::Window) + 'static) {
    static FIRST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
    let weak = ui.downgrade();
    let mut frames = 0u32;
    let mut counted_since = Instant::now();
    let notified = ui.window().set_rendering_notifier(move |state, _| {
        if !matches!(state, slint::RenderingState::AfterRendering) {
            return;
        }
        if let Some(ui) = weak.upgrade() {
            after_frame(ui.window());
        }
        if FIRST.swap(false, std::sync::atomic::Ordering::Relaxed)
            && let Some(started) = STARTED.get()
        {
            log::info!("first frame after {:?}", started.elapsed());
        }
        frames += 1;
        if counted_since.elapsed() >= Duration::from_secs(5) {
            let seconds = counted_since.elapsed().as_secs_f32();
            log::debug!("{:.1} frames per second", frames as f32 / seconds);
            (frames, counted_since) = (0, Instant::now());
        }
    });
    if let Err(e) = notified {
        log::debug!("frames are not watched: {e}");
    }
}

pub struct App {
    pub player: Player,
    /// While muted, the volume to go back to; the player plays at 0.
    muted: Option<f32>,
    /// Whether Freya Server answers on this computer, and when that was last looked at: for
    /// the welcome view and the Server tab, while the library is not this computer's.
    server_here: bool,
    here_looked: Option<Instant>,
    /// Until the window is open: whether to make it large then, as on its first start where
    /// the screen was not known before (see [`super::screen`]).
    pub settle_window: Option<bool>,
    pub playlist: Playlist,
    pub settings: Settings,
    tags: TagLoader,
    window_events: WindowEvents,
    /// Whether the skin window is hidden (covered, minimized) and whether it has focus, as
    /// far as the system says.
    window_hidden: bool,
    window_focused: bool,
    /// What the Activity page shows of the log: entries at this level or worse.
    log_level: log::Level,
    covers: Covers,
    /// The cover shown, kept so a new skin can show it too, and its file for the system.
    cover: Option<slint::Image>,
    cover_file: Option<PathBuf>,
    /// Media keys and the system's Now Playing display.
    pub system_media: Option<SystemMedia>,
    /// Tries left to connect to the system's media controls, made from the ticks: Windows
    /// ties them to the window, which only exists once the event loop has shown it.
    pub media_tries: u8,
    track: Option<Arc<TrackInfo>>,
    /// Whether the track playing has been listened to long enough to count as played.
    listening: Listening,
    pub playlist_dirty: bool,
    /// Station playlists being downloaded; they arrive here.
    station_lists: (Sender<StationList>, Receiver<StationList>),
    /// Why the last track could not be played, shown until another one starts.
    pub error: String,
    pub library: LibraryPanel,
    pub stations: Stations,
    /// The server's radio directory, as the radio view browses it.
    pub radio: radio_directory::Directory,
    /// The radio stations' logos, as they are fetched.
    pub radio_logos: radio_logos::Logos,
    pub playlists: Playlists,
    /// The playlist shown in the skin.
    pub shown_playlist: Option<usize>,
    /// Where the window has been, to go back and forward; and how many times a place was
    /// gone back or forward to, which tells the skin to scroll it as it was.
    history: History,
    restores: u32,
    /// Why the last playlist could not be made or renamed.
    pub playlists_error: String,
    /// The stream each station list (`.pls`, `.m3u`) led to, by the list's address.
    station_streams: HashMap<String, String>,
    /// Why the last station could not be saved.
    pub radio_error: String,
    pub view: View,

    /// The skin in use.
    pub skin: SkinSource,
    /// A skin to switch to on the next tick; set by the skin picker and by file watching.
    pub pending_skin: Option<SkinSource>,
    /// Why the last skin change failed, shown in the skin picker.
    pub skin_error: String,
    /// The saved skin, while it could not be loaded and the built-in one stands in: kept in
    /// the settings (a package on a drive that is not plugged in comes back), until the user
    /// chooses a skin.
    pub unloaded_skin: Option<PathBuf>,
    /// The library folders while the user edits them; only saving puts them in use.
    pub folders_draft: Option<Vec<PathBuf>>,
    skin_files: Vec<(PathBuf, Option<SystemTime>)>,
    skin_checked: Instant,

    about: Option<ComponentInstance>,
    pub picker: Option<ComponentInstance>,
    /// The skins listed in the picker, in order.
    pub picker_skins: Vec<freya_skin::SkinInfo>,

    pub updater: Updater,
    started: Instant,
    pub lastfm: LastFm,
    /// The server the library is on, as the user chooses it, and the account.
    pub server: ServerPanel,
    /// The entry asked to play, by its id, until it starts or fails: an error about any other
    /// (one from before Stop, say) does not move playback on.
    requested: Option<u64>,
    /// The entry queued to follow gaplessly.
    armed: Option<u64>,
    /// A queued entry that could not be opened: once what plays now has ended, playback goes
    /// on with the entry after it.
    skip_after: Option<u64>,
    /// Entries that failed to open since the last that played, so that a playlist where none
    /// can be played (a drive unplugged, with repeat on) is not tried round and round.
    failures: usize,
    /// The play Last.fm hears about, if the track may be scrobbled at all.
    lastfm_play: Option<LastFmPlay>,
    /// Whether something played at the last tick, so a resume can tell Last.fm again.
    lastfm_was_playing: bool,
}

/// What Last.fm is told about the track that plays.
struct LastFmPlay {
    track: lastfm::Track,
    /// When it started, in seconds since 1970.
    started: u64,
    /// Whether it may be scrobbled once heard enough: not when 30 seconds or shorter.
    scrobble: bool,
}

/// The music the user keeps in Freya, handed to the app when it starts.
pub struct Collections {
    pub library: LibraryPanel,
    pub stations: Stations,
    pub playlists: Playlists,
}

impl App {
    pub fn new(
        ui: SkinWindow,
        skin: SkinSource,
        player: Player,
        settings: Settings,
        window_events: WindowEvents,
        collections: Collections,
    ) -> Self {
        let Collections {
            mut library,
            stations,
            playlists,
        } = collections;
        configure_library(&mut library, &settings);
        let lastfm = LastFm::new();
        let mut server = ServerPanel::new();
        server.set_known(
            settings
                .server
                .as_ref()
                .and_then(|s| s.pin())
                .map(|pin| pin.certificate),
        );
        let mut app = Self {
            player,
            muted: None,
            server_here: false,
            here_looked: None,
            settle_window: None,
            playlist: Playlist::new(),
            settings,
            tags: TagLoader::new(),
            window_events,
            window_hidden: false,
            window_focused: true,
            log_level: log::Level::Info,
            covers: Covers::new(),
            cover: None,
            cover_file: None,
            system_media: None,
            media_tries: 0,
            track: None,
            listening: Listening::default(),
            playlist_dirty: true,
            station_lists: channel(),
            error: String::new(),
            library,
            stations,
            radio: radio_directory::Directory::new(),
            radio_logos: radio_logos::Logos::new(settings::cache_dir()),
            playlists,
            history: History::default(),
            restores: 0,
            shown_playlist: None,
            playlists_error: String::new(),
            station_streams: HashMap::new(),
            radio_error: String::new(),
            view: View::new(ui),
            skin,
            pending_skin: None,
            skin_error: String::new(),
            unloaded_skin: None,
            folders_draft: None,
            skin_files: Vec::new(),
            skin_checked: Instant::now(),
            about: None,
            picker: None,
            picker_skins: Vec::new(),
            updater: Updater::new(),
            started: Instant::now(),
            lastfm,
            server,
            requested: None,
            armed: None,
            skip_after: None,
            failures: 0,
            lastfm_play: None,
            lastfm_was_playing: false,
        };
        app.watch_skin();
        app.attach_library();
        app
    }

    /// Plays files from the command line, or restores the saved playlist.
    pub fn start(&mut self, files: &[PathBuf]) {
        if !files.is_empty() {
            self.replace_and_play(files);
            return;
        }
        let Some(path) = settings::playlist_path().filter(|p| p.exists()) else {
            return;
        };
        // Up Next of another library names its tracks: not opened here, nor looked for.
        if self.settings.queue_from != self.library_from() {
            log::info!("Up Next was of another library: not restored");
            return;
        }
        match playlist_file::load(&path) {
            Ok(entries) => {
                self.playlist.insert(None, entries);
                self.request_missing_tags();
                self.playlist_dirty = true;
            }
            Err(e) => log::warn!("could not restore playlist: {e}"),
        }
    }

    pub fn tick(&mut self) {
        self.player.rest_device_when_idle();
        self.look_for_server_here();
        if let Some(make_large) = self.settle_window
            && self.view.ui.window().with_winit_window(|_| ()).is_some()
        {
            super::screen::settle(self.view.ui.window(), make_large);
            self.settle_window = None;
        }
        while let Ok(event) = self.player.events().try_recv() {
            match event {
                Event::TrackStarted(track) => {
                    self.error.clear();
                    self.failures = 0;
                    if self.requested == Some(track.id) {
                        self.requested = None;
                    }
                    self.name_station(&track);
                    // By its id: another entry may play the same file.
                    match self.playlist.index_of(track.id) {
                        Some(i) => self.playlist.set_current(Some(i)),
                        None => self.playlist.set_current_track(&track.path, track.start),
                    }
                    self.listening.start(self.countable(&track), track.duration);
                    self.lastfm_started(&track);
                    let remote = self.library.stream_url(&track.path).and(self.library.api());
                    self.covers.request(&track.path, remote);
                    self.track = Some(track);
                    self.playlist_dirty = true;
                    self.queue_next();
                }
                Event::Finished => {
                    // The entry queued next could not be opened: go on with the one after it.
                    if let Some(i) = self
                        .skip_after
                        .take()
                        .and_then(|id| self.playlist.index_of(id))
                    {
                        self.playlist.set_current(Some(i));
                        self.step(true);
                    }
                }
                Event::Unplayable(id, msg) => {
                    log::warn!("{msg}");
                    self.error = msg;
                    self.failures += 1;
                    if self.failures >= self.playlist.len().max(1) {
                        // None of them plays: stop rather than try them round and round.
                        self.requested = None;
                        self.skip_after = None;
                    } else if self.requested == Some(id) {
                        // Asked to play, and nothing plays: skip to the entry after it.
                        self.requested = None;
                        if let Some(i) = self.playlist.index_of(id) {
                            self.playlist.set_current(Some(i));
                            self.step(true);
                        }
                    } else if self.armed == Some(id) {
                        self.skip_after = Some(id);
                    }
                }
                Event::Error(msg) => {
                    log::warn!("{msg}");
                    self.error = msg;
                }
            }
        }

        while let Ok((list, result)) = self.station_lists.1.try_recv() {
            match result {
                Ok(entries) => {
                    let stream = entries[0].path.to_string_lossy().into_owned();
                    // Played before, from this list or another way: not added again.
                    match self.entry_for(&stream) {
                        Some(index) => self.play_index(index),
                        None => self.add_and_play(entries),
                    }
                    self.station_streams.insert(list, stream);
                }
                Err(e) => {
                    log::warn!("could not read station playlist {e}");
                    self.error = format!("Could not read the station list: {e}");
                }
            }
        }

        let tags: HashMap<PathBuf, freya_media::TagInfo> = self.tags.poll().collect();
        if !tags.is_empty() {
            self.playlist.apply_all_tags(&tags);
            self.playlist_dirty = true;
        }

        let mut dropped = Vec::new();
        let main_window = self.view.ui.window().with_winit_window(|w| w.id());
        for event in self.window_events.take() {
            match event {
                WindowEvent::FileHover(on) => self
                    .view
                    .playlist_out
                    .write(&self.view.ui, vec![("drop-hover", Value::Bool(on))]),
                WindowEvent::FileDropped(path) => dropped.push(path),
                WindowEvent::Occluded(id, hidden) if Some(id) == main_window => {
                    log::debug!("window hidden: {hidden}");
                    self.window_hidden = hidden;
                }
                WindowEvent::Focused(id, focused) if Some(id) == main_window => {
                    log::debug!("window focused: {focused}");
                    self.window_focused = focused;
                }
                WindowEvent::Navigate(false) => {
                    self.go_back();
                    self.note_place();
                }
                WindowEvent::Navigate(true) => {
                    self.go_forward();
                    self.note_place();
                }
                WindowEvent::Occluded(..) | WindowEvent::Focused(..) => {}
            }
        }
        if !dropped.is_empty() {
            let first_new = self.playlist.len();
            let idle = self.player.status().state == PlaybackState::Stopped;
            self.add_paths(&dropped);
            // Dropping music onto an idle player starts it.
            if idle && first_new < self.playlist.len() {
                self.play_index(first_new);
            }
        }

        // With nothing to play, the cover of what played last goes too.
        if self.cover.is_some()
            && self.playlist.current_entry().is_none()
            && self.player.status().state == PlaybackState::Stopped
        {
            self.covers.wanted = None;
            self.cover = None;
            self.cover_file = None;
            show_cover(&self.view.ui, None);
        }
        if let Some(cover) = self.covers.poll() {
            self.cover_file = cover.as_ref().and_then(|(_, file)| file.clone());
            self.cover = cover.map(|(c, _)| {
                let buffer =
                    SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&c.rgba, c.width, c.height);
                slint::Image::from_rgba8(buffer)
            });
            show_cover(&self.view.ui, self.cover.clone());
        }

        if self.system_media.is_none() && self.media_tries > 0 {
            self.media_tries -= 1;
            self.system_media = SystemMedia::new(self.view.ui.window());
        }
        let requests: Vec<MediaRequest> = self
            .system_media
            .as_ref()
            .map(|media| media.poll().collect())
            .unwrap_or_default();
        for request in requests {
            self.handle_media_request(request);
        }

        let status = self.player.status();
        let playing = status.state == PlaybackState::Playing;
        if let Some((path, part)) = self.listening.update(status.position, playing) {
            self.library.record_play(path, part);
            self.lastfm_heard();
        }
        // After a pause, Last.fm shows the track as playing again.
        if playing && !self.lastfm_was_playing {
            self.lastfm_now_playing();
        }
        self.lastfm_was_playing = playing;

        self.library.poll();
        self.take_connected();
        if let Some(chosen) = self.server.poll() {
            self.use_remote_server(chosen);
        }
        self.server.load_update(self.library.remote());
        if self.server.take_refresh_wanted() {
            let remote = self.library.remote();
            self.server.refresh(remote.as_deref());
            self.server.load_users(remote);
        }
        if self.library.wants_reconnect() {
            self.attach_library();
        }
        // Changed by another client, or by this one.
        if self.library.take_playlists_changed() {
            self.playlists.reload();
        }
        if self.library.take_stations_changed() {
            self.stations.reload();
        }
        if self.library.take_radio_changed() {
            self.radio.reload();
        }
        self.radio.poll();
        self.radio_logos.poll();
        if let Some(status) = self.library.take_lastfm_status() {
            self.lastfm.set_status(status);
        }
        self.follow_updates();
        self.check_skin_files();
        self.refresh();
    }

    /// Whether the app has things to follow closely: the library is being read.
    pub fn is_busy(&self) -> bool {
        self.library.is_scanning()
    }

    pub fn save(&mut self) {
        // Muted, the volume from before: the next start plays.
        self.settings.volume = self.muted.unwrap_or_else(|| self.player.volume());
        let window = self.view.ui.window();
        // In full screen the window has the screen's size, not its own; maximized, the size and
        // place it had before stay, which it goes back to when no longer maximized.
        if !window.is_fullscreen() {
            self.settings.window_maximized = window.is_maximized();
            if !window.is_maximized() && !window.is_minimized() {
                let size = window.size().to_logical(window.scale_factor());
                // Restored as the size under the system's title bar.
                let (width, height) = self.view.title_bar.added();
                self.settings.window_size = Some((size.width - width, size.height - height));
                let at = window.position();
                self.settings.window_position = Some((at.x, at.y));
            }
        }
        self.settings.skin = self
            .skin
            .path()
            .map(Path::to_owned)
            .or_else(|| self.unloaded_skin.clone());
        self.settings.queue_from = self.library_from();
        self.settings.save();
        crate::errands::finish();
        self.library.save();
        if let Some(path) = settings::playlist_path() {
            let result = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| playlist_file::save(&path, self.playlist.entries()));
            if let Err(e) = result {
                log::warn!("could not save playlist: {e}");
            }
        }
    }
}

/// Gives `library` how the user set the library up, before it is connected.
fn configure_library(library: &mut LibraryPanel, settings: &Settings) {
    if let Some(sort) = AlbumSort::from_name(&settings.album_sort) {
        library.set_album_sort(sort);
    }
    library.set_album_list(settings.album_view == "list");
    if let Some(order) = library::artist_order(&settings.artist_sort) {
        library.set_artist_order(order);
    }
    library.set_artist_list(settings.artist_view == "list");
    library.set_albums_order(&settings.albums_sort);
    library.set_playlists_order(&settings.playlists_sort);
}

/// What the player plays for a playlist entry: its file, or for a track of a CUE sheet, the
/// part of the file the track is, with the track's own tags; read from the server when
/// `library` says it is played from there.
fn media(entry: &Entry, library: &LibraryPanel) -> Media {
    let remote = library.stream_url(&entry.path);
    let Some(part) = entry.part else {
        return Media {
            id: entry.id,
            remote,
            far_only: library.elsewhere(),
            ..Media::from(&entry.path)
        };
    };
    Media {
        path: entry.path.clone(),
        start: part.start,
        end: part.end,
        tags: MediaTags {
            title: entry.title.clone(),
            artist: entry.artist.clone(),
            album: entry.album.clone(),
        },
        id: entry.id,
        remote,
        far_only: library.elsewhere(),
    }
}
