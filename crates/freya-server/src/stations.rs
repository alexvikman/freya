//! The internet radio stations the server keeps for its users. They are kept as an M3U
//! playlist of stream addresses with the stations' names, so other players can read the file
//! too.

use std::path::PathBuf;
use std::sync::Mutex;

use freya_media::{Entry, playlist_file};
use freya_protocol::RadioStation;

/// The stations a new server starts with: long-running, listener-supported stations.
const STARTERS: [(&str, &str); 5] = [
    ("SomaFM: Groove Salad", "https://somafm.com/groovesalad.pls"),
    ("SomaFM: Drone Zone", "https://somafm.com/dronezone.pls"),
    (
        "Radio Paradise",
        "https://www.radioparadise.com/m3u/mp3-128.m3u",
    ),
    ("FIP", "https://icecast.radiofrance.fr/fip-midfi.mp3"),
    ("KEXP", "https://kexp.streamguys1.com/kexp160.aac"),
];

/// The stations in a file.
pub struct Stations {
    file: PathBuf,
    /// Held while the file is written, so two clients' changes are not interleaved.
    writing: Mutex<()>,
}

impl Stations {
    pub fn new(file: PathBuf) -> Self {
        Self {
            file,
            writing: Mutex::new(()),
        }
    }

    /// The stations saved, or the starter stations when none are saved yet.
    pub fn all(&self) -> Vec<RadioStation> {
        if !self.file.exists() {
            return STARTERS
                .iter()
                .map(|(name, url)| RadioStation {
                    name: (*name).to_owned(),
                    url: (*url).to_owned(),
                })
                .collect();
        }
        match playlist_file::load(&self.file) {
            Ok(entries) => entries
                .into_iter()
                .map(|entry| {
                    let url = entry.path.to_string_lossy().into_owned();
                    let name = entry.title.unwrap_or_else(|| url.clone());
                    RadioStation { name, url }
                })
                .collect(),
            Err(e) => {
                log::warn!(
                    "could not read the radio stations from {}: {e}",
                    self.file.display()
                );
                Vec::new()
            }
        }
    }

    /// Keeps `stations` in place of those saved.
    pub fn save(&self, stations: &[RadioStation]) -> Result<(), String> {
        for station in stations {
            check(station)?;
        }
        let entries: Vec<Entry> = stations
            .iter()
            .map(|station| {
                let mut entry = Entry::new(&station.url);
                entry.title = Some(station.name.clone());
                entry
            })
            .collect();
        let _writing = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        playlist_file::save(&self.file, &entries).map_err(|e| {
            log::warn!(
                "could not save the radio stations to {}: {e}",
                self.file.display()
            );
            "could not save the radio stations".to_owned()
        })
    }
}

/// Whether `station` can be kept and played: a web address, and a name and address that are
/// one line each, as the file holds them.
fn check(station: &RadioStation) -> Result<(), String> {
    let lower = station
        .url
        .get(..8)
        .unwrap_or(&station.url)
        .to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err(format!("not a web address: {}", station.url));
    }
    if station.name.trim().is_empty() {
        return Err("a station needs a name".to_owned());
    }
    if [&station.name, &station.url]
        .iter()
        .any(|text| text.chars().any(char::is_control))
    {
        return Err("a station's name and address are one line each".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station(name: &str, url: &str) -> RadioStation {
        RadioStation {
            name: name.to_owned(),
            url: url.to_owned(),
        }
    }

    #[test]
    fn starts_with_starter_stations_and_keeps_changes() {
        let dir = std::env::temp_dir().join(format!("freya-stations-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let stations = Stations::new(dir.join("stations.m3u8"));
        let mut all = stations.all();
        assert_eq!(all.len(), STARTERS.len());

        all.remove(0);
        all.push(station("Night Jazz", "https://jazz.example.org/"));
        stations.save(&all).unwrap();
        assert_eq!(Stations::new(dir.join("stations.m3u8")).all(), all);

        // None kept is none, not the starters again.
        stations.save(&[]).unwrap();
        assert_eq!(stations.all(), Vec::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_only_web_addresses_of_one_line() {
        let stations = Stations::new(std::env::temp_dir().join("freya-stations-never"));
        for bad in [
            station("Local", "file:///etc/passwd"),
            station("", "https://jazz.example.org/"),
            station("Two\n#EXTINF:-1,lines", "https://jazz.example.org/"),
            station(
                "Jazz",
                "https://jazz.example.org/\nhttps://other.example.org/",
            ),
        ] {
            assert!(
                stations.save(std::slice::from_ref(&bad)).is_err(),
                "{bad:?}"
            );
        }
    }
}
