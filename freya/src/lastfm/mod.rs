//! Scrobbling to Last.fm, if the user connects their account (U8 in PLAN.md).
//!
//! A thread of its own talks to Last.fm, so a slow network never holds up the player: the app
//! hands it what plays, and reads back how it is going. Plays wait in a queue on disk until
//! Last.fm has them, and are sent again with growing pauses while it cannot be reached.

mod api;
mod md5;
mod queue;
mod session;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use api::{Client, Credentials};
pub use api::{Scrobble, Track};
use queue::Queue;

/// While the user decides on Last.fm's page, Freya asks this often whether they have allowed it.
const AUTH_POLL: Duration = Duration::from_secs(3);
/// And gives up after this long; a request token lasts an hour.
const AUTH_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The first wait before sending again after a failure; it doubles up to `MAX_RETRY`.
const FIRST_RETRY: Duration = Duration::from_secs(30);
const MAX_RETRY: Duration = Duration::from_secs(30 * 60);
/// Tracks this short are never scrobbled, as Last.fm asks.
const MIN_DURATION: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    /// This build has no Last.fm API key.
    #[default]
    Unavailable,
    Disconnected,
    /// Waiting for the user to allow Freya on Last.fm's page.
    Authorizing,
    Connected,
}

impl State {
    /// The name the skin API uses.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Disconnected => "disconnected",
            Self::Authorizing => "authorizing",
            Self::Connected => "connected",
        }
    }
}

/// How scrobbling is going, for the settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub state: State,
    /// The connected Last.fm user.
    pub user: String,
    /// Plays waiting to be sent.
    pub waiting: usize,
    /// What went wrong last, until something works again.
    pub error: String,
}

enum Command {
    Connect,
    Cancel,
    Disconnect,
    NowPlaying(Track),
    Scrobble(Scrobble),
}

pub struct LastFm {
    commands: Sender<Command>,
    status: Arc<Mutex<Status>>,
}

impl LastFm {
    /// Starts the scrobbling thread. `user` is who was connected last time. The app reads
    /// [`Self::status`] on its ticks.
    pub fn start(user: Option<String>) -> Self {
        let (commands, receiver) = channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let worker = Worker {
            client: Credentials::of_build().map(Client::new),
            status: Arc::clone(&status),
            session: None,
            queue: Queue::default(),
            token: None,
            retry: None,
            retry_delay: FIRST_RETRY,
        };
        let spawned = std::thread::Builder::new()
            .name("freya-lastfm".into())
            .spawn(move || worker.run(user, &receiver));
        if let Err(e) = spawned {
            log::warn!("could not start Last.fm scrobbling: {e}");
        }
        Self { commands, status }
    }

    pub fn status(&self) -> Status {
        lock(&self.status).clone()
    }

    /// Opens Last.fm's page in the browser, where the user lets Freya scrobble for them.
    pub fn connect(&self) {
        let _ = self.commands.send(Command::Connect);
    }

    /// Stops waiting for the user to allow Freya.
    pub fn cancel(&self) {
        let _ = self.commands.send(Command::Cancel);
    }

    /// Forgets the session and the plays waiting.
    pub fn disconnect(&self) {
        let _ = self.commands.send(Command::Disconnect);
    }

    pub fn now_playing(&self, track: Track) {
        let _ = self.commands.send(Command::NowPlaying(track));
    }

    pub fn scrobble(&self, scrobble: Scrobble) {
        let _ = self.commands.send(Command::Scrobble(scrobble));
    }
}

/// The track Last.fm is told about, if it may be: tagged with an artist and a title (never
/// guessed from file names), and not a placeholder like "Unknown Artist".
pub fn track(
    artist: Option<&str>,
    title: Option<&str>,
    album: Option<&str>,
    duration: Option<Duration>,
) -> Option<Track> {
    let tag = |value: Option<&str>| {
        let value = value?.trim();
        (!value.is_empty() && !is_placeholder(value)).then(|| value.to_owned())
    };
    Some(Track {
        artist: tag(artist)?,
        title: tag(title)?,
        album: tag(album),
        duration: duration.map(|d| d.as_secs()),
    })
}

