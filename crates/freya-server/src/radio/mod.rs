//! The radio directory: the stations Freya picked, and Radio Browser's, which the server
//! fetches about once a week and keeps in its folder, to find by their tags and names. What the
//! server keeps for its users to play is apart from it (see `crate::stations`).

mod browser;
mod featured;
mod index;
mod tags;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant, SystemTime};

use freya_protocol::{RadioDirectoryStatus, RadioQuery, RadioResults, RadioShelf, RadioTag};

use browser::Fetched;
use featured::{Featured, Shelf};
use index::Index;
use tags::Tags;

/// Radio Browser's stations are fetched again once they are this old.
const FETCH_EVERY: Duration = Duration::from_secs(7 * 24 * 3600);
/// After fetching failed, it is tried again this much later; sooner while the directory has
/// none of Radio Browser's stations.
const RETRY_AFTER: Duration = Duration::from_secs(6 * 3600);
const RETRY_FIRST_AFTER: Duration = Duration::from_secs(10 * 60);
/// How many stations a row of the first page has.
const SHELF: u32 = 12;
/// A row with fewer stations than this is left out.
const FEWEST_ON_SHELF: usize = 3;
/// A station's plays are told Radio Browser at most this often, whoever plays it.
const PLAYS_EVERY: Duration = Duration::from_secs(3600);

/// What tells clients the directory changed.
pub type Tell = Arc<dyn Fn() + Send + Sync>;

/// The directory, kept up to date on a thread of its own.
pub struct Directory {
    shared: Arc<Shared>,
}

struct Shared {
    /// Where Radio Browser's stations are kept; without, none are fetched.
    file: Option<PathBuf>,
    /// Whether Radio Browser may be asked.
    online: AtomicBool,
    stop: AtomicBool,
    /// Set, and the thread woken, when `online` changes.
    changed: (Mutex<bool>, Condvar),
    shelves: Vec<Shelf>,
    featured: Vec<Featured>,
    index: RwLock<Arc<Index>>,
    names: Mutex<Tags>,
    status: Mutex<RadioDirectoryStatus>,
    /// Radio Browser's server that answered last.
    server: Mutex<Option<String>>,
    /// When each station's play was told last.
    told: Mutex<HashMap<String, Instant>>,
    tell: Tell,
}

impl Directory {
    /// The directory, with Radio Browser's stations kept in `file` and fetched when `online`;
    /// `tell` is told whenever it changes. The stations Freya picked are in it at once.
    pub fn start(file: Option<PathBuf>, online: bool, tell: Tell) -> Self {
        let (shelves, featured) = featured::load();
        let mut names = Tags::new();
        let index = Index::build(&mut names, &featured, Vec::new());
        let status = RadioDirectoryStatus {
            state: if online && file.is_some() {
                "loading"
            } else {
                "off"
            }
            .to_owned(),
            stations: index.len(),
            ..RadioDirectoryStatus::default()
        };
        let shared = Arc::new(Shared {
            file,
            online: AtomicBool::new(online),
            stop: AtomicBool::new(false),
            changed: (Mutex::new(false), Condvar::new()),
            shelves,
            featured,
            index: RwLock::new(Arc::new(index)),
            names: Mutex::new(names),
            status: Mutex::new(status),
            server: Mutex::new(None),
            told: Mutex::new(HashMap::new()),
            tell,
        });
        if shared.file.is_some() {
            let thread = Arc::clone(&shared);
            if let Err(e) = std::thread::Builder::new()
                .name("freya-radio".into())
                .spawn(move || thread.run())
            {
                log::warn!("could not start the radio directory's thread: {e}");
            }
        }
        Self { shared }
    }

    /// Asks Radio Browser from now on, or no longer, as the server's settings say.
    pub fn set_online(&self, online: bool) {
        if self.shared.online.swap(online, Ordering::Relaxed) != online {
            self.shared.wake();
        }
    }

    pub fn status(&self) -> RadioDirectoryStatus {
        lock(&self.shared.status).clone()
    }

    pub fn tags(&self) -> Vec<RadioTag> {
        self.shared.index().tags()
    }

    pub fn search(&self, query: &RadioQuery) -> RadioResults {
        self.shared.index().search(query)
    }

