//! Updates: Freya looks for a newer release, tells the user, and installs it only when they
//! say so. Checking and downloading run on a thread of their own; the app polls for news.
//!
//! A release is only installed if its `SHA256SUMS` carries a signature made with a key built
//! into Freya, and the downloaded file matches its line there.

mod install;
mod release;
mod signing;

use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, SystemTime};

use slint_interpreter::Value;

pub use install::Target;
pub use release::Release;

/// How often Freya looks for an update on its own, when the user lets it.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Debug)]
pub enum State {
    Idle,
    Checking {
        /// Asked for by the user, who wants to hear the answer either way.
        asked: bool,
    },
    UpToDate,
    Available(Release),
    Downloading {
        release: Release,
        /// 0 to 1, as far as known.
        progress: f32,
    },
    /// Installed; Freya restarts into it.
    Installed(Release),
    Failed(String),
}

/// What happened on the thread.
enum News {
    Checked(anyhow::Result<Option<Release>>),
    Progress(f32),
    Installed(anyhow::Result<()>),
}

pub struct Updater {
    state: State,
    current: &'static str,
    keys: &'static [&'static str],
    feed: String,
    target: Option<Target>,
    news: Option<Receiver<News>>,
}

impl Updater {
    pub fn new() -> Self {
        let target = Target::current();
        if let Some(target) = &target {
            target.clean_up();
        }
        Self::with(release::feed_url(), signing::RELEASE_KEYS, target)
    }

