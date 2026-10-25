//! The library as a service: scans when the user asks, when the music's folders change and
//! when it is time to; asks the internet what artists lack once a scan is done; and tells its
//! clients how that goes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use freya_library::Library;
use freya_library::library::{LastFm, Online, ScanOptions, Scanner};
use freya_media::pictures::Pictures;
use freya_protocol::{
    ApiError, ApiResult, FolderListing, LastFmStatus, LastFmTrack, LibraryApi, LibraryData,
    LibraryPictures, Part, PlaylistEntry, RadioDirectoryStatus, RadioQuery, RadioResults,
    RadioShelf, RadioStation, RadioTag, SavedPlaylist, ScanEvent, Scrobble, ServerControl,
    ServerEvents, ServerSettings, ServiceEvent,
};

use crate::folder_watch::FolderWatch;
use crate::playlists::Playlists;
use crate::radio::Directory;
use crate::scrobbler::Scrobbler;
use crate::stations::Stations;

/// How often the service looks at the scan running, the folders' watch and the time.
const TICK: Duration = Duration::from_millis(100);
/// How often it looks whether the whole library is due to be scanned, and whether folders
/// not there before are there now.
const SCAN_CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// How the service keeps the library up to date.
#[derive(Clone, Default)]
pub struct ServiceConfig {
    /// As the user set it.
    pub settings: ServerSettings,
    /// Where `settings` are kept when the user changes them; nowhere when none.
    pub settings_file: Option<PathBuf>,
    /// Whether scans may bring the library in line with the folders: not when the settings
    /// were damaged, and the list may be missing some, which would take their tracks away.
    pub scans_allowed: bool,
    /// Last.fm, for what it says of artists, when this build may ask it.
    pub lastfm: Option<Arc<dyn LastFm>>,
    /// Where the user's playlists are kept; without, there are none.
    pub playlists: Option<PathBuf>,
    /// The file the radio stations are kept in; without, there are none.
    pub stations: Option<PathBuf>,
    /// The file Radio Browser's stations are kept in, for the radio directory; without, it
    /// has only the stations Freya picked.
    pub radio_directory: Option<PathBuf>,
    /// Where scrobbling to Last.fm keeps the session and the plays waiting; without, the server
    /// does not scrobble.
    pub scrobbles: Option<PathBuf>,
}

impl ServiceConfig {
    /// What scans may ask the internet about artists.
    fn online(&self) -> Online {
        let settings = &self.settings;
        Online {
            deezer: settings.artist_pictures_online,
            fanart_key: settings.fanart_key.clone(),
            lastfm: self.lastfm.clone().filter(|_| settings.artist_bios_online),
            wikipedia: settings.artist_wikipedia_online,
        }
    }

    /// How often the whole library is scanned, if at all.
    fn scan_every(&self) -> Option<Duration> {
        let hours = self.settings.scan_every_hours;
        (hours > 0).then(|| Duration::from_secs(u64::from(hours) * 3600))
    }
}

/// The library and its scans, run on a thread of their own.
pub struct Service {
    shared: Arc<Shared>,
    ticker: Option<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<State>,
    playlists: Option<Playlists>,
    stations: Option<Stations>,
    radio: Directory,
    scrobbler: Option<Mutex<Scrobbler>>,
    stop: AtomicBool,
    listeners: Arc<Mutex<Listeners>>,
    /// What clients ask of the library is read and written here, not waiting for a scan.
    api: Arc<LocalLibrary>,
}

