//! The app's state, and the skin window it is shown in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime};

use freya_audio::{Event, Media, MediaTags, PlaybackState, Player, TrackInfo};
use freya_library::cover::{Cover, read_cover};
use freya_library::cue::{self, Found, Part};
use freya_library::library::Track;
use freya_library::{Entry, Playlist, TagLoader, playlist_file};
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
use super::skin_state::{self, GlobalWriter, ModelRows, Rows};
use super::skin_window::SkinWindow;
use super::system_media::{MediaRequest, SystemMedia};
use super::title_bar::TitleBar;
use super::visualizer::{self, Visualizer};
use super::window_events::{WindowEvent, WindowEvents};
use crate::lastfm::{self, LastFm};
use crate::settings::{self, Settings};
use crate::update::{self, Updater};

/// Covers are decoded at most this large; the built-in skin shows them at 88 points.
const COVER_SIZE: u32 = 512;
/// How often the files of a skin in use are checked for changes.
const SKIN_WATCH_INTERVAL: Duration = Duration::from_secs(1);

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
    playlists_out: GlobalWriter,
    updates_out: GlobalWriter,
    lastfm_out: GlobalWriter,
    playlist_list_rows: ModelRows,
    playlist_track_rows: ModelRows,
    activity_rows: ModelRows,
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
            playlists_out: GlobalWriter::new(&ui, "Playlists"),
            updates_out: GlobalWriter::new(&ui, "Updates"),
            lastfm_out: GlobalWriter::new(&ui, "LastFm"),
            playlist_list_rows: ModelRows::new(),
            playlist_track_rows: ModelRows::new(),
            activity_rows: ModelRows::new(),
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

/// A decoded cover, and the PNG file it was saved to for the system's Now Playing.
type CoverResult = (PathBuf, Option<(Cover, Option<PathBuf>)>);

/// Decodes cover art on a background thread, one track at a time.
struct Covers {
    tx: Sender<CoverResult>,
    rx: Receiver<CoverResult>,
    wanted: Option<PathBuf>,
}

impl Covers {
    fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            wanted: None,
        }
    }

    fn request(&mut self, path: &Path) {
        if self.wanted.as_deref() == Some(path) {
            return;
        }
        self.wanted = Some(path.to_owned());
        let tx = self.tx.clone();
        let path = path.to_owned();
        std::thread::spawn(move || {
            let cover = read_cover(&path, COVER_SIZE).map(|cover| {
                let file = save_cover_file(&path, &cover);
                (cover, file)
            });
            let _ = tx.send((path, cover));
        });
    }

    /// The newest cover for the wanted track, if one arrived.
    fn poll(&self) -> Option<Option<(Cover, Option<PathBuf>)>> {
        let mut latest = None;
        for (path, cover) in self.rx.try_iter() {
            if self.wanted.as_ref() == Some(&path) {
                latest = Some(cover);
            }
        }
        if let Some(Some((_, Some(file)))) = &latest {
            remove_other_cover_files(file);
        }
        latest
    }
}

/// Where covers are saved for the system's Now Playing display.
fn cover_files_dir() -> Option<PathBuf> {
    Some(settings::cache_dir()?.join("now-playing"))
}

