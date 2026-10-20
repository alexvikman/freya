//! The library as a service: scans when the user asks, when the music's folders change and
//! when it is time to; asks the internet what artists lack once a scan is done; and tells its
//! clients how that goes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use freya_library::Library;
use freya_library::library::{Online, ScanOptions, Scanner};
use freya_library::pictures::Pictures;
use freya_protocol::{
    ApiError, ApiResult, LibraryApi, LibraryData, LibraryPictures, Part, ScanEvent, ServiceEvent,
};

use crate::folder_watch::FolderWatch;

/// How often the service looks at the scan running, the folders' watch and the time.
const TICK: Duration = Duration::from_millis(100);
/// How often it looks whether the whole library is due to be scanned, and whether folders
/// not there before are there now.
const SCAN_CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// How the library is kept up to date, as the user set it.
#[derive(Clone, Debug)]
pub struct ServiceConfig {
    /// The music's folders.
    pub folders: Vec<PathBuf>,
    /// Whether scans may bring the library in line with `folders`: not when the settings were
    /// damaged, and the list may be missing some, which would take their tracks away.
    pub scans_allowed: bool,
    /// Whether the folders are watched, and what changes in them scanned at once.
    pub watch_folders: bool,
    /// How often the whole library is scanned, if at all; and whether also each time the
    /// service starts.
    pub scan_every: Option<Duration>,
    pub scan_at_start: bool,
    /// What scans may ask the internet about artists.
    pub online: Online,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            scans_allowed: true,
            watch_folders: true,
            scan_every: Some(Duration::from_secs(24 * 3600)),
            scan_at_start: false,
            online: Online::default(),
        }
    }
}

/// The library and its scans, run on a thread of their own.
pub struct Service {
    shared: Arc<Shared>,
    ticker: Option<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<State>,
    stop: AtomicBool,
    listeners: Mutex<Vec<Sender<ServiceEvent>>>,
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
        let (db, reader) = (open()?, open()?);
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
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            stop: AtomicBool::new(false),
            listeners: Mutex::new(Vec::new()),
            api: Arc::new(LocalLibrary::new(reader, pictures)),
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
                    while !shared.stop.load(Ordering::Relaxed) {
                        std::thread::sleep(TICK);
                        Self::run(&shared, State::tick);
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

    /// What happens from now on, as it happens.
    pub fn subscribe(&self) -> Receiver<ServiceEvent> {
        let (tell, events) = channel();
        lock(&self.shared.listeners).push(tell);
        events
    }

    /// Whether a scan, or asking the internet, runs.
    pub fn is_scanning(&self) -> bool {
        lock(&self.shared.state).scanning
    }

    /// The music's folders now: those watched, and scanned.
    pub fn set_folders(&self, folders: &[PathBuf]) {
        self.with(|state| state.set_folders(folders));
    }

    pub fn set_watch_folders(&self, watch: bool) {
        self.with(|state| {
            state.config.watch_folders = watch;
            if !watch {
                state.watch = None;
            }
            state.start_watching();
        });
    }

    pub fn set_scan_every(&self, every: Option<Duration>) {
        self.with(|state| {
            state.config.scan_every = every;
            state.scan_checked = None;
        });
    }

    pub fn set_scan_at_start(&self, scan: bool) {
        self.with(|state| state.config.scan_at_start = scan);
    }

    pub fn set_online(&self, online: Online) {
        self.with(|state| state.config.online = online);
    }

    /// Brings the library in line with the folders: reads what changed, and asks the internet
    /// for what artists lack.
    pub fn rescan(&self) {
        self.with(|state| state.scan(Vec::new(), false));
    }

    /// Reads every file again and makes every cover again, changed or not.
    pub fn rescan_files(&self) {
        self.with(|state| state.scan(Vec::new(), true));
    }

    /// Looks through `folders`, within the library's, only.
    pub fn scan_only(&self, folders: Vec<PathBuf>) {
        self.with(|state| state.scan(folders, false));
    }

    /// Stops the scan running, and what was to follow it; what it did so far is kept.
    pub fn stop_scan(&self) {
        self.with(|state| {
            state.fetch_later.clear();
            state.fetch_all_later = false;
            state.scan_later.clear();
            state.scanner.stop();
        });
    }

    /// Asks the internet what `artists` lack, every artist when none, once no scan runs:
    /// after the user matched them to another artist or allowed another service.
    pub fn fetch(&self, artists: Option<Vec<String>>) {
        self.with(|state| state.fetch(artists));
    }

    /// Notes that the track at `path` was played at `at`. Returns whether it is in the
    /// library now: while a scan writes, the play waits for it.
    pub fn record_play(&self, path: &Path, part: Option<Part>, at: SystemTime) -> bool {
        self.with(|state| {
            state.plays.push((path.to_owned(), part, at));
            !state.scanning && state.write_plays()
        })
    }

    /// Writes the plays noted, whether a scan runs or not: for when Freya quits.
    pub fn save(&self) {
        self.with(|state| {
            state.write_plays();
        });
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
        if !events.is_empty() {
            let mut listeners = lock(&shared.listeners);
            for event in events {
                listeners.retain(|listener| listener.send(event.clone()).is_ok());
            }
        }
        result
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
    }
}

/// The library in this process, as a client asks of it: what it holds, read and written on a
/// connection of its own, and its pictures.
pub struct LocalLibrary {
    db: Mutex<Library>,
    pictures: Option<Pictures>,
}

impl LocalLibrary {
    pub fn new(db: Library, pictures: Option<Pictures>) -> Self {
        Self {
            db: Mutex::new(db),
            pictures,
        }
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
                    lock(&self.db)
                        .$name($($arg),*)
                        .map(IntoApi::into_api)
                        .map_err(api_error)
                }
            )*
        }
    };
}