impl Service {
    /// Opens the library at `database`, with its pictures kept in `pictures`, and starts
    /// keeping it up to date as `config` says: the first time, everything is read; after,
    /// what changed while it was not running is found by a periodic scan, or by one at each
    /// start if the user wants it.
    pub fn start(
        database: PathBuf,
        pictures: Option<PathBuf>,
        config: ServiceConfig,
    ) -> Result<Self, String> {
        let open = || {
            Library::open(&database)
                .map_err(|e| format!("could not open the library at {}: {e}", database.display()))
        };
        let db = open()?;
        let readers = (0..READERS).map(|_| open()).collect::<Result<_, _>>()?;
        let watch = open()?;
        let pictures = pictures.map(Pictures::new);
        let state = State {
            database: database.clone(),
            db,
            pictures: pictures.clone(),
            scanner: Scanner::new(),
            scanning: false,
            config,
            watch: None,
            watch_tried: None,
            scan_later: Vec::new(),
            fetch_later: Vec::new(),
            fetch_all_later: false,
            scan_checked: None,
            plays: Vec::new(),
            outbox: Vec::new(),
        };
        let playlists = state.config.playlists.clone().map(Playlists::new);
        let stations = state.config.stations.clone().map(Stations::new);
        let listeners: Arc<Mutex<Listeners>> = Arc::default();
        let radio = {
            let listeners = Arc::clone(&listeners);
            Directory::start(
                state.config.radio_directory.clone(),
                state.config.settings.radio_directory_online,
                Arc::new(move || tell_all(&listeners, ServiceEvent::RadioDirectoryChanged)),
            )
        };
        let scrobbler = state.config.scrobbles.clone().map(|dir| {
            let listeners = Arc::clone(&listeners);
            let tell: crate::scrobbler::Tell = Arc::new(move |status| {
                tell_all(&listeners, ServiceEvent::LastFm(status));
            });
            Mutex::new(Scrobbler::start(dir, tell))
        });
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            playlists,
            stations,
            radio,
            scrobbler,
            stop: AtomicBool::new(false),
            listeners,
            api: Arc::new(LocalLibrary::with_connections(readers, pictures).watched_by(watch)),
        });
        let mut service = Self {
            shared,
            ticker: None,
        };
        service.with(State::begin);
        let shared = Arc::clone(&service.shared);
        service.ticker = Some(
            std::thread::Builder::new()
                .name("freya-service".into())
                .spawn(move || {
                    let mut generation = shared.api.generation();
                    while !shared.stop.load(Ordering::Relaxed) {
                        std::thread::sleep(TICK);
                        Self::run(&shared, State::tick);
                        // Whatever changed it, clients keeping answers are told.
                        let now = shared.api.generation();
                        if now != generation {
                            generation = now;
                            tell_all(&shared.listeners, ServiceEvent::Changed);
                        }
                    }
                })
                .map_err(|e| format!("could not start the library's thread: {e}"))?,
        );
        Ok(service)
    }

    /// The library, for a client to ask of.
    pub fn api(&self) -> Arc<dyn LibraryApi> {
        Arc::clone(&self.shared.api) as _
    }

    /// Runs `f` on the state, then tells the listeners what it said.
    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        Self::run(&self.shared, f)
    }

    fn run<R>(shared: &Shared, f: impl FnOnce(&mut State) -> R) -> R {
        let (result, events) = {
            let mut state = lock(&shared.state);
            let result = f(&mut state);
            (result, std::mem::take(&mut state.outbox))
        };
        for event in events {
            tell_all(&shared.listeners, event);
        }
        result
    }

    /// The scrobbler, when the server scrobbles.
    fn scrobbler(&self) -> ApiResult<MutexGuard<'_, Scrobbler>> {
        self.shared
            .scrobbler
            .as_ref()
            .map(lock)
            .ok_or_else(|| ApiError("this server does not scrobble".into()))
    }
}

impl ServerControl for Service {
    fn settings(&self) -> ApiResult<ServerSettings> {
        Ok(lock(&self.shared.state).config.settings.clone())
    }

    fn set_settings(&self, settings: &ServerSettings) -> ApiResult<()> {
        self.with(|state| state.set_settings(settings.clone()))?;
        self.shared
            .radio
            .set_online(settings.radio_directory_online);
        Ok(())
    }

    fn rescan(&self) -> ApiResult<()> {
        self.with(|state| state.scan(Vec::new(), false));
        Ok(())
    }

    fn rescan_files(&self) -> ApiResult<()> {
        self.with(|state| state.scan(Vec::new(), true));
        Ok(())
    }