    /// The first page's rows: the stations Freya picked, then each row's tags' best known.
    pub fn shelves(&self) -> Vec<RadioShelf> {
        let index = self.shared.index();
        self.shared
            .shelves
            .iter()
            .filter_map(|shelf| {
                let stations = if shelf.tags.is_empty() {
                    let picked: Vec<_> = self
                        .shared
                        .featured
                        .iter()
                        .filter_map(|f| index.get(&format!("freya:{}", f.id)).cloned())
                        .collect();
                    picked
                } else {
                    index
                        .search(&RadioQuery {
                            tags: shelf.tags.clone(),
                            limit: SHELF,
                            ..RadioQuery::default()
                        })
                        .stations
                };
                (stations.len() >= FEWEST_ON_SHELF).then(|| RadioShelf {
                    title: shelf.title.clone(),
                    tags: shelf.tags.clone(),
                    stations,
                })
            })
            .collect()
    }

    /// Tells Radio Browser the station `id` was played, if it is one of its, and Radio Browser
    /// may be asked; at most once an hour for a station, whoever plays it.
    pub fn played(&self, id: &str) {
        let shared = &self.shared;
        if !shared.online.load(Ordering::Relaxed)
            || !browser::is_station_id(id)
            || shared.index().get(id).is_none()
        {
            return;
        }
        {
            let mut told = lock(&shared.told);
            let now = Instant::now();
            told.retain(|_, at| now.duration_since(*at) < PLAYS_EVERY);
            if told.contains_key(id) {
                return;
            }
            told.insert(id.to_owned(), now);
        }
        let Some(server) = lock(&shared.server).clone() else {
            return;
        };
        let id = id.to_owned();
        let _ = std::thread::Builder::new()
            .name("freya-radio-play".into())
            .spawn(move || browser::count_play(&server, &id));
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake();
    }
}

impl Shared {
    fn index(&self) -> Arc<Index> {
        Arc::clone(&self.index.read().unwrap_or_else(|e| e.into_inner()))
    }

    fn wake(&self) {
        let (changed, woken) = &self.changed;
        *lock(changed) = true;
        woken.notify_all();
    }

    /// Reads the stations kept, then fetches them whenever they are old, while Radio Browser may
    /// be asked; until the server stops. They are not kept in memory but in the directory.
    fn run(self: Arc<Self>) {
        let mut fetched = None;
        let mut built_online = None;
        let mut tried: Option<Instant> = None;
        while !self.stop.load(Ordering::Relaxed) {
            let online = self.online.load(Ordering::Relaxed);
            if built_online != Some(online) {
                built_online = Some(online);
                if online {
                    let kept = self.read();
                    fetched = kept.fetched;
                    self.build(Some(kept));
                } else {
                    self.build(None);
                }
            }
            let old = fetched.is_none_or(|at: SystemTime| {
                SystemTime::now()
                    .duration_since(at)
                    .is_ok_and(|age| age >= FETCH_EVERY)
            });
            let wait = if fetched.is_some() {
                RETRY_AFTER
            } else {
                RETRY_FIRST_AFTER
            };
            let may_try = tried.is_none_or(|at| at.elapsed() >= wait);
            if online && old && may_try {
                tried = Some(Instant::now());
                if let Some(at) = self.fetch() {
                    fetched = Some(at);
                }
            }
            let (changed, woken) = &self.changed;
            let mut changed = lock(changed);
            if !*changed {
                changed = woken
                    .wait_timeout(changed, Duration::from_secs(60))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            *changed = false;
        }
    }

    /// The stations kept in the file, if any.
    fn read(&self) -> Fetched {
        let Some(file) = &self.file else {
            return Fetched::default();
        };
        match std::fs::read(file) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                log::warn!("could not read the radio directory {}: {e}", file.display());
                Fetched::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Fetched::default(),
            Err(e) => {
                log::warn!("could not read the radio directory {}: {e}", file.display());
                Fetched::default()
            }
        }
    }

    /// Fetches Radio Browser's stations, keeps them, and the directory is made of them; when
    /// they were fetched, if they were.
    fn fetch(&self) -> Option<SystemTime> {
        let first = self.index().listed() == 0;
        if first {
            lock(&self.status).state = "loading".into();
            (self.tell)();
        }
        let servers = browser::servers();
        match browser::fetch(&servers, &self.stop) {
            Ok((server, stations)) => {
                *lock(&self.server) = Some(server);
                let at = SystemTime::now();
                let kept = Fetched {
                    fetched: Some(at),
                    stations,
                };
                self.keep(&kept);
                lock(&self.status).error.clear();
                if self.online.load(Ordering::Relaxed) {
                    self.build(Some(kept));
                }
                Some(at)
            }
            Err(e) => {
                if let Some(server) = servers.first() {
                    lock(&self.server).get_or_insert_with(|| server.clone());
                }
                let mut status = lock(&self.status);
                status.error = e;
                if first {
                    status.state = "failed".into();
                }
                drop(status);
                (self.tell)();
                None
            }
        }
    }

