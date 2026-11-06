//! Updating Freya Server: it looks for a newer release of itself once a day, and installs it
//! only when an admin says so, from Freya's Server tab or the server's own menu. Installed, the
//! server stops and starts again as the new version.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use freya_protocol::ServerUpdate;
use freya_update::{Product, State, Updater};

/// How long after starting the server first looks: not while it opens the library.
const FIRST_CHECK: Duration = Duration::from_secs(60);
/// How often the thread takes in what the updater found.
const POLL: Duration = Duration::from_secs(1);

/// The exit code that has launchd or systemd start the server again, as the new version: they
/// start it again only when it fails.
pub const RESTART_EXIT_CODE: i32 = 75;

/// What the server ends with: 0, or [`RESTART_EXIT_CODE`] after an update.
pub static EXIT_CODE: AtomicI32 = AtomicI32::new(0);

/// The server's updater, shared by the API, the menu and its own thread.
pub struct Updates {
    updater: Mutex<Updater>,
}

impl Updates {
    /// Starts looking for updates; `stop` stops the server once one is installed.
    pub fn start(stop: Sender<()>) -> Arc<Self> {
        let updates = Arc::new(Self {
            updater: Mutex::new(Updater::new(Product::Server, env!("CARGO_PKG_VERSION"))),
        });
        let follower = Arc::clone(&updates);
        let _ = std::thread::Builder::new()
            .name("freya-updates".into())
            .spawn(move || follower.follow(&stop));
        updates
    }

    fn updater(&self) -> std::sync::MutexGuard<'_, Updater> {
        self.updater.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Looks once a day, takes in what was found, and restarts once an update is installed.
    fn follow(&self, stop: &Sender<()>) {
        let mut next = Instant::now() + FIRST_CHECK;
        loop {
            std::thread::sleep(POLL);
            let mut updater = self.updater();
            updater.poll();
            if Instant::now() >= next {
                next = Instant::now() + freya_update::CHECK_INTERVAL;
                if matches!(updater.state(), State::Idle | State::UpToDate) {
                    updater.check(false, None);
                }
            }
            if let State::Installed(release) = updater.state() {
                log::info!(
                    "Freya Server {} is installed; restarting",
                    release.version()
                );
                // Under launchd or systemd, they start it again; otherwise it starts the new
                // one itself, once this one has ended.
                if supervised() || updater.restart() {
                    if supervised() {
                        EXIT_CODE.store(RESTART_EXIT_CODE, Ordering::Relaxed);
                    }
                    let _ = stop.send(());
                    return;
                }
            }
        }
    }

    /// The menu's line for an update, when one is available: "Update to 0.4.1…".
    pub fn offered(&self) -> Option<String> {
        match self.updater().state() {
            State::Available(release) => Some(format!("Update to {}…", release.version())),
            State::Downloading { release, .. } => {
                Some(format!("Updating to {}…", release.version()))
            }
            _ => None,
        }
    }

    /// Installs the available update; the release's page where the server cannot install
    /// itself, for the menu to open.
    pub fn install_or_page(&self) -> Option<String> {
        self.updater().install()
    }
}

impl crate::serve::Updating for Updates {
    fn status(&self) -> ServerUpdate {
        let updater = self.updater();
        let (state, release, progress, error) = match updater.state() {
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
        ServerUpdate {
            state: state.into(),
            current: updater.current().into(),
            version: release.map(|r| r.version().to_owned()).unwrap_or_default(),
            notes: release
                .and_then(|r| r.body.as_deref())
                .map(freya_update::plain_notes)
                .unwrap_or_default(),
            progress,
            error: error.into(),
            can_install: updater.can_install(),
            page: release.map(|r| r.html_url.clone()).unwrap_or_default(),
        }
    }

    fn check(&self) {
        self.updater().check(true, None);
    }

    fn install(&self) {
        // Where the server cannot install itself, the admin is shown the page instead.
        let _ = self.updater().install();
    }
}

/// Whether launchd or systemd started the server, as what starts it at login, and starts it
/// again when it fails.
fn supervised() -> bool {
    #[cfg(target_os = "macos")]
    let launchd =
        std::env::var_os("XPC_SERVICE_NAME").is_some_and(|name| name == crate::autostart::LABEL);
    #[cfg(not(target_os = "macos"))]
    let launchd = false;
    launchd || std::env::var_os("INVOCATION_ID").is_some()
}

/// Whether the server is to start again once it has stopped: a backup is to be restored.
pub static START_AGAIN: AtomicBool = AtomicBool::new(false);

/// Once the server has stopped: when it is to start again, under launchd or systemd by
/// ending with [`RESTART_EXIT_CODE`], otherwise by starting itself, told to wait for this one
/// to end (`--after`).
pub fn after_stop() {
    if !START_AGAIN.load(Ordering::Relaxed) {
        return;
    }
    if supervised() {
        EXIT_CODE.store(RESTART_EXIT_CODE, Ordering::Relaxed);
        return;
    }
    let Ok(program) = std::env::current_exe() else {
        log::warn!("could not start again: the server's program is unknown");
        return;
    };
    // As it was started, but for an earlier `--after`.
    let mut args = Vec::new();
    let mut given = std::env::args().skip(1);
    while let Some(arg) = given.next() {
        if arg == "--after" {
            given.next();
        } else {
            args.push(arg);
        }
    }
    let started = std::process::Command::new(program)
        .args(args)
        .arg("--after")
        .arg(std::process::id().to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Err(e) = started {
        log::warn!("could not start again: {e}");
    }
}

/// Waits, at most a minute, for the server that started this one (`--after`) to end.
pub fn wait_for(process: u32) {
    let until = Instant::now() + Duration::from_secs(60);
    while running(process) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(unix)]
#[allow(unsafe_code, reason = "asks the system whether a process runs")]
fn running(process: u32) -> bool {
    let Ok(process) = libc::pid_t::try_from(process) else {
        return false;
    };
    // SAFETY: signal 0 only asks whether the process is there.
    unsafe { libc::kill(process, 0) == 0 }
}

#[cfg(windows)]
#[allow(unsafe_code, reason = "asks the system whether a process runs")]
fn running(process: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    // SAFETY: a handle opened here, waited on briefly, and closed.
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, process);
        if handle.is_null() {
            return false;
        }
        let waited = WaitForSingleObject(handle, 0);
        CloseHandle(handle);
        waited == WAIT_TIMEOUT
    }
}

#[cfg(not(any(unix, windows)))]
fn running(_process: u32) -> bool {
    false
}