    fn scan_only(&self, folders: &[PathBuf]) -> ApiResult<()> {
        self.with(|state| state.scan(folders.to_vec(), false));
        Ok(())
    }

    /// Stops the scan running, and what was to follow it; what it did so far is kept.
    fn stop_scan(&self) -> ApiResult<()> {
        self.with(|state| {
            state.fetch_later.clear();
            state.fetch_all_later = false;
            state.scan_later.clear();
            state.scanner.stop();
        });
        Ok(())
    }

    fn fetch(&self, artists: Option<&[String]>) -> ApiResult<()> {
        self.with(|state| state.fetch(artists.map(<[String]>::to_vec)));
        Ok(())
    }

    /// While a scan writes, the play waits for it.
    fn record_play(&self, path: &Path, part: Option<Part>, at: SystemTime) -> ApiResult<bool> {
        Ok(self.with(|state| {
            state.plays.push((path.to_owned(), part, at));
            !state.scanning && state.write_plays()
        }))
    }

    fn save(&self) -> ApiResult<()> {
        self.with(|state| {
            state.write_plays();
        });
        Ok(())
    }

    fn is_scanning(&self) -> ApiResult<bool> {
        Ok(lock(&self.shared.state).scanning)
    }

    fn playlists(&self) -> ApiResult<Vec<SavedPlaylist>> {
        Ok(self
            .shared
            .playlists
            .as_ref()
            .map(Playlists::all)
            .unwrap_or_default())
    }

    fn save_playlist(&self, name: &str, entries: &[PlaylistEntry]) -> ApiResult<()> {
        self.playlists_changed(|playlists| playlists.save(name, entries))
    }

    fn rename_saved_playlist(&self, from: &str, to: &str) -> ApiResult<()> {
        self.playlists_changed(|playlists| playlists.rename(from, to))?;
        // What the library keeps of it follows it.
        self.shared.api.with_db(|db| db.rename_playlist(from, to))
    }

    fn delete_playlist(&self, name: &str) -> ApiResult<()> {
        self.playlists_changed(|playlists| playlists.delete(name))?;
        self.shared.api.with_db(|db| db.forget_playlist(name))
    }

    fn stations(&self) -> ApiResult<Vec<RadioStation>> {
        Ok(self
            .shared
            .stations
            .as_ref()
            .map(Stations::all)
            .unwrap_or_default())
    }

    fn save_stations(&self, stations: &[RadioStation]) -> ApiResult<()> {
        self.shared
            .stations
            .as_ref()
            .ok_or_else(|| ApiError("the server keeps no radio stations".into()))?
            .save(stations)
            .map_err(ApiError)?;
        self.with(|state| state.outbox.push(ServiceEvent::StationsChanged));
        Ok(())
    }

    fn radio_directory(&self) -> ApiResult<RadioDirectoryStatus> {
        Ok(self.shared.radio.status())
    }

    fn radio_tags(&self) -> ApiResult<Vec<RadioTag>> {
        Ok(self.shared.radio.tags())
    }

    fn radio_search(&self, query: &RadioQuery) -> ApiResult<RadioResults> {
        Ok(self.shared.radio.search(query))
    }

    fn radio_shelves(&self) -> ApiResult<Vec<RadioShelf>> {
        Ok(self.shared.radio.shelves())
    }

    fn radio_played(&self, id: &str) -> ApiResult<()> {
        self.shared.radio.played(id);
        Ok(())
    }

    fn server_folders(&self, folder: Option<&Path>) -> ApiResult<FolderListing> {
        Ok(crate::folders::listing(folder))
    }

    fn lastfm_status(&self) -> ApiResult<LastFmStatus> {
        Ok(self
            .shared
            .scrobbler
            .as_ref()
            .map(|scrobbler| lock(scrobbler).status())
            .unwrap_or_default())
    }

    fn lastfm_connect(&self) -> ApiResult<String> {
        self.scrobbler()?.connect().map_err(ApiError)
    }

    fn lastfm_cancel(&self) -> ApiResult<()> {
        self.scrobbler()?.cancel();
        Ok(())
    }