/// Saves `cover` for the system's Now Playing display. Named after the track, so the system
/// does not show an old image it cached. Earlier ones are removed once this one is shown, from
/// the UI thread: a slower cover saved after it would otherwise remove it.
fn save_cover_file(track: &Path, cover: &Cover) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let dir = cover_files_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    track.hash(&mut hasher);
    let file = dir.join(format!("cover-{:016x}.png", hasher.finish()));
    match cover.save_png(&file) {
        Ok(()) => Some(file),
        Err(e) => {
            log::debug!("could not save the cover for Now Playing: {e}");
            None
        }
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

/// Removes the saved covers but `keep`.
fn remove_other_cover_files(keep: &Path) {
    let Some(entries) = cover_files_dir().and_then(|dir| std::fs::read_dir(dir).ok()) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.path() != keep {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub struct App {
    pub player: Player,
    pub playlist: Playlist,
    pub settings: Settings,
    tags: TagLoader,
    window_events: WindowEvents,
    /// Whether the skin window is hidden (covered, minimized) and whether it has focus, as
    /// far as the system says.
    window_hidden: bool,
    window_focused: bool,
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
        if let Some(sort) = AlbumSort::from_name(&settings.album_sort) {
            library.set_album_sort(sort);
        }
        library.set_album_list(settings.album_view == "list");
        if let Some(order) = library::artist_order(&settings.artist_sort) {
            library.set_artist_order(order);
        }
        library.set_artist_list(settings.artist_view == "list");
        library.set_online_pictures(settings.artist_pictures_online);
        library.set_online_bios(settings.artist_bios_online);
        library.set_online_wikipedia(settings.artist_wikipedia_online);
        library.set_fanart_key(settings.fanart_key.clone());
        let lastfm = LastFm::start(settings.lastfm_user.clone());
        let mut app = Self {
            player,
            playlist: Playlist::new(),
            settings,
            tags: TagLoader::new(),
            window_events,
            window_hidden: false,
            window_focused: true,
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
        match playlist_file::load(&path) {
            Ok(entries) => {
                self.playlist.insert(None, entries);
                self.request_missing_tags();
                self.playlist_dirty = true;
            }
            Err(e) => log::warn!("could not restore playlist: {e}"),
        }
    }

    // ----- Playback ----------------------------------------------------------------------------

    pub fn play_index(&mut self, index: usize) {
        let Some(entry) = self.playlist.get(index) else {
            return;
        };
        let media = media(entry);
        self.requested = Some(media.id);
        self.skip_after = None;
        self.playlist.set_current(Some(index));
        self.covers.request(&media.path);
        self.player.play(media);
        self.playlist_dirty = true;
        // What follows this entry, not the one queued after the last.
        self.queue_next();
    }

    pub fn stop(&mut self) {
        self.requested = None;
        self.skip_after = None;
        self.player.stop();
    }

    pub fn play(&mut self) {
        match self.player.status().state {
            PlaybackState::Paused => self.player.set_paused(false),
            PlaybackState::Playing => {}
            PlaybackState::Stopped => {
                let index = self
                    .playlist
                    .current()
                    .or_else(|| self.playlist.selected_indices().next())
                    .unwrap_or(0);
                self.play_index(index);
            }
        }
    }

    pub fn toggle_pause(&mut self) {
        match self.player.status().state {
            PlaybackState::Playing => self.player.set_paused(true),
            _ => self.play(),
        }
    }

    pub fn step(&mut self, forward: bool) {
        let (shuffle, repeat) = (self.settings.shuffle, self.settings.repeat);
        let target = if forward {
            self.playlist.peek_next(shuffle, repeat)
        } else {
            self.playlist.peek_previous(shuffle, repeat)
        };
        if let Some(i) = target {
            self.play_index(i);
        }
    }

    pub fn seek(&mut self, fraction: f64) {
        if let Some(length) = self.player.status().track.and_then(|t| t.duration) {
            self.player.seek(length.mul_f64(fraction.clamp(0.0, 1.0)));
        }
    }

    pub fn seek_by(&mut self, seconds: f64) {
        let position = self.player.status().position.as_secs_f64();
        self.player
            .seek(Duration::from_secs_f64((position + seconds).max(0.0)));
    }

    pub fn toggle_shuffle(&mut self) {
        self.settings.shuffle = !self.settings.shuffle;
        self.queue_next();
    }

    pub fn toggle_repeat(&mut self) {
        self.settings.repeat = !self.settings.repeat;
        self.queue_next();
    }

    /// Tells the engine which track follows the current one, for gapless playback.
    fn queue_next(&mut self) {
        let next = self
            .playlist
            .peek_next(self.settings.shuffle, self.settings.repeat)
            .and_then(|i| self.playlist.get(i))
            .map(media);
        self.armed = next.as_ref().map(|m| m.id);
        self.player.set_next(next);
    }

    // ----- Playlist ----------------------------------------------------------------------------

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
            let found = freya_library::files::collect_with_sheets(std::slice::from_ref(path));
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
    fn entry_for(&self, url: &str) -> Option<usize> {
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
    fn add_and_play(&mut self, entries: Vec<Entry>) {
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

    fn request_missing_tags(&self) {
        for entry in self.playlist.entries().iter().filter(|e| !e.tags_loaded) {
            if !entry.path.to_string_lossy().contains("://") {
                self.tags.request(entry.path.clone());
            }
        }
    }

    // ----- Library -----------------------------------------------------------------------------

    /// Shows the library, if the skin has one.
    fn attach_library(&mut self) {
        if self.view.library_out.available {
            self.library.attach(
                &self.view.ui,
                &self.settings.library_folders,
                !self.settings.damaged,
            );
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

    // ----- Playlists ---------------------------------------------------------------------------

    /// The entries of the playlist shown.
    pub fn shown_playlist_entries(&self) -> Vec<Entry> {
        self.shown_playlist
            .and_then(|i| self.playlists.get(i))
            .map(|p| p.entries.clone())
            .unwrap_or_default()
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

    // ----- Back and forward ----------------------------------------------------------------------

    /// The place the window shows, by the skin's `App.view` and what the app has open; `None`
    /// for skins without views.
    fn place(&self) -> Option<Place> {
        let Ok(Value::String(view)) = self.view.ui.get_global_property("App", "view") else {
            return None;
        };
        let view = view.to_string();
        if view.is_empty() {
            return None;
        }
        Some(Place {
            library: (view == "library").then(|| self.library.place()),
            playlist: self.shown_playlist.filter(|_| view == "playlist"),
            view,
            scroll: 0.0,
        })
    }

    /// How far down the skin says the place shown is scrolled.
    fn shown_scroll(&self) -> f32 {
        match self.view.ui.get_global_property("App", "scroll") {
            Ok(Value::Number(scroll)) => scroll as f32,
            _ => 0.0,
        }
    }

    /// Notes the place shown in the history, after anything the skin asked for.
    pub fn note_place(&mut self) {
        let Some(place) = self.place() else {
            return;
        };
        let scroll = self.shown_scroll();
        if self.history.visit(place, scroll) {
            // A place reached anew starts at its top, not where a restored one was.
            let _ = self
                .view
                .ui
                .set_global_property("App", "restore-view", skin_state::string(""));
        }
    }

    pub fn go_back(&mut self) {
        let scroll = self.shown_scroll();
        if let Some(place) = self.history.back(scroll).cloned() {
            self.show_place(&place);
        }
    }

    pub fn go_forward(&mut self) {
        let scroll = self.shown_scroll();
        if let Some(place) = self.history.forward(scroll).cloned() {
            self.show_place(&place);
        }
    }

    /// Shows `place` again: the library or playlist as it was, the skin's view, and how far
    /// down it was scrolled.
    /// Shows the skin's `view`, as the user would by choosing it.
    pub fn show_view(&mut self, view: &str) {
        let _ = self
            .view
            .ui
            .set_global_property("App", "view", skin_state::string(view));
    }

    fn show_place(&mut self, place: &Place) {
        if let Some(library) = &place.library {
            self.library.go_to(library);
        }
        if place.view == "playlist" {
            self.shown_playlist = place.playlist;
        }
        self.restores += 1;
        let ui = &self.view.ui;
        let scroll = Value::Number(f64::from(place.scroll));
        let _ = ui.set_global_property("App", "view", skin_state::string(&place.view));
        let _ = ui.set_global_property("App", "scroll", scroll.clone());
        let _ = ui.set_global_property("App", "restore-scroll", scroll);
        let _ = ui.set_global_property("App", "restore-view", skin_state::string(&place.view));
        let _ = ui.set_global_property("App", "restores", Value::Number(f64::from(self.restores)));
    }

    pub fn add_library_folder(&mut self, folder: PathBuf) {
        if add_folder(&mut self.settings.library_folders, folder) {
            self.library.rescan(&self.settings.library_folders);
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
            self.settings.library_folders = draft;
            self.library.rescan(&self.settings.library_folders);
        }
    }

    /// Changes the equalizer and sends the result to the audio engine.
    pub fn edit_eq(&mut self, change: impl FnOnce(&mut EqualizerSettings)) {
        change(&mut self.settings.equalizer);
        self.player.set_equalizer(&self.settings.equalizer.engine());
    }

    // ----- Skins -------------------------------------------------------------------------------

    /// Puts a newly loaded skin in place of the current one. The returned view is the old one,
    /// whose window the caller hides.
    pub fn replace_view(&mut self, ui: SkinWindow, skin: SkinSource) -> View {
        // The new view starts with the visualization tap off; it turns it on when wanted.
        self.player.set_visualization(false);
        let old = std::mem::replace(&mut self.view, View::new(ui));
        (self.window_hidden, self.window_focused) = (false, true);
        self.skin = skin;
        self.skin_error.clear();
        self.unloaded_skin = None;
        self.playlist_dirty = true;
        show_cover(&self.view.ui, self.cover.clone());
        self.watch_skin();
        self.attach_library();
        self.refresh();
        old
    }

    /// Remembers the modification times of the skin's files, to notice when they change.
    pub fn watch_skin(&mut self) {
        self.skin_files = self
            .skin
            .watched_files()
            .into_iter()
            .map(|p| {
                let modified = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
                (p, modified)
            })
            .collect();
        self.skin_checked = Instant::now();
    }

    /// Asks for the skin to be reloaded if any of its files changed since it was loaded.
    fn check_skin_files(&mut self) {
        if self.skin_files.is_empty() || self.skin_checked.elapsed() < SKIN_WATCH_INTERVAL {
            return;
        }
        self.skin_checked = Instant::now();
        let current: Vec<PathBuf> = self.skin.watched_files();
        let changed = current.len() != self.skin_files.len()
            || self.skin_files.iter().any(|(path, modified)| {
                std::fs::metadata(path).and_then(|m| m.modified()).ok() != *modified
            });
        if changed {
            log::info!("skin files changed; reloading");
            self.pending_skin = Some(self.skin.clone());
            // Even if the reload fails, the same edit should not be retried every second.
            self.watch_skin();
        }
    }

    // ----- Updates -----------------------------------------------------------------------------

    pub fn tick(&mut self) {
        self.player.rest_device_when_idle();
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
                    self.covers.request(&track.path);
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

        let tags: HashMap<PathBuf, freya_library::TagInfo> = self.tags.poll().collect();
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
        self.follow_updates();
        self.check_skin_files();
        self.refresh();
    }

    /// Looks for an update when it is time, and restarts into one that was installed.
    fn follow_updates(&mut self) {
        // Not in tests, and not before Freya has settled after starting.
        let wanted = !cfg!(test)
            && self.view.updates_out.available
            && self.settings.check_for_updates
            && self.started.elapsed() >= UPDATE_CHECK_DELAY
            && matches!(self.updater.state(), update::State::Idle)
            && update::due(self.settings.last_update_check, SystemTime::now());
        if wanted {
            self.settings.last_update_check = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs());
            let skipped = self.settings.skipped_update.clone();
            self.updater.check(false, skipped.as_deref());
        }
        if self.updater.poll()
            && matches!(self.updater.state(), update::State::Installed(_))
            && self.updater.restart()
        {
            log::info!("restarting into the update");
            let _ = slint::quit_event_loop();
        }
    }

    /// The file, and the part of it, that a track started plays, when its plays are counted:
    /// not for radio.
    fn countable<'a>(&self, track: &'a TrackInfo) -> Option<(&'a Path, Option<Part>)> {
        if track.station.is_some() || freya_audio::is_url(&track.path.to_string_lossy()) {
            return None;
        }
        // The playlist knows whether a track starting at 0 is a whole file or a CUE sheet's
        // first track.
        let part = match self.playlist.current_entry() {
            Some(entry) if entry.plays(&track.path, track.start) => entry.part,
            _ => (!track.start.is_zero() || track.end.is_some()).then_some(Part {
                start: track.start,
                end: track.end,
            }),
        };
        Some((&track.path, part))
    }

    /// A track started: Last.fm shows it as playing, if it may be scrobbled at all (tagged,
    /// not radio).
    fn lastfm_started(&mut self, track: &TrackInfo) {
        self.lastfm_play = track
            .station
            .is_none()
            .then(|| {
                lastfm::track(
                    track.artist.as_deref(),
                    track.title.as_deref(),
                    track.album.as_deref(),
                    track.duration,
                )
            })
            .flatten()
            .map(|t| LastFmPlay {
                track: t,
                started: lastfm::now(),
                scrobble: lastfm::long_enough(track.duration),
            });
        self.lastfm_now_playing();
        self.lastfm_was_playing = true;
    }

    fn lastfm_now_playing(&self) {
        if self.settings.lastfm_scrobbling
            && let Some(play) = &self.lastfm_play
        {
            self.lastfm.now_playing(play.track.clone());
        }
    }

    /// The track that plays has been heard enough to count: Last.fm gets the play.
    fn lastfm_heard(&self) {
        if self.settings.lastfm_scrobbling
            && let Some(play) = self.lastfm_play.as_ref().filter(|p| p.scrobble)
        {
            self.lastfm.scrobble(lastfm::Scrobble {
                track: play.track.clone(),
                started: play.started,
            });
        }
    }

    /// A request from the system's media controls: media keys, Now Playing.
    pub fn handle_media_request(&mut self, request: MediaRequest) {
        match request {
            MediaRequest::Play => self.play(),
            MediaRequest::Pause => self.player.set_paused(true),
            MediaRequest::Toggle => self.toggle_pause(),
            MediaRequest::Next => self.step(true),
            MediaRequest::Previous => self.step(false),
            MediaRequest::Stop => self.stop(),
            MediaRequest::SeekBy(seconds) => self.seek_by(seconds),
            MediaRequest::SetPosition(position) => self.player.seek(position),
            MediaRequest::SetVolume(volume) => self.player.set_volume(volume as f32),
            // Added to Up Next and played, as for an address: what is queued stays.
            MediaRequest::OpenUri(uri) => match playlist_file::file_url_path(&uri) {
                Some(path) => {
                    let first = self.playlist.len();
                    self.add_paths(&[path]);
                    if self.playlist.len() > first {
                        self.play_index(first);
                    }
                }
                None => self.add_url(&uri),
            },
            MediaRequest::Raise => {
                let _ = self.view.ui.show();
            }
            MediaRequest::Quit => {
                let _ = slint::quit_event_loop();
            }
        }
    }

    /// A stream added by its address gets the station's name in the playlist.
    fn name_station(&mut self, track: &TrackInfo) {
        let Some(station) = &track.station else {
            return;
        };
        let untitled = self
            .playlist
            .entries()
            .iter()
            .any(|e| e.path == track.path && e.title.is_none());
        if untitled {
            let tags = freya_library::TagInfo {
                title: Some(station.clone()),
                ..Default::default()
            };
            self.playlist.apply_tags(&track.path, &tags);
            self.playlist_dirty = true;
        }
    }

    /// Writes current state into the skin.
    pub fn refresh(&mut self) {
        let station_rows = self.stations.rows(self.playing_station().as_deref());
        let view = &mut self.view;
        let ui = &view.ui;
        let status = self.player.status();
        view.visualizer.update(
            ui,
            &self.player,
            status.state == PlaybackState::Playing && !self.window_hidden,
            &self.settings.visualizer_mode,
            !self.window_hidden,
        );
        view.visualizer.show_fps(ui, self.settings.visualizer_fps);
        let mut values = skin_state::player_values(
            &status,
            self.track.as_ref(),
            &self.playlist,
            self.player.volume(),
        );
        values.push(("error", skin_state::string(&self.error)));
        view.player_out.write(ui, values);
        if let Some(media) = &mut self.system_media {
            let track = status.track.as_deref().or(self.track.as_deref());
            media.update(
                &status,
                track,
                self.cover_file.as_deref(),
                self.player.volume(),
            );
        }
        if view.playlist_out.available {
            if self.playlist_dirty {
                view.rows.sync(&self.playlist);
                self.playlist_dirty = false;
            }
            let values = skin_state::playlist_values(
                &self.playlist,
                self.settings.shuffle,
                self.settings.repeat,
            );
            view.playlist_out.write(ui, values);
        }
        let eq = &self.settings.equalizer;
        if view.eq_out.available {
            if view.eq_bands_shown != Some(eq.bands_db) {
                let values: Vec<Value> = eq
                    .bands_db
                    .iter()
                    .map(|&db| Value::Number(f64::from(db)))
                    .collect();
                let _ = ui.set_global_property(
                    "Equalizer",
                    "bands",
                    Value::Model(ModelRc::new(VecModel::from(values))),
                );
                view.eq_bands_shown = Some(eq.bands_db);
            }
            view.eq_out.write(
                ui,
                vec![
                    ("enabled", Value::Bool(eq.enabled)),
                    ("preamp", Value::Number(f64::from(eq.preamp_db))),
                    ("preset-name", skin_state::string(eq.preset_name())),
                ],
            );
        }
        if view.library_out.available {
            let playing = status
                .track
                .as_ref()
                .filter(|_| status.state != PlaybackState::Stopped)
                .map(|track| (track.path.as_path(), track.start));
            self.library.set_playing(playing);
            self.library.show_folders(
                ui,
                &self.settings.library_folders,
                self.folders_draft.as_deref(),
            );
            view.library_out.write(ui, self.library.values());
        }
        if view.playlists_out.available {
            let shown = self.shown_playlist;
            let synced = Some((self.playlists.version(), shown));
            if view.playlists_synced != synced {
                view.playlist_list_rows.sync(self.playlists.rows());
                let tracks = shown
                    .and_then(|i| self.playlists.get(i))
                    .map(|p| {
                        p.entries
                            .iter()
                            .enumerate()
                            .map(|(i, e)| skin_state::track_row(i, e, false))
                            .collect()
                    })
                    .unwrap_or_default();
                view.playlist_track_rows.sync(tracks);
                view.playlists_synced = synced;
            }
            let summary = shown.map(|i| self.playlists.summary(i)).unwrap_or_default();
            view.playlists_out.write(
                ui,
                vec![
                    ("shown", Value::Number(shown.map_or(-1.0, |i| i as f64))),
                    ("summary", skin_state::string(summary)),
                    ("error", skin_state::string(&self.playlists_error)),
                ],
            );
        }
        if view.radio_out.available {
            view.station_rows.sync(station_rows);
            view.radio_out
                .write(ui, vec![("error", skin_state::string(&self.radio_error))]);
        }
        if !view.icon_set {
            view.icon_set = app_icon::set_window_icon(ui.window());
        }
        let buttons_width = view.title_bar.update(ui);
        let lastfm = self.lastfm.status();
        match lastfm.state {
            lastfm::State::Connected => self.settings.lastfm_user = Some(lastfm.user.clone()),
            lastfm::State::Disconnected => self.settings.lastfm_user = None,
            // Unreadable keeps the user: the session may be readable next time.
            _ => {}
        }
        view.lastfm_out.write(
            ui,
            vec![
                ("state", skin_state::string(lastfm.state.name())),
                ("user", skin_state::string(&lastfm.user)),
                ("scrobbling", Value::Bool(self.settings.lastfm_scrobbling)),
                ("waiting", Value::Number(lastfm.waiting as f64)),
                ("error", skin_state::string(&lastfm.error)),
            ],
        );
        if view.updates_out.available {
            let mut values = self.updater.values();
            values.push((
                "check-automatically",
                Value::Bool(self.settings.check_for_updates),
            ));
            view.updates_out.write(ui, values);
        }
        let activities = self.activities();
        let view = &mut self.view;
        let ui = &view.ui;
        view.activity_rows.sync(activities);
        view.app_out.write(
            ui,
            vec![
                (
                    "window-buttons-width",
                    Value::Number(f64::from(buttons_width)),
                ),
                ("theme", skin_state::string(&self.settings.theme)),
                (
                    "equalizer-visible",
                    Value::Bool(self.settings.equalizer_visible),
                ),
                ("can-go-back", Value::Bool(self.history.can_go_back())),
                ("can-go-forward", Value::Bool(self.history.can_go_forward())),
            ],
        );
    }

    /// What Freya does in the background, for `App.activities`.
    fn activities(&self) -> Vec<Value> {
        let mut activities = self.library.activities();
        let lastfm = self.lastfm.status();
        if lastfm.waiting > 0 {
            let detail = if lastfm.error.is_empty() {
                format!("{} waiting", lastfm.waiting)
            } else {
                lastfm.error.clone()
            };
            activities.push(("Sending plays to Last.fm".to_owned(), detail));
        }
        if let update::State::Downloading { release, .. } = self.updater.state() {
            activities.push(("Downloading an update".to_owned(), release.tag_name.clone()));
        }
        activities
            .into_iter()
            .map(|(title, detail)| {
                Value::Struct(slint_interpreter::Struct::from_iter([
                    ("title".to_owned(), skin_state::string(title)),
                    ("detail".to_owned(), skin_state::string(detail)),
                ]))
            })
            .collect()
    }

    /// Whether the app has things to follow closely: the library is being read.
    pub fn is_busy(&self) -> bool {
        self.library.is_scanning()
    }

    /// How often to draw visualization frames, or `None` when there is nothing to draw:
    /// nothing plays, or the window cannot be seen.
    pub fn visualization_interval(&self) -> Option<Duration> {
        let fps = visualizer::frame_rate(
            self.settings.visualizer_fps,
            self.view.visualizer.active(),
            self.window_hidden,
            self.window_focused,
        )?;
        Some(visualizer::frame_interval(fps))
    }

    /// Draws one visualization frame.
    pub fn visualize(&mut self, dt: Duration) {
        let playing = self.player.status().state == PlaybackState::Playing;
        self.view.visualizer.frame(
            &self.view.ui,
            &self.player,
            playing,
            &self.settings.visualizer_mode,
            dt,
        );
    }

    pub fn show_about(&mut self) {
        if self.about.is_none() {
            match freya_skin::about_window() {
                Ok(window) => {
                    let _ = window
                        .set_property("version", skin_state::string(env!("CARGO_PKG_VERSION")));
                    self.about = Some(window);
                }
                Err(e) => {
                    log::warn!("could not open About: {e}");
                    return;
                }
            }
        }
        if let Some(about) = &self.about {
            let _ = about.show();
        }
    }

    pub fn save(&mut self) {
        self.lastfm.finish();
        self.settings.volume = self.player.volume();
        let window = self.view.ui.window();
        // In full screen the window has the screen's size, not its own.
        if !window.is_fullscreen() {
            let size = window.size().to_logical(window.scale_factor());
            // Restored as the size under the system's title bar.
            let height = size.height - self.view.title_bar.added_height();
            self.settings.window_size = Some((size.width, height));
        }
        self.settings.skin = self
            .skin
            .path()
            .map(Path::to_owned)
            .or_else(|| self.unloaded_skin.clone());
        self.settings.save();
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

fn show_cover(ui: &SkinWindow, image: Option<slint::Image>) {
    let has_cover = image.is_some();
    let _ = ui.set_global_property("Player", "cover", Value::Image(image.unwrap_or_default()));
    let _ = ui.set_global_property("Player", "has-cover", Value::Bool(has_cover));
}

/// What the player plays for a playlist entry: its file, or for a track of a CUE sheet, the
/// part of the file the track is, with the track's own tags.
fn media(entry: &Entry) -> Media {
    let Some(part) = entry.part else {
        return Media {
            id: entry.id,
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
    }
}