    fn with(feed: String, keys: &'static [&'static str], target: Option<Target>) -> Self {
        Self {
            state: State::Idle,
            current: env!("CARGO_PKG_VERSION"),
            keys,
            feed,
            target,
            news: None,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    fn busy(&self) -> bool {
        self.news.is_some()
    }

    /// Looks for a newer release. `asked` when the user asked: then the answer is shown even
    /// when there is nothing new, and a skipped version is offered again.
    pub fn check(&mut self, asked: bool, skipped: Option<&str>) {
        if self.busy() {
            return;
        }
        self.state = State::Checking { asked };
        let (send, news) = channel();
        let feed = self.feed.clone();
        let current = self.current;
        let skipped = if asked {
            None
        } else {
            skipped.map(str::to_owned)
        };
        std::thread::Builder::new()
            .name("freya-update".into())
            .spawn(move || {
                let found = release::latest(&release::agent(), &feed).map(|latest| {
                    latest.filter(|r| {
                        release::is_newer(r.version(), current)
                            && skipped.as_deref() != Some(r.version())
                    })
                });
                let _ = send.send(News::Checked(found));
            })
            .expect("spawn update thread");
        self.news = Some(news);
    }

    /// Whether Freya can install the update itself; otherwise `install` opens the release's
    /// page.
    pub fn can_install(&self) -> bool {
        self.target.is_some() && !self.keys.is_empty()
    }

    /// Installs the available update, or opens its page where Freya cannot.
    pub fn install(&mut self) {
        let State::Available(release) = &self.state else {
            return;
        };
        let release = release.clone();
        let Some(target) = self.target.clone().filter(|_| !self.keys.is_empty()) else {
            open_page(&release.html_url);
            return;
        };
        if self.busy() {
            return;
        }
        self.state = State::Downloading {
            release: release.clone(),
            progress: 0.0,
        };
        let (send, news) = channel();
        let keys = self.keys;
        std::thread::Builder::new()
            .name("freya-update".into())
            .spawn(move || {
                let file = target.asset(release.version());
                let progress = send.clone();
                let result = release::download_verified(
                    &release::agent(),
                    &release,
                    &file,
                    keys,
                    move |done, size| {
                        if let Some(size) = size.filter(|&s| s > 0) {
                            let _ = progress.send(News::Progress(done as f32 / size as f32));
                        }
                    },
                )
                .and_then(|package| target.install(&package));
                let _ = send.send(News::Installed(result));
            })
            .expect("spawn update thread");
        self.news = Some(news);
    }

    /// Hides the update until Freya looks again.
    pub fn later(&mut self) {
        if !self.busy() {
            self.state = State::Idle;
        }
    }

    /// Takes in what the thread found. Returns true when the state changed.
    pub fn poll(&mut self) -> bool {
        let Some(news) = &self.news else {
            return false;
        };
        let mut changed = false;
        while let Ok(item) = news.try_recv() {
            changed = true;
            let asked = matches!(self.state, State::Checking { asked: true });
            match item {
                News::Checked(Ok(Some(release))) => {
                    log::info!("Freya {} is available", release.version());
                    self.state = State::Available(release);
                }
                News::Checked(Ok(None)) => {
                    self.state = if asked { State::UpToDate } else { State::Idle };
                }
                News::Checked(Err(e)) => {
                    log::info!("could not look for updates: {e:#}");
                    self.state = if asked {
                        State::Failed(format!("Could not look for updates: {e:#}"))
                    } else {
                        State::Idle
                    };
                }
                News::Progress(progress) => {
                    if let State::Downloading {
                        progress: shown, ..
                    } = &mut self.state
                    {
                        *shown = progress.clamp(0.0, 1.0);
                    }
                }
                News::Installed(result) => {
                    let release = match &self.state {
                        State::Downloading { release, .. } => release.clone(),
                        _ => continue,
                    };
                    self.state = match result {
                        Ok(()) => State::Installed(release),
                        Err(e) => {
                            log::warn!("the update failed: {e:#}");
                            State::Failed(format!("The update failed: {e:#}"))
                        }
                    };
                }
            }
            if matches!(
                self.state,
                State::Available(_)
                    | State::UpToDate
                    | State::Idle
                    | State::Installed(_)
                    | State::Failed(_)
            ) {
                self.news = None;
                break;
            }
        }
        changed
    }

    /// Starts the installed Freya once this one has quit. Returns false if it could not.
    pub fn restart(&mut self) -> bool {
        let Some(target) = &self.target else {
            return false;
        };
        match target.restart() {
            Ok(()) => true,
            Err(e) => {
                self.state = State::Failed(format!("{e:#}"));
                false
            }
        }
    }

    /// Values for the skin's `Updates` global, except `check-automatically`.
    pub fn values(&self) -> Vec<(&'static str, Value)> {
        let text = |s: &str| Value::String(s.into());
        let (state, release, progress, error) = match &self.state {
            State::Idle => ("idle", None, 0.0, ""),
            State::Checking { asked } => (if *asked { "checking" } else { "idle" }, None, 0.0, ""),
            State::UpToDate => ("up-to-date", None, 0.0, ""),
            State::Available(release) => ("available", Some(release), 0.0, ""),
            State::Downloading { release, progress } => {
                ("downloading", Some(release), *progress, "")
            }
            State::Installed(release) => ("installed", Some(release), 1.0, ""),
            State::Failed(error) => ("failed", None, 0.0, error.as_str()),
        };
        vec![
            ("state", text(state)),
            ("current-version", text(self.current)),
            ("version", text(release.map_or("", Release::version))),
            (
                "notes",
                text(
                    &release
                        .and_then(|r| r.body.as_deref())
                        .map(release::plain_notes)
                        .unwrap_or_default(),
                ),
            ),
            ("progress", Value::Number(f64::from(progress))),
            ("error", text(error)),
            ("can-install", Value::Bool(self.can_install())),
        ]
    }
}

/// Whether it is time to look for an update on Freya's own, last done at `last` (seconds since
/// 1970).
pub fn due(last: Option<u64>, now: SystemTime) -> bool {
    let now = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    last.is_none_or(|last| now.saturating_sub(last) >= CHECK_INTERVAL.as_secs())
}

/// Opens a web page in the user's browser.
fn open_page(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(opener).arg(url).spawn() {
        log::warn!("could not open {url}: {e}");
    }
}

#[cfg(test)]
mod tests;