    fn lastfm_disconnect(&self) -> ApiResult<()> {
        self.scrobbler()?.disconnect();
        Ok(())
    }

    fn lastfm_now_playing(&self, track: &LastFmTrack) -> ApiResult<()> {
        self.scrobbler()?.now_playing(track.clone());
        Ok(())
    }

    fn lastfm_scrobble(&self, scrobble: &Scrobble) -> ApiResult<()> {
        self.scrobbler()?.scrobble(scrobble.clone());
        Ok(())
    }
}

impl Service {
    /// Changes the playlists as `change` does, and tells the clients.
    fn playlists_changed(
        &self,
        change: impl FnOnce(&Playlists) -> Result<(), String>,
    ) -> ApiResult<()> {
        let playlists = self
            .shared
            .playlists
            .as_ref()
            .ok_or_else(|| ApiError("the server keeps no playlists".into()))?;
        change(playlists).map_err(ApiError)?;
        self.with(|state| state.outbox.push(ServiceEvent::PlaylistsChanged));
        Ok(())
    }
}

impl ServerEvents for Service {
    fn events(&self) -> ApiResult<Receiver<ServiceEvent>> {
        let (tell, events) = sync_channel(EVENTS_WAITING);
        // Told under the state's lock, so nothing it does is missed in between.
        let state = lock(&self.shared.state);
        let mut listeners = lock(&self.shared.listeners);
        let _ = tell.send(ServiceEvent::Connected {
            scanning: state.scanning,
            told: listeners.told,
            changes: true,
        });
        listeners.senders.push(tell);
        drop(state);
        Ok(events)
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        let mut state = lock(&self.shared.state);
        state.scanner.stop();
        state.write_plays();
        drop(state);
        // Plays to scrobble reach the queue on disk.
        if let Some(scrobbler) = &self.shared.scrobbler {
            lock(scrobbler).finish();
        }
    }
}

/// How many connections the server's clients are answered on, so one asking for the whole
/// library does not hold up another, or a track that is to play.
const READERS: usize = 4;

/// The library in this process, as a client asks of it: what it holds, read and written on
/// connections of its own, and its pictures.
pub struct LocalLibrary {
    connections: Vec<Mutex<Library>>,
    /// Where the next call looks for a free connection first.
    next: AtomicUsize,
    pictures: Option<Pictures>,
    /// A connection that only watches what is written to the database, by any other, and the
    /// calls that may have written on these: together the library's generation.
    watch: Option<Mutex<Library>>,
    writes: AtomicU64,
}

impl LocalLibrary {
    pub fn new(db: Library, pictures: Option<Pictures>) -> Self {
        Self::with_connections(vec![db], pictures)
    }

    /// A library answered on each of `connections`, at once: as many as there are. Each is
    /// a connection to the same database.
    pub fn with_connections(connections: Vec<Library>, pictures: Option<Pictures>) -> Self {
        assert!(!connections.is_empty(), "a library needs a connection");
        Self {
            connections: connections.into_iter().map(Mutex::new).collect(),
            next: AtomicUsize::new(0),
            pictures,
            watch: None,
            writes: AtomicU64::new(0),
        }
    }

    /// The library, its generation told by `watch`, a connection to its database that only
    /// watches it. Without one, it has none: it cannot see what others write.
    pub fn watched_by(mut self, watch: Library) -> Self {
        self.watch = Some(Mutex::new(watch));
        self
    }

    /// A connection no other call uses, or when all are, the next one when it is.
    fn connection(&self) -> MutexGuard<'_, Library> {
        let count = self.connections.len();
        let first = self.next.fetch_add(1, Ordering::Relaxed);
        for at in first..first + count {
            if let Ok(free) = self.connections[at % count].try_lock() {
                return free;
            }
        }
        lock(&self.connections[first % count])
    }

    /// Runs `f` on one of the library's connections.
    fn with_db<T>(
        &self,
        f: impl FnOnce(&mut Library) -> freya_library::library::Result<T>,
    ) -> ApiResult<T> {
        let done = f(&mut self.connection()).map_err(api_error);
        self.writes.fetch_add(1, Ordering::Relaxed);
        done
    }

    fn pictures(&self) -> ApiResult<&Pictures> {
        self.pictures
            .as_ref()
            .ok_or_else(|| ApiError("the library keeps no pictures".into()))
    }
}