freya_protocol::with_library_data!(serve_library_data);

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

    fn take_picture(&self, bytes: &[u8], largest: u32) -> ApiResult<String> {
        self.pictures()?
            .take_bytes(bytes, largest)
            .map_err(api_error)
    }

    fn album_art(&self, track: &Path, largest: u32) -> ApiResult<Option<Vec<u8>>> {
        // Only the library's own music is read.
        if !lock(&self.db).knows_file(track).map_err(api_error)? {
            return Ok(None);
        }
        let Some(picture) = freya_library::cover::album_art_large(track, largest) else {
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
        if empty || self.config.scan_at_start {
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

    fn set_folders(&mut self, folders: &[PathBuf]) {
        if self.config.folders != folders {
            self.config.folders = folders.to_vec();
            self.watch = None;
        }
        self.start_watching();
    }

    fn start_watching(&mut self) {
        if self.watch.is_none()
            && self.config.watch_folders
            && self.config.scans_allowed
            && !self.config.folders.is_empty()
        {
            self.watch_tried = Some(Instant::now());
            self.watch = FolderWatch::new(&self.config.folders);
        }
    }

    /// Watches the folders once they are there, when they were not: a drive mounted later.
    fn watch_when_there(&mut self) {
        let folders = &self.config.folders;
        let all = self
            .watch
            .as_ref()
            .is_some_and(|watch| watch.watching() == folders.len());
        let tried_lately = self
            .watch_tried
            .is_some_and(|at| at.elapsed() < SCAN_CHECK_INTERVAL);
        if all || tried_lately || !self.config.watch_folders {
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
            .filter(|c| self.config.folders.iter().any(|f| c.starts_with(f)))
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
        let Some(every) = self.config.scan_every else {
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
            online: self.config.online.clone(),
        };
        self.scanner
            .start(self.database.clone(), self.config.folders.clone(), options);
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
        if !self.config.online.any() {
            return;
        }
        self.scanner.fetch(
            self.database.clone(),
            artists,
            self.config.online.clone(),
            pictures.clone(),
        );
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
            folders: vec![dir.join("music")],
            watch_folders: false,
            ..ServiceConfig::default()
        };
        let service = Service::start(dir.join("library.db"), None, config).unwrap();
        let events = service.subscribe();
        assert!(service.is_scanning(), "an empty library is read at once");
        // Noted while the scan writes: it waits for it.
        let track = dir.join("music/sine.flac");
        let written = service.record_play(&track, None, SystemTime::now());
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
        assert!(!service.is_scanning());
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
            folders: vec![dir.join("music")],
            scans_allowed: false,
            ..ServiceConfig::default()
        };
        let service = Service::start(dir.join("library.db"), None, config).unwrap();
        service.rescan();
        assert!(!service.is_scanning());
        drop(service);
        let library = Library::open(&dir.join("library.db")).unwrap();
        assert!(library.all_tracks().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