/// Whether a play of a track this long may be scrobbled: not when it is 30 seconds or
/// shorter. One of unknown length is, once enough of it was heard.
pub fn long_enough(duration: Option<Duration>) -> bool {
    duration.is_none_or(|d| d > MIN_DURATION)
}

/// Seconds since 1970, as Last.fm counts when a play started.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn is_placeholder(value: &str) -> bool {
    let value = value.to_lowercase();
    [
        "unknown",
        "unknown artist",
        "unknown track",
        "unknown title",
        "[unknown]",
        "<unknown>",
    ]
    .contains(&value.as_str())
}

fn queue_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Freya").map(|d| d.data_dir().join("lastfm-queue.json"))
}

fn lock(status: &Mutex<Status>) -> MutexGuard<'_, Status> {
    status.lock().unwrap_or_else(|e| e.into_inner())
}

struct Worker {
    /// `None` when this build has no API key.
    client: Option<Client>,
    status: Arc<Mutex<Status>>,
    /// The connected user's name and session key.
    session: Option<(String, String)>,
    queue: Queue,
    /// The request token the user is asked to allow, and when it was asked for.
    token: Option<(String, Instant)>,
    /// When to send the waiting plays again, after a failure.
    retry: Option<Instant>,
    retry_delay: Duration,
}