/// An answer of the library as the API gives it: as it is, or a map as its pairs.
trait IntoApi<T> {
    fn into_api(self) -> T;
}

impl<T> IntoApi<T> for T {
    fn into_api(self) -> T {
        self
    }
}

impl<K, V> IntoApi<Vec<(K, V)>> for std::collections::HashMap<K, V> {
    fn into_api(self) -> Vec<(K, V)> {
        self.into_iter().collect()
    }
}

fn api_error(e: impl std::fmt::Display) -> ApiError {
    ApiError(e.to_string())
}

macro_rules! serve_library_data {
    ($(
        $(#[doc = $doc:literal])*
        fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty;
    )*) => {
        impl LibraryData for LocalLibrary {
            $(
                fn $name(&self, $($arg: $ty),*) -> ApiResult<$ret> {
                    let answer = self.connection()
                        .$name($($arg),*)
                        .map(IntoApi::into_api)
                        .map_err(api_error);
                    if !freya_protocol::CACHED_CALLS.contains(&stringify!($name)) {
                        self.writes.fetch_add(1, Ordering::Relaxed);
                    }
                    answer
                }
            )*
        }
    };
}

freya_protocol::with_library_data!(serve_library_data {});

impl LibraryApi for LocalLibrary {
    fn generation(&self) -> Option<u64> {
        let watch = lock(self.watch.as_ref()?);
        let written = watch.data_version().ok()?;
        Some(written.wrapping_add(self.writes.load(Ordering::Relaxed)))
    }
}

impl LibraryPictures for LocalLibrary {
    fn picture(&self, checksum: &str) -> ApiResult<Option<Vec<u8>>> {
        if !freya_protocol::is_checksum(checksum) {
            return Ok(None);
        }
        Ok(self.pictures()?.read(checksum))
    }

    fn put_picture(&self, bytes: &[u8]) -> ApiResult<String> {
        self.pictures()?.put(bytes).map_err(api_error)
    }

    fn small_picture(&self, checksum: &str) -> ApiResult<Option<Vec<u8>>> {
        if !freya_protocol::is_checksum(checksum) {
            return Ok(None);
        }
        Ok(self
            .pictures()?
            .small(checksum, freya_protocol::SMALL_PICTURE))
    }

    fn take_picture(&self, bytes: &[u8], largest: u32) -> ApiResult<String> {
        self.pictures()?
            .take_bytes(bytes, largest)
            .map_err(api_error)
    }

    fn album_art(&self, track: &Path, largest: u32) -> ApiResult<Option<Vec<u8>>> {
        // Only the library's own music is read.
        if !self.connection().knows_file(track).map_err(api_error)? {
            return Ok(None);
        }
        let Some(picture) = freya_media::cover::album_art_large(track, largest) else {
            return Ok(None);
        };
        // Shown whole and large: a high quality.
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 92)
            .encode_image(&picture.to_rgb8())
            .map_err(api_error)?;
        Ok(Some(jpeg))
    }
}

/// How many events wait for a listener before it is let go: one that stopped reading them
/// does not keep them all.
const EVENTS_WAITING: usize = 1024;

/// Tells every listener of `event`; those gone, or that let too many wait, are forgotten.
fn tell_all(listeners: &Mutex<Listeners>, event: ServiceEvent) {
    let mut listeners = lock(listeners);
    listeners.told += 1;
    listeners
        .senders
        .retain(|listener| listener.try_send(event.clone()).is_ok());
}

/// Who is told what happens, and how many events were told so far.
#[derive(Default)]
struct Listeners {
    senders: Vec<SyncSender<ServiceEvent>>,
    told: u64,
}

