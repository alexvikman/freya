//! The server's icon: in the menu bar on macOS, in the notification area on Windows. It shows
//! that the server runs, and its version, with Freya to open where it is installed, a code for
//! a new device, starting at login, an update when one is available, the log, and stopping.
//! The menu runs on the main thread; the server's work goes on the others.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod windows;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use freya_protocol::ServerControl;

use crate::Service;
use crate::accounts::Accounts;
use crate::updates::Updates;

/// What the menu's items do and show.
pub struct Actions {
    /// Stops the server, as Ctrl-C does.
    pub stop: Sender<()>,
    pub log: PathBuf,
    pub service: Arc<Service>,
    pub accounts: Arc<Accounts>,
    pub updates: Arc<Updates>,
}

impl Actions {
    fn starts_at_login(&self) -> bool {
        self.service.settings().is_ok_and(|s| s.start_at_login)
    }

    fn toggle_start_at_login(&self) {
        let Ok(mut settings) = self.service.settings() else {
            return;
        };
        settings.start_at_login = !settings.start_at_login;
        if let Err(e) = self.service.set_settings(&settings) {
            log::warn!("could not change how the server starts: {e}");
        }
    }

    /// A new connection code, for a device to type, as the menu shows it.
    fn connection_code(&self) -> String {
        let (code, lasts) = self.accounts.connection_code();
        format!("Code {code}, for {} minutes", lasts.as_secs().div_ceil(60))
    }

    /// Installs the update offered, or opens its page where the server cannot install itself.
    fn update(&self) {
        if let Some(page) = self.updates.install_or_page()
            && page.starts_with("https://")
        {
            open(page.as_ref());
        }
    }

    fn show_log(&self) {
        open(self.log.as_os_str());
    }

    fn stop(&self) {
        let _ = self.stop.send(());
    }
}

/// Shows the icon and runs its menu on this thread, the main one, for as long as the server
/// runs; none when this is not the main thread, and the server goes on without it.
pub fn run(actions: Actions) -> Option<()> {
    #[cfg(target_os = "macos")]
    return mac::run(actions);
    #[cfg(windows)]
    return windows::run(actions);
}

/// Hands `what`, a page or a file, to the system to open.
fn open(what: &std::ffi::OsStr) {
    #[cfg(windows)]
    let opened = shell_open(what);
    #[cfg(not(windows))]
    let opened = std::process::Command::new("open")
        .arg(what)
        .spawn()
        .map(drop);
    if let Err(e) = opened {
        log::warn!("could not open {}: {e}", what.to_string_lossy());
    }
}

/// Opens `what` as a click on it in Windows would, with ShellExecuteW: an address in the
/// browser, a file with its program. Not through explorer.exe, which reads its argument for
/// switches and commas of its own, and opened the Documents folder for an address with a query.
#[cfg(windows)]
#[allow(unsafe_code, reason = "asks Windows to open a page or a file")]
fn shell_open(what: &std::ffi::OsStr) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = what.encode_wide().chain([0]).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain([0]).collect();
    // SAFETY: valid wide strings that outlive the call; no window, folder or parameters.
    let done = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        )
    };
    // Above 32 when it worked, as ShellExecuteW says.
    if done as usize > 32 {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "ShellExecuteW said {}",
            done as usize
        )))
    }
}
