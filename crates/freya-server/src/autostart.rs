//! Starting the server when the user logs in, so it runs without any client open: a
//! LaunchAgent on macOS, a systemd user service on Linux, and the Run key on Windows. Each is
//! the user's own and starts the server as them, with their music folders and drives. It is
//! started again only if it fails: a server that finds another one on its port, or was told
//! to stop, ends for good.

use std::path::{Path, PathBuf};

/// What starts the server: a program, its arguments, and the home folder it keeps its data
/// under: the one of the Freya that asked for it, as what starts it at login may not know it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub home: Option<PathBuf>,
}

impl Launch {
    /// The server `server` beside Freya: run as it is, or from an AppImage, through
    /// `freya server`, so the image stays mounted while the server runs.
    pub fn of(server: &Path) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        match std::env::var_os("APPIMAGE") {
            Some(image) => Self {
                program: PathBuf::from(image),
                args: vec!["server".into()],
                home,
            },
            None => Self {
                program: server.to_owned(),
                args: Vec::new(),
                home,
            },
        }
    }
}

/// What the server is called where the system keeps what starts at login.
#[cfg(target_os = "macos")]
const LABEL: &str = "io.github.alexvikman.freya.server";

/// Whether the server starts when the user logs in.
pub fn is_installed() -> bool {
    platform::file().is_some_and(|file| file.exists()) || platform::registered()
}

/// Makes the server start, as `launch` says, when the user logs in; and now, if it does not
/// run yet.
pub fn install(launch: &Launch) -> Result<(), String> {
    platform::install(launch)
}

/// The server no longer starts when the user logs in; one running now is stopped.
pub fn uninstall() -> Result<(), String> {
    platform::uninstall()
}

/// Runs `program` with `args`; its error output when it fails.
#[cfg_attr(windows, allow(dead_code))]
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Writes `text` to `file`, unless it holds it already; whether it changed.
#[cfg_attr(windows, allow(dead_code))]
fn write_if_changed(file: &Path, text: &str) -> Result<bool, String> {
    if std::fs::read_to_string(file).is_ok_and(|kept| kept == text) {
        return Ok(false);
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(file, text).map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::PathBuf;

    use super::{LABEL, Launch, run, write_if_changed};

    pub fn file() -> Option<PathBuf> {
        let home = directories::BaseDirs::new()?.home_dir().to_owned();
        Some(home.join(format!("Library/LaunchAgents/{LABEL}.plist")))
    }

    pub fn registered() -> bool {
        false
    }

    /// The user's launchd domain.
    fn domain() -> Result<String, String> {
        let out = std::process::Command::new("id")
            .arg("-u")
            .output()
            .map_err(|e| e.to_string())?;
        Ok(format!(
            "gui/{}",
            String::from_utf8_lossy(&out.stdout).trim()
        ))
    }

    pub fn install(launch: &Launch) -> Result<(), String> {
        let file = file().ok_or("no home folder")?;
        if !write_if_changed(&file, &plist(launch))? {
            return Ok(());
        }
        let domain = domain()?;
        // Loaded anew, as launchd reads the file when it loads it.
        let _ = run("launchctl", &["bootout", &format!("{domain}/{LABEL}")]);
        run(
            "launchctl",
            &["bootstrap", &domain, &file.to_string_lossy()],
        )
        .map_err(|e| format!("launchd did not take the server: {e}"))
    }

    pub fn uninstall() -> Result<(), String> {
        let file = file().ok_or("no home folder")?;
        if !file.exists() {
            return Ok(());
        }
        let _ = run("launchctl", &["bootout", &format!("{}/{LABEL}", domain()?)]);
        std::fs::remove_file(&file).map_err(|e| e.to_string())
    }

    /// The LaunchAgent: started at login, and again only when it fails.
    pub fn plist(launch: &Launch) -> String {
        let escape = |text: &str| {
            text.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        };
        let arguments: String = std::iter::once(launch.program.to_string_lossy().into_owned())
            .chain(launch.args.iter().cloned())
            .map(|arg| format!("        <string>{}</string>\n", escape(&arg)))
            .collect();
        let environment = match &launch.home {
            Some(home) => format!(
                "    <key>EnvironmentVariables</key>\n    <dict>\n        <key>HOME</key>\n        <string>{}</string>\n    </dict>\n",
                escape(&home.to_string_lossy())
            ),
            None => String::new(),
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{arguments}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ProcessType</key>
    <string>Interactive</string>
{environment}</dict>
</plist>
"#
        )
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use std::path::PathBuf;

    use super::{Launch, run, write_if_changed};

    const UNIT: &str = "freya-server.service";

    pub fn file() -> Option<PathBuf> {
        Some(
            directories::BaseDirs::new()?
                .config_dir()
                .join("systemd/user")
                .join(UNIT),
        )
    }

    pub fn registered() -> bool {
        false
    }

    pub fn install(launch: &Launch) -> Result<(), String> {
        let file = file().ok_or("no settings folder")?;
        if !write_if_changed(&file, &unit(launch))? {
            return Ok(());
        }
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", "--now", UNIT])
            .map_err(|e| format!("systemd did not take the server: {e}"))
    }

    pub fn uninstall() -> Result<(), String> {
        let file = file().ok_or("no settings folder")?;
        if !file.exists() {
            return Ok(());
        }
        let _ = run("systemctl", &["--user", "disable", "--now", UNIT]);
        std::fs::remove_file(&file).map_err(|e| e.to_string())?;
        run("systemctl", &["--user", "daemon-reload"])
    }

    /// The unit: started at login, and again only when it fails.
    pub fn unit(launch: &Launch) -> String {
        // Quoted as systemd reads ExecStart: within double quotes, \ and " escaped.
        let quote = |arg: &str| format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""));
        let command: Vec<String> = std::iter::once(launch.program.to_string_lossy().into_owned())
            .chain(launch.args.iter().cloned())
            .map(|arg| quote(&arg))
            .collect();
        let environment = launch
            .home
            .as_ref()
            .map(|home| {
                format!(
                    "Environment={}\n",
                    quote(&format!("HOME={}", home.display()))
                )
            })
            .unwrap_or_default();
        format!(
            "[Unit]\nDescription=Freya's music library\n\n[Service]\nExecStart={}\n{environment}Restart=on-failure\n\n[Install]\nWantedBy=default.target\n",
            command.join(" ")
        )
    }
}

