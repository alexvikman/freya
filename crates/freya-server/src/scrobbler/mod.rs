//! Scrobbling to Last.fm for the user, once they connect their account.
//!
//! A thread of its own talks to Last.fm, so a slow network never holds up the server: clients
//! hand it what plays, and are told how it is going. Plays wait in a queue on disk until
//! Last.fm has them, and are sent again with growing pauses while it cannot be reached.

mod queue;
mod session;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use freya_lastfm::{Client, Credentials, Scrobble, Track};
use freya_protocol::{LastFmState as State, LastFmStatus as Status};
use queue::Queue;

/// While the user decides on Last.fm's page, Freya asks this often whether they have allowed it.
const AUTH_POLL: Duration = Duration::from_secs(3);
/// And gives up after this long; a request token lasts an hour.
const AUTH_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The first wait before sending again after a failure; it doubles up to `MAX_RETRY`.
const FIRST_RETRY: Duration = Duration::from_secs(30);
const MAX_RETRY: Duration = Duration::from_secs(30 * 60);
/// Where the connected user's name is kept in the folder, and the plays waiting.
pub const USER_FILE: &str = "lastfm-user";
pub const QUEUE_FILE: &str = "lastfm-queue.json";

pub use session::file_name as session_file;

/// What tells clients how scrobbling goes, whenever that changes.
pub type Tell = Arc<dyn Fn(Status) + Send + Sync>;

enum Command {
    /// Wait for the user to allow the request token.
    Authorize(String),
    Cancel,
    Disconnect,
    NowPlaying(Track),
    Scrobble(Scrobble),
}

pub struct Scrobbler {
    /// `None` once [`Self::finish`] has let the thread end.
    commands: Option<Sender<Command>>,
    status: Arc<Mutex<Status>>,
    /// Disconnected when the thread has ended.
    ended: Receiver<()>,
    /// For asking Last.fm for a request token; `None` when this build has no API key.
    client: Option<Client>,
}

impl Scrobbler {
    /// Starts the scrobbling thread, which keeps the session, the waiting plays and who is
    /// connected in `dir`, and tells `tell` how it goes whenever that changes.
    pub fn start(dir: PathBuf, tell: Tell) -> Self {
        let user = std::fs::read_to_string(dir.join(USER_FILE))
            .ok()
            .map(|user| user.trim().to_owned())
            .filter(|user| !user.is_empty());
        let (commands, receiver) = channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let worker = Worker {
            client: Credentials::of_build().map(Client::new),
            dir,
            tell,
            status: Arc::clone(&status),
            session: None,
            queue: Queue::default(),
            token: None,
            retry: None,
            retry_delay: FIRST_RETRY,
        };
        let (ending, ended) = channel::<()>();
        let spawned = std::thread::Builder::new()
            .name("freya-lastfm".into())
            .spawn(move || {
                worker.run(user, &receiver);
                drop(ending);
            });
        if let Err(e) = spawned {
            log::warn!("could not start Last.fm scrobbling: {e}");
        }
        Self {
            commands: Some(commands),
            status,
            ended,
            client: Credentials::of_build().map(Client::new),
        }
    }

    /// As the server stops: lets the thread handle what it was given, plays to scrobble first among
    /// them (they are put in the queue on disk before anything is sent), and waits for it a
    /// moment. Without this, plays counted just before quitting were lost.
    pub fn finish(&mut self) {
        if self.commands.take().is_some() {
            let _ = self.ended.recv_timeout(Duration::from_secs(2));
        }
    }

    fn send(&self, command: Command) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(command);
        }
    }

    pub fn status(&self) -> Status {
        lock(&self.status).clone()
    }

    /// Asks Last.fm for a request token and waits for the user to allow it: the page where
    /// they do, for their browser.
    pub fn connect(&self) -> Result<String, String> {
        let client = self
            .client
            .as_ref()
            .ok_or("this server has no Last.fm key")?;
        let token = client.token().map_err(|e| e.to_string())?;
        let page = client.auth_page(&token);
        self.send(Command::Authorize(token));
        Ok(page)
    }

    /// Stops waiting for the user to allow Freya.
    pub fn cancel(&self) {
        self.send(Command::Cancel);
    }

    /// Forgets the session and the plays waiting.
    pub fn disconnect(&self) {
        self.send(Command::Disconnect);
    }

    pub fn now_playing(&self, track: Track) {
        self.send(Command::NowPlaying(track));
    }

    pub fn scrobble(&self, scrobble: Scrobble) {
        self.send(Command::Scrobble(scrobble));
    }
}

fn lock(status: &Mutex<Status>) -> MutexGuard<'_, Status> {
    status.lock().unwrap_or_else(|e| e.into_inner())
}