    /// Writes the stations to the file, through a new one renamed over it.
    fn keep(&self, kept: &Fetched) {
        let Some(file) = &self.file else {
            return;
        };
        let written = serde_json::to_vec(kept)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                if let Some(dir) = file.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                let partial = file.with_extension("partial");
                std::fs::write(&partial, bytes).map_err(|e| e.to_string())?;
                std::fs::rename(&partial, file).map_err(|e| e.to_string())
            });
        if let Err(e) = written {
            log::warn!(
                "could not keep the radio directory in {}: {e}",
                file.display()
            );
        }
    }

    /// Makes the directory of the stations Freya picked, and of `kept` when Radio Browser may be
    /// asked; and tells the clients.
    fn build(&self, kept: Option<Fetched>) {
        let online = kept.is_some();
        let (fetched, listed) = kept.map_or((None, Vec::new()), |k| (k.fetched, k.stations));
        let index = {
            let mut names = lock(&self.names);
            Index::build(&mut names, &self.featured, listed)
        };
        let mut status = lock(&self.status);
        status.stations = index.len();
        status.fetched = fetched;
        status.state = if !online {
            "off"
        } else if index.listed() > 0 {
            "ready"
        } else if !status.error.is_empty() {
            "failed"
        } else {
            "loading"
        }
        .to_owned();
        drop(status);
        *self.index.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(index);
        (self.tell)();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_the_picked_stations_without_radio_browser() {
        let directory = Directory::start(None, true, Arc::new(|| {}));
        let status = directory.status();
        assert_eq!(status.state, "off");
        assert!(status.stations > 0);
        let shelves = directory.shelves();
        assert_eq!(shelves[0].title, "Picked by Freya");
        assert!(shelves[0].stations.iter().all(|s| s.featured));
        let jazz = directory.search(&RadioQuery {
            tags: vec!["genre:jazz".into()],
            limit: 10,
            ..RadioQuery::default()
        });
        assert!(jazz.stations.iter().any(|s| s.id == "freya:fip"));
        assert!(directory.tags().iter().any(|t| t.id == "country:fr"));
        // Not one of Radio Browser's, so not told.
        directory.played("freya:fip");
    }

    #[test]
    fn reads_the_stations_kept() {
        let dir = std::env::temp_dir().join(format!("freya-radio-kept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("radio-directory.json");
        let kept = Fetched {
            fetched: Some(SystemTime::now()),
            stations: vec![browser::Listed {
                stationuuid: "96062a7b-0601-11e8-ae97-52543be04c81".into(),
                name: "Radio Jazz Italia".into(),
                url: "https://jazz.example.org/stream".into(),
                tags: "jazz".into(),
                countrycode: "IT".into(),
                codec: "MP3".into(),
                bitrate: 128,
                lastcheckok: 1,
                ..browser::Listed::default()
            }],
        };
        std::fs::write(&file, serde_json::to_vec(&kept).unwrap()).unwrap();
        let (told, heard) = std::sync::mpsc::channel();
        let told = Mutex::new(told);
        let directory = Directory::start(
            Some(file),
            true,
            Arc::new(move || {
                let _ = lock(&told).send(());
            }),
        );
        heard.recv_timeout(Duration::from_secs(10)).unwrap();
        let status = directory.status();
        assert_eq!(status.state, "ready");
        let italy = directory.search(&RadioQuery {
            tags: vec!["country:it".into(), "genre:jazz".into()],
            limit: 10,
            ..RadioQuery::default()
        });
        assert_eq!(italy.stations[0].name, "Radio Jazz Italia");
        // Taken off Radio Browser, only the picked stations are left.
        directory.set_online(false);
        heard.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(directory.status().state, "off");
        assert!(
            directory
                .search(&RadioQuery {
                    tags: vec!["country:it".into()],
                    limit: 10,
                    ..RadioQuery::default()
                })
                .stations
                .is_empty()
        );
        drop(directory);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