#[cfg(windows)]
mod platform {
    use std::path::PathBuf;

    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };

    use super::Launch;

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "Freya Server";

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn file() -> Option<PathBuf> {
        None
    }

    #[allow(unsafe_code, reason = "reads a value of the user's Run key")]
    pub fn registered() -> bool {
        let (key, name) = (wide(RUN), wide(NAME));
        // SAFETY: NUL-terminated names; no data is asked for, only whether the value is there.
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ) == 0
        }
    }

    #[allow(unsafe_code, reason = "writes a value of the user's Run key")]
    pub fn install(launch: &Launch) -> Result<(), String> {
        let command: Vec<String> = std::iter::once(launch.program.to_string_lossy().into_owned())
            .chain(launch.args.iter().cloned())
            .map(|arg| format!("\"{arg}\""))
            .collect();
        let (key, name, data) = (wide(RUN), wide(NAME), wide(&command.join(" ")));
        // SAFETY: NUL-terminated names, and the data with its length in bytes, alive for the
        // call.
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        };
        if status != 0 {
            return Err(format!("Windows did not take the server ({status})"));
        }
        // Started now too, as Windows does only at the next login.
        let mut start = std::process::Command::new(&launch.program);
        start.args(&launch.args);
        let _ = start.spawn();
        Ok(())
    }

    #[allow(unsafe_code, reason = "deletes a value of the user's Run key")]
    pub fn uninstall() -> Result<(), String> {
        let (key, name) = (wide(RUN), wide(NAME));
        // SAFETY: NUL-terminated names.
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_appimage_starts_the_server_through_freya() {
        // Only this test reads APPIMAGE, set as an AppImage sets it.
        let server = Path::new("/opt/freya/freya-server");
        if std::env::var_os("APPIMAGE").is_none() {
            assert_eq!(Launch::of(server).program, server);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_launch_agent_starts_the_server_at_login_and_after_a_failure() {
        let plist = platform::plist(&Launch {
            program: "/Applications/Freya & Co.app/Contents/MacOS/freya-server".into(),
            args: Vec::new(),
            home: Some("/Users/anna".into()),
        });
        assert!(plist.contains(
            "<string>/Applications/Freya &amp; Co.app/Contents/MacOS/freya-server</string>"
        ));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(plist.contains("<key>SuccessfulExit</key>\n        <false/>"));
        assert!(plist.contains("<key>HOME</key>\n        <string>/Users/anna</string>"));
        // As launchd reads it.
        let file = std::env::temp_dir().join(format!("freya-agent-{}.plist", std::process::id()));
        std::fs::write(&file, &plist).unwrap();
        let lint = std::process::Command::new("plutil")
            .arg("-lint")
            .arg(&file)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&file);
        assert!(
            lint.status.success(),
            "{}",
            String::from_utf8_lossy(&lint.stdout)
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn the_unit_quotes_the_command() {
        let unit = platform::unit(&Launch {
            program: "/home/a/Freya \"x\".AppImage".into(),
            args: vec!["server".into()],
            home: Some("/home/a".into()),
        });
        assert!(unit.contains(r#"ExecStart="/home/a/Freya \"x\".AppImage" "server""#));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains(r#"Environment="HOME=/home/a""#));
    }
}