/// A lock, taken even when a thread panicked holding it: what it guards stays usable.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct State {
    database: PathBuf,
    /// For what the service writes itself: plays, and when the library was scanned.
    db: Library,
    /// Where albums' covers and artists' pictures are kept; without, scans make none.
    pictures: Option<Pictures>,
    scanner: Scanner,
    /// A scan was started and its end not yet told.
    scanning: bool,
    config: ServiceConfig,
    /// The folders' watch, while they are watched, and when it was last tried: a drive not
    /// mounted yet is tried again.
    watch: Option<FolderWatch>,
    watch_tried: Option<Instant>,
    /// The folders where something changed, to scan once the scan running ends; and the
    /// artists to ask the internet about then, all of them when `fetch_all_later`.
    scan_later: Vec<PathBuf>,
    fetch_later: Vec<String>,
    fetch_all_later: bool,
    /// When it was last looked into whether the whole library is due to be scanned.
    scan_checked: Option<Instant>,
    /// Plays not yet written: a scan may be writing to the database.
    plays: Vec<(PathBuf, Option<Part>, SystemTime)>,
    /// What to tell the listeners.
    outbox: Vec<ServiceEvent>,
}

impl State {
    fn begin(&mut self) {
        self.start_watching();
        if !self.config.scans_allowed {
            log::warn!("the library is not checked against its folders: the settings were damaged");
            return;
        }
        let empty = self.db.last_full_scan().ok().flatten().is_none();
        if empty || self.config.settings.scan_at_start {
            self.scan(Vec::new(), false);
        } else if !self.db.cover_colors_current().unwrap_or(true)
            && let Some(pictures) = &self.pictures
        {
            // Albums' colors as they are made now, from their covers: quick.
            self.scanner
                .make_colors(self.database.clone(), pictures.clone());
            self.started();
        }
    }

    fn tick(&mut self) {
        self.watch_when_there();
        if let Some(changed) = self.watch.as_mut().and_then(FolderWatch::poll) {
            self.scan_changed(changed);
        }
        self.scan_when_due();
        // Asked before the events are read: a scan that had ended by then has sent them all.
        let ended = !self.scanner.is_running();
        let mut finished = false;
        let events: Vec<ScanEvent> = self.scanner.poll().collect();
        for event in events {
            match &event {
                ScanEvent::Finished(_) => finished = true,
                ScanEvent::Failed(e) => {
                    log::warn!("library scan failed: {e}");
                    finished = true;
                }
                _ => {}
            }
            self.outbox.push(ServiceEvent::Scan(event));
        }
        // A scan whose thread ended without saying so is over all the same.
        if self.scanning && (finished || ended) {
            self.ended();
        }
    }

    fn started(&mut self) {
        if !self.scanning {
            self.scanning = true;
            self.outbox.push(ServiceEvent::ScanStarted);
        }
    }

    /// The scan ended: the plays noted while it wrote are written, pictures no artist has are
    /// taken away, and what was to follow it starts.
    fn ended(&mut self) {
        self.scanning = false;
        if self.write_plays() {
            self.outbox.push(ServiceEvent::PlaysWritten);
        }
        self.keep_named_pictures();
        self.outbox.push(ServiceEvent::ScanEnded);
        // A scan asks the internet for what every artist lacks too.
        if !self.scan_later.is_empty() {
            self.fetch_later.clear();
            self.fetch_all_later = false;
            let changed = std::mem::take(&mut self.scan_later);
            self.scan_changed(changed);
        } else if std::mem::take(&mut self.fetch_all_later) {
            self.fetch_later.clear();
            self.fetch(None);
        } else if !self.fetch_later.is_empty() {
            let artists = std::mem::take(&mut self.fetch_later);
            self.fetch(Some(artists));
        }
    }

