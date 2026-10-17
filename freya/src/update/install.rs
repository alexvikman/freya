//! Puts a downloaded release in place of the running Freya, and starts the new one once this
//! one has quit.
//!
//! Freya replaces itself where that is one file or folder it can write: the app bundle on
//! macOS and the AppImage on Linux. A `.deb` belongs to the package manager and the Windows
//! installer to Windows, so those are only told about an update.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// What this Freya can replace itself as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// An app bundle, e.g. `/Applications/Freya.app`.
    MacApp(PathBuf),
    /// An AppImage file.
    AppImage(PathBuf),
}

impl Target {
    /// The running Freya's, if it can replace itself.
    pub fn current() -> Option<Self> {
        if cfg!(target_os = "macos") {
            let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
            // …/Freya.app/Contents/MacOS/freya
            let app = exe.parent()?.parent()?.parent()?;
            let bundled = app.extension().is_some_and(|ext| ext == "app")
                && exe.parent()?.ends_with("Contents/MacOS");
            return bundled.then(|| Self::MacApp(app.to_owned()));
        }
        if cfg!(target_os = "linux") {
            // Set by the AppImage runtime to the image's own path.
            let image = std::env::var_os("APPIMAGE")?;
            return Some(Self::AppImage(PathBuf::from(image)));
        }
        None
    }

    /// The release file this target is replaced with.
    pub fn asset(&self, version: &str) -> String {
        let arch = std::env::consts::ARCH;
        match self {
            Self::MacApp(_) => format!("Freya-{version}-macos-{arch}.app.zip"),
            Self::AppImage(_) => format!("Freya-{version}-linux-{arch}.AppImage"),
        }
    }

    fn path(&self) -> &Path {
        match self {
            Self::MacApp(path) | Self::AppImage(path) => path,
        }
    }

    /// A hidden name next to the target, e.g. `.Freya.app.old`.
    fn beside(&self, suffix: &str) -> Result<PathBuf> {
        let path = self.path();
        let name = path.file_name().context("the app has no file name")?;
        let parent = path.parent().context("the app is in no folder")?;
        Ok(parent.join(format!(".{}.{suffix}", name.to_string_lossy())))
    }

    /// Puts `package`, the downloaded release file, in place of the running Freya. The running
    /// Freya keeps running; the new one starts with `restart`.
    pub fn install(&self, package: &[u8]) -> Result<()> {
        match self {
            Self::MacApp(app) => self.install_app(app, package),
            Self::AppImage(image) => self.install_image(image, package),
        }
    }

    fn install_app(&self, app: &Path, zip: &[u8]) -> Result<()> {
        let staging = self.beside("update")?;
        let old = self.beside("old")?;
        let archive = self.beside("update.zip")?;
        remove(&staging)?;
        remove(&old)?;
        std::fs::write(&archive, zip)
            .with_context(|| format!("could not write next to {}", app.display()))?;
        let unpacked = run(Command::new("ditto")
            .arg("-x")
            .arg("-k")
            .arg(&archive)
            .arg(&staging));
        let _ = std::fs::remove_file(&archive);
        unpacked.context("could not unpack the update")?;

        let new = staging.join("Freya.app");
        // The signature seals the whole bundle: anything changed or missing shows here.
        run(Command::new("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&new))
        .context("the new app's signature does not hold")?;

        std::fs::rename(app, &old).context("could not move the running app aside")?;
        if let Err(e) = std::fs::rename(&new, app) {
            let _ = std::fs::rename(&old, app);
            return Err(e).context("could not move the new app into place");
        }
        let _ = std::fs::remove_dir_all(&staging);
        Ok(())
    }

    fn install_image(&self, image: &Path, bytes: &[u8]) -> Result<()> {
        let staging = self.beside("update")?;
        std::fs::write(&staging, bytes)
            .with_context(|| format!("could not write next to {}", image.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))?;
        }
        // A rename over the running image is atomic; the running Freya keeps its open copy.
        std::fs::rename(&staging, image).context("could not move the new AppImage into place")
    }

    /// Starts the installed Freya once this process has ended. The program is started directly,
    /// not through `open`, so it runs with this one's environment.
    pub fn restart(&self) -> Result<()> {
        let program = match self {
            Self::MacApp(app) => {
                let exe = std::env::current_exe()?;
                let name = exe.file_name().context("Freya's program has no name")?;
                app.join("Contents/MacOS").join(name)
            }
            Self::AppImage(image) => image.clone(),
        };
        let script = r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec "$2""#;
        Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .arg("sh")
            .arg(std::process::id().to_string())
            .arg(program)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("could not start the new Freya")?;
        Ok(())
    }

    /// Removes what an earlier update left beside the app.
    pub fn clean_up(&self) {
        for suffix in ["old", "update", "update.zip"] {
            if let Ok(path) = self.beside(suffix)
                && path.exists()
                && let Err(e) = remove(&path)
            {
                log::debug!("could not remove {}: {e}", path.display());
            }
        }
    }
}

fn remove(path: &Path) -> Result<()> {
    let result = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match result {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

fn run(command: &mut Command) -> Result<()> {
    let output = command.output()?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("freya-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A tiny signed app: a script as its executable, signed ad hoc as releases are.
    fn make_app(dir: &Path, marker: &str) -> PathBuf {
        let app = dir.join("Freya.app");
        let macos = app.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>\
             <key>CFBundleExecutable</key><string>freya</string>\
             <key>CFBundleIdentifier</key><string>io.github.alexvikman.freya.test</string>\
             </dict></plist>",
        )
        .unwrap();
        std::fs::write(macos.join("freya"), format!("#!/bin/sh\necho {marker}\n")).unwrap();
        run(Command::new("chmod").arg("+x").arg(macos.join("freya"))).unwrap();
        run(Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&app))
        .unwrap();
        app
    }

    fn zip(app: &Path, out: &Path) -> Vec<u8> {
        run(Command::new("ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(app)
            .arg(out))
        .unwrap();
        std::fs::read(out).unwrap()
    }

    #[test]
    fn swaps_the_app_and_refuses_a_broken_one() {
        let release = temp_dir("release");
        let new = make_app(&release, "new");
        let package = zip(&new, &release.join("new.zip"));

        let installed = temp_dir("installed");
        let app = make_app(&installed, "old");
        let target = Target::MacApp(app.clone());
        assert_eq!(
            target.asset("0.2.0"),
            format!("Freya-0.2.0-macos-{}.app.zip", std::env::consts::ARCH)
        );
        target.install(&package).unwrap();
        let script = std::fs::read_to_string(app.join("Contents/MacOS/freya")).unwrap();
        assert!(script.contains("new"), "the new app is in place");
        assert!(installed.join(".Freya.app.old").exists());
        target.clean_up();
        assert!(!installed.join(".Freya.app.old").exists());

        // Changed after signing: refused, and the installed app is left alone.
        std::fs::write(new.join("Contents/MacOS/freya"), "#!/bin/sh\necho evil\n").unwrap();
        let broken = zip(&new, &release.join("broken.zip"));
        assert!(target.install(&broken).is_err());
        let script = std::fs::read_to_string(app.join("Contents/MacOS/freya")).unwrap();
        assert!(script.contains("new"), "still the good app");
        assert!(
            !installed.join(".Freya.app.update").exists() || {
                target.clean_up();
                true
            }
        );

        std::fs::remove_dir_all(&release).unwrap();
        std::fs::remove_dir_all(&installed).unwrap();
    }
}