impl Worker {
    fn run(mut self, user: Option<String>, commands: &Receiver<Command>) {
        if self.client.is_some() {
            match user.and_then(|u| session::load(&u).map(|key| (u, key))) {
                Some((user, key)) => self.connected(user, key),
                None => self.set(|s| s.state = State::Disconnected),
            }
        }
        loop {
            let command = match self.next_deadline() {
                Some(deadline) => {
                    match commands.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    {
                        Ok(command) => Some(command),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                None => match commands.recv() {
                    Ok(command) => Some(command),
                    Err(_) => return,
                },
            };
            match command {
                Some(command) => self.handle(command),
                None => self.on_time(),
            }
        }
    }

    /// When something is due without a command: asking about the token, or sending again.
    fn next_deadline(&self) -> Option<Instant> {
        if self.token.is_some() {
            return Some(Instant::now() + AUTH_POLL);
        }
        self.retry
    }

    fn handle(&mut self, command: Command) {
        let Some(client) = &self.client else {
            return;
        };
        match command {
            Command::Connect => match client.token() {
                Ok(token) => {
                    crate::update::open_page(&client.auth_page(&token));
                    self.token = Some((token, Instant::now()));
                    self.set(|s| {
                        s.state = State::Authorizing;
                        s.error.clear();
                    });
                }
                Err(e) => self.set(|s| s.error = e.to_string()),
            },
            Command::Cancel => {
                self.token = None;
                if self.session.is_none() {
                    self.set(|s| s.state = State::Disconnected);
                }
            }
            Command::Disconnect => {
                if let Some((user, _)) = self.session.take() {
                    session::delete(&user);
                }
                self.queue.clear();
                self.token = None;
                self.retry = None;
                self.set(|s| {
                    *s = Status {
                        state: State::Disconnected,
                        ..Status::default()
                    }
                });
            }
            Command::NowPlaying(track) => {
                if let Some((_, key)) = &self.session
                    && let Err(e) = client.now_playing(key, &track)
                {
                    log::debug!("Last.fm now playing: {e}");
                    if e.is_session_invalid() {
                        self.session_lost(&e);
                    }
                }
            }
            Command::Scrobble(scrobble) => {
                if self.session.is_some() {
                    self.queue.push(scrobble);
                    self.send_waiting();
                }
            }
        }
    }

    fn on_time(&mut self) {
        if let Some((token, asked)) = self.token.clone()
            && let Some(client) = &self.client
        {
            if asked.elapsed() > AUTH_TIMEOUT {
                self.token = None;
                self.set(|s| {
                    s.state = State::Disconnected;
                    s.error =
                        "Freya was not allowed on Last.fm in time. Connect again to retry.".into();
                });
                return;
            }
            match client.session(&token) {
                Ok(session) => {
                    self.token = None;
                    let kept = session::save(&session.user, &session.key);
                    self.connected(session.user, session.key);
                    if let Err(e) = kept {
                        log::warn!("{e}");
                        self.set(|s| {
                            s.error = format!("{e}: connect again the next time Freya starts.");
                        });
                    }
                }
                // The user has not decided yet.
                Err(e) if e.is_unauthorized_token() || e.is_temporary() => {}
                Err(e) => {
                    self.token = None;
                    self.set(|s| {
                        s.state = State::Disconnected;
                        s.error = e.to_string();
                    });
                }
            }
            return;
        }
        if self.retry.is_some_and(|at| Instant::now() >= at) {
            self.retry = None;
            self.send_waiting();
        }
    }

    fn connected(&mut self, user: String, key: String) {
        self.queue = Queue::load(queue_path(), &user);
        self.session = Some((user.clone(), key));
        let waiting = self.queue.len();
        self.set(|s| {
            s.state = State::Connected;
            s.user = user;
            s.waiting = waiting;
            s.error.clear();
        });
        self.send_waiting();
    }

    /// Sends the waiting plays, oldest first, as many at a time as Last.fm takes.
    fn send_waiting(&mut self) {
        let (Some(client), Some((_, key))) = (&self.client, &self.session) else {
            return;
        };
        while !self.queue.is_empty() {
            let batch = self.queue.oldest(api::MAX_BATCH).to_vec();
            match client.scrobble(key, &batch) {
                Ok(accepted) => {
                    if accepted < batch.len() {
                        log::info!(
                            "Last.fm took {accepted} of {} plays; it ignores the others",
                            batch.len()
                        );
                    }
                    self.queue.remove_oldest(batch.len());
                    self.retry_delay = FIRST_RETRY;
                    let waiting = self.queue.len();
                    self.set(|s| {
                        s.waiting = waiting;
                        s.error.clear();
                    });
                }
                Err(e) if e.is_temporary() => {
                    self.retry = Some(Instant::now() + self.retry_delay);
                    self.retry_delay = (self.retry_delay * 2).min(MAX_RETRY);
                    let waiting = self.queue.len();
                    self.set(|s| {
                        s.waiting = waiting;
                        s.error = e.to_string();
                    });
                    return;
                }
                Err(e) if e.is_session_invalid() => {
                    self.session_lost(&e);
                    return;
                }
                Err(e) => {
                    // Last.fm will never take these: sending them again would block the rest.
                    log::warn!("Last.fm refused {} plays: {e}", batch.len());
                    self.queue.remove_oldest(batch.len());
                    let waiting = self.queue.len();
                    self.set(|s| {
                        s.waiting = waiting;
                        s.error = e.to_string();
                    });
                }
            }
        }
    }

    /// Last.fm no longer takes the session: the user has to connect again. Their waiting
    /// plays stay, for when they do.
    fn session_lost(&mut self, error: &api::Error) {
        if let Some((user, _)) = self.session.take() {
            session::delete(&user);
        }
        self.retry = None;
        self.set(|s| {
            s.state = State::Disconnected;
            s.error = format!("{error}. Connect again to keep scrobbling.");
        });
    }

    fn set(&self, change: impl FnOnce(&mut Status)) {
        change(&mut lock(&self.status));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tagged_tracks_are_told_about() {
        let track = track(Some(" Opeth "), Some("Blackwater Park"), Some(""), None).unwrap();
        assert_eq!(track.artist, "Opeth");
        assert_eq!(track.album, None);
        assert!(super::track(None, Some("Song"), None, None).is_none());
        assert!(super::track(Some("Unknown Artist"), Some("Song"), None, None).is_none());
        assert!(super::track(Some("Artist"), Some("  "), None, None).is_none());
    }

    #[test]
    fn short_tracks_are_not_scrobbled() {
        assert!(!long_enough(Some(Duration::from_secs(30))));
        assert!(long_enough(Some(Duration::from_secs(31))));
        assert!(long_enough(None));
    }
}