struct Worker {
    /// `None` when this build has no API key.
    client: Option<Client>,
    /// Where the session, the waiting plays and who is connected are kept.
    dir: PathBuf,
    tell: Tell,
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
            match user.map(|u| (session::load(&self.dir, &u), u)) {
                Some((Ok(Some(key)), user)) => self.connected(user, key),
                Some((Err(e), user)) => {
                    log::warn!("{e}");
                    self.set(|s| {
                        s.state = State::Unreadable;
                        s.user = user;
                        s.error = format!("{e}. Connect again to keep scrobbling.");
                    });
                }
                Some((Ok(None), _)) | None => self.set(|s| s.state = State::Disconnected),
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
            Command::Authorize(token) => {
                self.token = Some((token, Instant::now()));
                self.set(|s| {
                    s.state = State::Authorizing;
                    s.error.clear();
                });
            }
            Command::Cancel => {
                self.token = None;
                if self.session.is_none() {
                    self.set(|s| s.state = State::Disconnected);
                }
            }
            Command::Disconnect => {
                if let Some((user, _)) = self.session.take() {
                    session::delete(&self.dir, &user);
                }
                self.forget_user();
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
                    let kept = session::save(&self.dir, &session.user, &session.key);
                    self.connected(session.user, session.key);
                    if let Err(e) = kept {
                        log::warn!("{e}");
                        self.set(|s| {
                            s.error = format!("{e}: connect again once the server starts again.");
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
        if let Err(e) = std::fs::write(self.dir.join(USER_FILE), &user) {
            log::warn!("could not keep who is connected to Last.fm: {e}");
        }
        self.queue = Queue::load(Some(self.dir.join(QUEUE_FILE)), &user);
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
            let batch = self.queue.oldest(freya_lastfm::MAX_BATCH).to_vec();
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
                    let delay = self.retry_delay;
                    self.retry_delay = (self.retry_delay * 2).min(MAX_RETRY);
                    self.wait_to_retry(&e, delay);
                    return;
                }
                Err(e) if e.is_session_invalid() => {
                    self.session_lost(&e);
                    return;
                }
                // Last.fm finds the one play itself invalid: it will never take it, and
                // sending it again would block the rest.
                Err(e) if e.is_invalid_play() && batch.len() == 1 => {
                    log::warn!("Last.fm refused a play: {e}");
                    self.queue.remove_oldest(1);
                    let waiting = self.queue.len();
                    self.set(|s| {
                        s.waiting = waiting;
                        s.error = e.to_string();
                    });
                }
                // Which play of the batch? Sent one at a time until it is found.
                Err(e) if e.is_invalid_play() => {
                    if let Err(e) = client.scrobble(key, &batch[..1]) {
                        if e.is_invalid_play() {
                            log::warn!("Last.fm refused a play: {e}");
                            self.queue.remove_oldest(1);
                        } else {
                            self.wait_to_retry(&e, MAX_RETRY);
                            return;
                        }
                    } else {
                        self.queue.remove_oldest(1);
                    }
                }
                // Anything else is kept, and sent again later: a refusal Freya does not
                // understand must not cost the user their plays.
                Err(e) => {
                    log::warn!("Last.fm refused {} plays: {e}", batch.len());
                    self.wait_to_retry(&e, MAX_RETRY);
                    return;
                }
            }
        }
    }

    /// Sends the waiting plays again after `delay`, and shows why they wait.
    fn wait_to_retry(&mut self, error: &freya_lastfm::Error, delay: Duration) {
        self.retry = Some(Instant::now() + delay);
        let waiting = self.queue.len();
        let error = error.to_string();
        self.set(|s| {
            s.waiting = waiting;
            s.error = error;
        });
    }

    /// Last.fm no longer takes the session: the user has to connect again. Their waiting
    /// plays stay, for when they do.
    fn session_lost(&mut self, error: &freya_lastfm::Error) {
        if let Some((user, _)) = self.session.take() {
            session::delete(&self.dir, &user);
        }
        self.forget_user();
        self.retry = None;
        self.set(|s| {
            s.state = State::Disconnected;
            s.error = format!("{error}. Connect again to keep scrobbling.");
        });
    }

    /// Nobody is connected any longer.
    fn forget_user(&self) {
        let _ = std::fs::remove_file(self.dir.join(USER_FILE));
    }

    /// Changes the status, and tells the clients when it changed.
    fn set(&self, change: impl FnOnce(&mut Status)) {
        let changed = {
            let mut status = lock(&self.status);
            let before = status.clone();
            change(&mut status);
            (*status != before).then(|| status.clone())
        };
        if let Some(status) = changed {
            (self.tell)(status);
        }
    }
}
