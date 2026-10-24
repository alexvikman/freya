//! Updates of Freya itself, through `freya-update`, shown in the skin's `Updates` global; and
//! the pages Freya opens in the browser.

use slint_interpreter::Value;

pub use freya_update::{Release, State, due};

/// Freya's own updater: it finds Freya's releases, and installs them when the user says so.
pub struct Updater(freya_update::Updater);

impl Updater {
    pub fn new() -> Self {
        Self(freya_update::Updater::new(
            freya_update::Product::Freya,
            env!("CARGO_PKG_VERSION"),
        ))
    }

    pub fn state(&self) -> &State {
        self.0.state()
    }

    pub fn check(&mut self, asked: bool, skipped: Option<&str>) {
        self.0.check(asked, skipped);
    }

    /// Installs the available update, or opens its page where Freya cannot.
    pub fn install(&mut self) {
        if let Some(page) = self.0.install() {
            open_page(&page);
        }
    }

    pub fn later(&mut self) {
        self.0.later();
    }

    pub fn poll(&mut self) -> bool {
        self.0.poll()
    }

    pub fn restart(&mut self) -> bool {
        self.0.restart()
    }

    /// Values for the skin's `Updates` global, except `check-automatically`.
    pub fn values(&self) -> Vec<(&'static str, Value)> {
        let text = |s: &str| Value::String(s.into());
        let (state, release, progress, error) = match self.0.state() {
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
            ("current-version", text(self.0.current())),
            ("version", text(release.map_or("", Release::version))),
            (
                "notes",
                text(
                    &release
                        .and_then(|r| r.body.as_deref())
                        .map(freya_update::plain_notes)
                        .unwrap_or_default(),
                ),
            ),
            ("progress", Value::Number(f64::from(progress))),
            ("error", text(error)),
            ("can-install", Value::Bool(self.0.can_install())),
        ]
    }
}

/// The address of one of Freya's own pages, by the name skins use (`App.open-link`).
pub fn page(name: &str) -> Option<String> {
    let base = format!("https://github.com/{}", freya_update::REPOSITORY);
    Some(match name {
        "github" => base,
        "releases" => format!("{base}/releases"),
        // Where Freya Server is downloaded: the latest release carries it for every system.
        "server" => format!("{base}/releases/latest"),
        "issues" => format!("{base}/issues"),
        "license" => format!("{base}/blob/main/README.md#license"),
        // Where users get the personal key Freya asks fanart.tv with.
        "fanart-key" => "https://fanart.tv/get-an-api-key/".to_owned(),
        _ => return None,
    })
}

/// Opens a web page in the user's browser: one on the internet over HTTPS, or Freya's own
/// pages on this computer. Only plain URL characters are passed to the system's opener, so a
/// URL that came from elsewhere cannot be read by it as anything else.
pub fn open_page(url: &str) {
    if !is_openable(url) {
        log::warn!("did not open a page with an unexpected address");
        return;
    }
    open_with_system(std::ffi::OsStr::new(url));
}

/// Opens a page of Freya's own whose address holds a secret, the login's state or the start of
/// the password page: through a file only the user can read, which sends the browser on, so
/// the address is in no program's command line, where other users of the computer could read
/// it. The file goes again a minute later.
pub fn open_page_privately(url: &str) {
    if !is_openable(url) {
        log::warn!("did not open a page with an unexpected address");
        return;
    }
    let Some(dir) = crate::settings::cache_dir() else {
        log::warn!("no folder to open a page through");
        return;
    };
    let file = dir.join("open-page.html");
    let address = url.replace('&', "&amp;");
    let page = format!(
        "<!doctype html><meta charset=\"utf-8\"><meta http-equiv=\"refresh\" content=\"0;url={address}\"><title>Freya</title><a href=\"{address}\">Continue</a>\n"
    );
    if let Err(e) = write_private(&file, page.as_bytes()) {
        log::warn!("could not open a page: {e}");
        return;
    }
    open_with_system(file.as_os_str());
    let _ = std::thread::Builder::new()
        .name("freya-opened-page".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(60));
            let _ = std::fs::remove_file(file);
        });
}

/// Hands `what`, a page's address or a file, to the system to open.
/// The licenses of the crates and libraries built into Freya and its server, as cargo-about
/// lists them (see docs/DEPENDENCIES.md): their notices go with the programs.
const THIRD_PARTY: &str = include_str!("../../../licenses/THIRD-PARTY.html");

/// Opens the third-party licenses in the browser, from a file Freya writes out of what is built
/// into it.
pub fn open_licenses() {
    let Some(dir) = crate::settings::cache_dir() else {
        log::warn!("no folder to open the licenses from");
        return;
    };
    let file = dir.join("third-party-licenses.html");
    if std::fs::read(&file).ok().as_deref() != Some(THIRD_PARTY.as_bytes())
        && let Err(e) = write_private(&file, THIRD_PARTY.as_bytes())
    {
        log::warn!("could not write the licenses: {e}");
        return;
    }
    open_with_system(file.as_os_str());
}

fn open_with_system(what: &std::ffi::OsStr) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(opener).arg(what).spawn() {
        log::warn!("could not open a page: {e}");
    }
}

/// Writes `bytes` to `file`, a new file the user alone can read.
fn write_private(file: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::remove_file(file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(file)?.write_all(bytes)
}

/// Whether `url` may be opened: see [`open_page`].
fn is_openable(url: &str) -> bool {
    let ours = format!("http://127.0.0.1:{}/", freya_protocol::CALLBACK_PORT);
    let plain = url
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-._~:/?#&=%+@".contains(&b));
    plain && (url.starts_with("https://") || url.starts_with(&ours))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_only_plain_https_pages_and_freyas_own() {
        assert!(is_openable(
            "https://www.last.fm/api/auth/?api_key=k&token=t"
        ));
        assert!(is_openable(
            "http://127.0.0.1:47477/web/password?start=ab12"
        ));
        assert!(!is_openable("http://example.com/"));
        assert!(!is_openable("http://127.0.0.1:5555/"));
        assert!(!is_openable("file:///etc/passwd"));
        assert!(!is_openable("https://example.com/a b"));
        assert!(!is_openable("https://example.com/a,b"));
        assert!(!is_openable("https://example.com/\"--args"));
    }

    #[test]
    fn the_licenses_built_in_name_what_freya_is_made_with() {
        assert!(THIRD_PARTY.starts_with("<!doctype html>"));
        // Slint under the license Freya uses it with, and libopus' copyright.
        assert!(THIRD_PARTY.contains("LicenseRef-Slint-Royalty-free-2.0"));
        assert!(THIRD_PARTY.contains("Xiph.Org"));
        for krate in ["symphonia", "rustls", "rusqlite", "ureq"] {
            assert!(THIRD_PARTY.contains(&format!(">{krate} ")), "{krate}");
        }
        // Freya's own crates are not third parties.
        assert!(!THIRD_PARTY.contains(">freya-server "));
    }
}
