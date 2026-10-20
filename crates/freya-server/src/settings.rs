//! The server's settings, kept as TOML.

use std::path::Path;

use freya_protocol::ServerSettings;

/// The settings in `file`, and whether they were damaged: then the file could not be read as a
/// whole, a copy of it is kept beside it, and its folder list may be missing some, so the
/// library is not brought in line with it. None there yet is no damage.
pub fn load(file: &Path) -> (ServerSettings, bool) {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (ServerSettings::default(), false);
        }
        Err(e) => {
            log::warn!("could not read {}: {e}", file.display());
            return (ServerSettings::default(), true);
        }
    };
    match toml::from_str(&text) {
        Ok(settings) => (settings, false),
        Err(e) => {
            let copy = file.with_extension("invalid.toml");
            log::warn!(
                "{} could not be read ({e}); a copy is kept as {}",
                file.display(),
                copy.display()
            );
            let _ = std::fs::write(copy, &text);
            (ServerSettings::default(), true)
        }
    }
}

/// Keeps `settings` in `file`, through a new file renamed over it, so a crash leaves the old
/// settings or the new ones, never half of them.
pub fn save(file: &Path, settings: &ServerSettings) -> std::io::Result<()> {
    let text = toml::to_string_pretty(settings).map_err(std::io::Error::other)?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = file.with_extension("toml.partial");
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_kept_and_damage_is_told() {
        let dir =
            std::env::temp_dir().join(format!("freya-server-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("settings.toml");
        assert_eq!(load(&file), (ServerSettings::default(), false));
        let settings = ServerSettings {
            library_folders: vec!["/music".into()],
            scan_every_hours: 6,
            fanart_key: Some("key".into()),
            ..ServerSettings::default()
        };
        save(&file, &settings).unwrap();
        assert_eq!(load(&file), (settings, false));
        std::fs::write(&file, "library_folders = 7").unwrap();
        assert_eq!(load(&file), (ServerSettings::default(), true));
        assert!(dir.join("settings.invalid.toml").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
