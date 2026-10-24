//! Saved internet radio stations. They are kept as an M3U playlist of stream addresses with
//! the stations' names, so other players can read the file too.

use std::path::PathBuf;

use freya_media::{Entry, playlist_file};
use slint_interpreter::{Struct, Value};

use super::skin_state::string;

/// The stations a new user starts with: long-running, listener-supported stations.
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Station {
    pub name: String,
    pub url: String,
}

pub struct Stations {
    /// Where the stations are saved; `None` keeps them only while Freya runs.
    path: Option<PathBuf>,
    list: Vec<Station>,
}

impl Stations {
    /// The stations saved at `path`, or the starter stations when nothing is saved yet.
    pub fn load(path: Option<PathBuf>) -> Self {
        let saved = path.as_deref().filter(|p| p.exists()).map(|p| {
            playlist_file::load(p).unwrap_or_else(|e| {
                log::warn!(
                    "could not read the radio stations from {}: {e}",
                    p.display()
                );
                Vec::new()
            })
        });
        let list = match saved {
            Some(entries) => entries
                .into_iter()
                .map(|entry| {
                    let url = entry.path.to_string_lossy().into_owned();
                    let name = entry.title.unwrap_or_else(|| host(&url).to_owned());
                    Station { name, url }
                })
                .collect(),
            None => STARTERS
                .iter()
                .map(|(name, url)| Station {
                    name: (*name).to_owned(),
                    url: (*url).to_owned(),
                })
                .collect(),
        };
        Self { path, list }
    }

    pub fn get(&self, index: usize) -> Option<&Station> {
        self.list.get(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Station> {
        self.list.iter()
    }

    /// Saves a station at the end. `url` may leave out "http://"; without a name, the station
    /// is named after its address's host.
    pub fn add(&mut self, name: &str, url: &str) -> Result<(), String> {
        let url = stream_url(url)?;
        let name = match name.trim() {
            "" => host(&url).to_owned(),
            name => name.to_owned(),
        };
        self.list.push(Station { name, url });
        self.save();
        Ok(())
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.list.len() {
            self.list.remove(index);
            self.save();
        }
    }

    /// `StationRow`s for the skin. `playing` is the address of the station playing, if any.
    pub fn rows(&self, playing: Option<&str>) -> Vec<Value> {
        self.list
            .iter()
            .map(|station| {
                Value::Struct(Struct::from_iter([
                    ("name".to_owned(), string(&station.name)),
                    ("address".to_owned(), string(address(&station.url))),
                    (
                        "playing".to_owned(),
                        Value::Bool(playing == Some(station.url.as_str())),
                    ),
                ]))
            })
            .collect()
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let entries: Vec<Entry> = self
            .list
            .iter()
            .map(|station| {
                let mut entry = Entry::new(&station.url);
                entry.title = Some(station.name.clone());
                entry
            })
            .collect();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = playlist_file::save(path, &entries) {
            log::warn!(
                "could not save the radio stations to {}: {e}",
                path.display()
            );
        }
    }
}

/// A stream's address from what the user typed: "http://" is added when no scheme is given.
pub fn stream_url(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("No address given".to_owned());
    }
    let url = if text.contains("://") {
        text.to_owned()
    } else {
        format!("http://{text}")
    };
    if freya_audio::is_url(&url) {
        Ok(url)
    } else {
        Err(format!("Not a web address: {text}"))
    }
}

/// The address as shown: without "https://" and a trailing slash.
fn address(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.strip_suffix('/').unwrap_or(rest)
}

/// "somafm.com" for "https://somafm.com/groovesalad.pls".
fn host(url: &str) -> &str {
    address(url).split(['/', '?', '#']).next().unwrap_or(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("freya-radio-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("stations.m3u8")
    }

    fn names(stations: &Stations) -> Vec<&str> {
        stations.list.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn starts_with_starter_stations_and_keeps_changes() {
        let path = temp_file("keep");
        let mut stations = Stations::load(Some(path.clone()));
        assert_eq!(stations.list.len(), STARTERS.len());

        stations.remove(0);
        stations.add("", "radio.example.com:8000/live").unwrap();
        stations
            .add("Night Jazz", "https://jazz.example.org/")
            .unwrap();
        assert_eq!(
            stations.add("Nothing", "  ").unwrap_err(),
            "No address given"
        );

        let again = Stations::load(Some(path));
        assert_eq!(names(&again), names(&stations));
        assert_eq!(again.list.len(), STARTERS.len() + 1);
        let added = &again.list[STARTERS.len() - 1];
        assert_eq!(added.name, "radio.example.com:8000");
        assert_eq!(added.url, "http://radio.example.com:8000/live");
    }

    #[test]
    fn rows_show_addresses_and_which_station_plays() {
        let mut stations = Stations::load(None);
        stations
            .add("Night Jazz", "https://jazz.example.org/")
            .unwrap();
        let rows = stations.rows(Some("https://jazz.example.org/"));
        let Some(Value::Struct(last)) = rows.last() else {
            panic!("a row per station");
        };
        assert_eq!(last.get_field("address"), Some(&string("jazz.example.org")));
        assert_eq!(last.get_field("playing"), Some(&Value::Bool(true)));
        assert_eq!(host("https://somafm.com/groovesalad.pls"), "somafm.com");
    }
}
