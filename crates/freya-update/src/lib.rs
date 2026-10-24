//! Updates for Freya and Freya Server: each looks for a newer release of itself, tells its user,
//! and installs it only when they say so. Checking and downloading run on a thread of their
//! own; the program polls for news.
//!
//! A release is only installed if its `SHA256SUMS` carries a signature made with a key built
//! in here, and the downloaded file matches its line there.

mod install;
mod release;
mod signing;

use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, SystemTime};

pub use install::{Place, Target};
pub use release::{REPOSITORY, Release, plain_notes};

/// How often a program looks for an update on its own, when its user lets it.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// What is updated: the release files and app bundle of each are named after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Product {
    Freya,
    Server,
}

impl Product {
    /// What it is called to its user, "Freya" or "Freya Server".
    pub fn name(self) -> &'static str {
        match self {
            Self::Freya => "Freya",
            Self::Server => "Freya Server",
        }
    }

    /// How its release files begin, "Freya" or "FreyaServer".
    pub fn asset_prefix(self) -> &'static str {
        match self {
            Self::Freya => "Freya",
            Self::Server => "FreyaServer",
        }
    }
}

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
    /// Installed; the program restarts into it.
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
    product: Product,
    state: State,
    current: &'static str,
    keys: &'static [&'static str],
    feed: String,
    target: Option<Target>,
    news: Option<Receiver<News>>,
}

impl Updater {
    /// The updater of `product`, the running program, of version `current`.
    pub fn new(product: Product, current: &'static str) -> Self {
        let target = Target::current(product);
        if let Some(target) = &target {
            target.clean_up();
        }
        Self::with(
            product,
            current,
            release::feed_url(),
            signing::RELEASE_KEYS,
            target,
        )
    }

    fn with(
        product: Product,
        current: &'static str,
        feed: String,
        keys: &'static [&'static str],
        target: Option<Target>,
    ) -> Self {
        Self {
            product,
            state: State::Idle,
            current,
            keys,
            feed,
            target,
            news: None,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// The running program's version.
    pub fn current(&self) -> &'static str {
        self.current
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
        let spawned = std::thread::Builder::new()
            .name("freya-update".into())
            .spawn(move || {
                let found = release::latest(&release::agent(), &feed).map(|latest| {
                    latest.filter(|r| {
                        release::is_newer(r.version(), current)
                            && skipped.as_deref() != Some(r.version())
                    })
                });
                let _ = send.send(News::Checked(found));
            });
        match spawned {
            Ok(_) => self.news = Some(news),
            Err(e) => self.state = State::Failed(format!("Could not look for updates: {e}")),
        }
    }

    /// Whether the program can install the update itself; otherwise `install` gives the
    /// release's page to open.
    pub fn can_install(&self) -> bool {
        self.target.is_some() && !self.keys.is_empty()
    }

    /// Installs the available update. Where the program cannot install itself, as from a
    /// Windows installer or a `.deb`, the release's page to open instead.
    pub fn install(&mut self) -> Option<String> {
        let State::Available(release) = &self.state else {
            return None;
        };
        let release = release.clone();
        let Some(target) = self.target.clone().filter(|_| !self.keys.is_empty()) else {
            return Some(release.html_url.clone());
        };
        if self.busy() {
            return None;
        }
        self.state = State::Downloading {
            release: release.clone(),
            progress: 0.0,
        };
        let (send, news) = channel();
        let keys = self.keys;
        let spawned = std::thread::Builder::new()
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
            });
        match spawned {
            Ok(_) => self.news = Some(news),
            Err(e) => self.state = State::Failed(format!("The update failed: {e}")),
        }
        None
    }

    /// Hides the update until the program looks again.
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
                    log::info!("{} {} is available", self.product.name(), release.version());
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

    /// Starts the installed program once this one has ended. Returns false if it could not.
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
}

/// Whether it is time to look for an update on the program's own, last done at `last`
/// (seconds since 1970).
pub fn due(last: Option<u64>, now: SystemTime) -> bool {
    let now = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    last.is_none_or(|last| now.saturating_sub(last) >= CHECK_INTERVAL.as_secs())
}

#[cfg(test)]
mod tests;