    /// Works by `settings` from now on, and keeps them.
    fn set_settings(&mut self, settings: ServerSettings) -> ApiResult<()> {
        let before = std::mem::replace(&mut self.config.settings, settings);
        let now = &self.config.settings;
        if before.library_folders != now.library_folders
            || before.watch_folders != now.watch_folders
        {
            self.watch = None;
        }
        if before.scan_every_hours != now.scan_every_hours {
            self.scan_checked = None;
        }
        // A service newly allowed is asked now what artists lack.
        let allowed = (now.artist_pictures_online && !before.artist_pictures_online)
            || (now.artist_bios_online && !before.artist_bios_online)
            || (now.artist_wikipedia_online && !before.artist_wikipedia_online)
            || (now.fanart_key.is_some() && now.fanart_key != before.fanart_key);
        self.start_watching();
        if allowed {
            self.fetch(None);
        }
        self.save_settings()
    }

    fn save_settings(&self) -> ApiResult<()> {
        let Some(file) = &self.config.settings_file else {
            return Ok(());
        };
        crate::settings::save(file, &self.config.settings).map_err(|e| {
            ApiError(format!(
                "could not keep the settings in {}: {e}",
                file.display()
            ))
        })
    }

    fn start_watching(&mut self) {
        let settings = &self.config.settings;
        if self.watch.is_none()
            && settings.watch_folders
            && self.config.scans_allowed
            && !settings.library_folders.is_empty()
        {
            self.watch_tried = Some(Instant::now());
            self.watch = FolderWatch::new(&settings.library_folders);
        }
    }

    /// Watches the folders once they are there, when they were not: a drive mounted later.
    fn watch_when_there(&mut self) {
        let folders = &self.config.settings.library_folders;
        let all = self
            .watch
            .as_ref()
            .is_some_and(|watch| watch.watching() == folders.len());
        let tried_lately = self
            .watch_tried
            .is_some_and(|at| at.elapsed() < SCAN_CHECK_INTERVAL);
        if all || tried_lately || !self.config.settings.watch_folders {
            return;
        }
        // Looked at once a minute, not at each tick.
        self.watch_tried = Some(Instant::now());
        if !folders.iter().any(|f| f.is_dir()) {
            return;
        }
        // Some of them were not there: tried again from scratch.
        let missing = self
            .watch
            .as_ref()
            .map_or(folders.len(), |watch| folders.len() - watch.watching());
        if missing > 0 {
            self.watch = None;
            self.start_watching();
        }
    }

    /// Scans `changed`, folders within the library's, once no scan runs.
    fn scan_changed(&mut self, changed: Vec<PathBuf>) {
        let changed: Vec<PathBuf> = changed
            .into_iter()
            .filter(|c| {
                let folders = &self.config.settings.library_folders;
                folders.iter().any(|f| c.starts_with(f))
            })
            .collect();
        if changed.is_empty() {
            return;
        }
        if self.scanning {
            self.scan_later.extend(changed);
            return;
        }
        log::info!("scanning what changed in {} folders", changed.len());
        self.scan(changed, false);
    }

    /// Scans the whole library when the last scan of it is older than the user wants.
    fn scan_when_due(&mut self) {
        let Some(every) = self.config.scan_every() else {
            return;
        };
        if self.scanning
            || !self.config.scans_allowed
            || self
                .scan_checked
                .is_some_and(|at| at.elapsed() < SCAN_CHECK_INTERVAL)
        {
            return;
        }
        self.scan_checked = Some(Instant::now());
        let due = match self.db.last_full_scan() {
            Ok(Some(last)) => last.elapsed().is_ok_and(|age| age >= every),
            Ok(None) => true,
            Err(e) => {
                log::warn!("could not read when the library was last scanned: {e}");
                false
            }
        };
        if due {
            log::info!("scanning the whole library, as it is time to");
            self.scan(Vec::new(), false);
        }
    }

    /// Looks through the library's folders, or those of them in `only`; with `rescan`,
    /// reads every file again.
    fn scan(&mut self, only: Vec<PathBuf>, rescan: bool) {
        if !self.config.scans_allowed {
            return;
        }
        let options = ScanOptions {
            pictures: self.pictures.clone(),
            rescan,
            only,
            online: self.config.online(),
        };
        let folders = self.config.settings.library_folders.clone();
        self.scanner.start(self.database.clone(), folders, options);
        self.started();
    }

    fn fetch(&mut self, artists: Option<Vec<String>>) {
        if self.scanning {
            match artists {
                Some(artists) => self.fetch_later.extend(artists),
                None => self.fetch_all_later = true,
            }
            return;
        }
        let Some(pictures) = &self.pictures else {
            return;
        };
        let online = self.config.online();
        if !online.any() {
            return;
        }
        self.scanner
            .fetch(self.database.clone(), artists, online, pictures.clone());
        self.started();
    }

    /// Writes the plays noted. Returns true when any were written.
    fn write_plays(&mut self) -> bool {
        let mut written = false;
        while let Some((path, part, at)) = self.plays.first() {
            if let Err(e) = self.db.record_play(path, *part, *at) {
                log::warn!("could not note a play of {}: {e}", path.display());
                break;
            }
            self.plays.remove(0);
            written = true;
        }
        written
    }

    /// Takes away the downloaded pictures no artist has any longer.
    fn keep_named_pictures(&self) {
        let Some(pictures) = &self.pictures else {
            return;
        };
        let kept = self
            .db
            .artist_picture_checksums()
            .map_err(|e| e.to_string())
            .and_then(|named| pictures.keep_only(&named).map_err(|e| e.to_string()));
        match kept {
            Ok(0) => {}
            Ok(n) => log::info!("took away {n} downloaded pictures no artist has"),
            Err(e) => log::warn!("could not tidy the downloaded pictures: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("freya-service-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("music")).unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../freya-audio/tests/fixtures/sine.flac");
        std::fs::copy(fixture, dir.join("music/sine.flac")).unwrap();
        dir
    }

    /// The events until the scan running ends.
    fn until_ended(events: &Receiver<ServiceEvent>) -> Vec<ServiceEvent> {
        let mut seen = Vec::new();
        while let Ok(event) = events.recv_timeout(Duration::from_secs(20)) {
            let ended = event == ServiceEvent::ScanEnded;
            seen.push(event);
            if ended {
                return seen;
            }
        }
        panic!("the scan did not end: {seen:?}");
    }

    #[test]
    fn a_first_start_reads_the_music_and_plays_wait_for_the_scan() {
        let dir = temp_dir("first");
        let config = ServiceConfig {
            settings: ServerSettings {
                library_folders: vec![dir.join("music")],
                watch_folders: false,
                ..ServerSettings::default()
            },
            scans_allowed: true,
            ..ServiceConfig::default()
        };
        let service = Service::start(dir.join("library.db"), None, config).unwrap();
        let events = service.events().unwrap();
        assert!(
            matches!(
                events.recv().unwrap(),
                ServiceEvent::Connected { scanning: true, .. }
            ),
            "an empty library is read at once"
        );
        // Noted while the scan writes: it waits for it.
        // As the scan names it, on Windows too.
        let track = dir.join("music").join("sine.flac");
        let written = service
            .record_play(&track, None, SystemTime::now())
            .unwrap();
        let seen = until_ended(&events);
        assert!(
            seen.iter()
                .any(|e| matches!(e, ServiceEvent::Scan(ScanEvent::Finished(s)) if s.added == 1))
        );
        assert!(!written, "it waited");
        assert!(
            seen.contains(&ServiceEvent::PlaysWritten),
            "and was written after"
        );
        assert!(!service.is_scanning().unwrap());
        drop(service);

        let library = Library::open(&dir.join("library.db")).unwrap();
        assert_eq!(library.all_tracks().unwrap().len(), 1);
        assert_eq!(library.most_played(10).unwrap()[0].plays, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn damaged_settings_scan_nothing() {
        let dir = temp_dir("damaged");
        let config = ServiceConfig {
            settings: ServerSettings {
                library_folders: vec![dir.join("music")],
                ..ServerSettings::default()
            },
            ..ServiceConfig::default()
        };
        let service = Service::start(dir.join("library.db"), None, config).unwrap();
        service.rescan().unwrap();
        assert!(!service.is_scanning().unwrap());
        drop(service);
        let library = Library::open(&dir.join("library.db")).unwrap();
        assert!(library.all_tracks().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
