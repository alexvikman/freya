//! Internet radio stations, which the server keeps for its users (see
//! `freya_server::stations`).

use std::sync::Arc;

use freya_protocol::{ApiResult, RadioStation, Server};
use slint_interpreter::{Struct, Value};

use super::skin_state::string;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Station {
    pub name: String,
    pub url: String,
}

pub struct Stations {
    /// The server that keeps them; without one, they are kept only while Freya runs.
    server: Option<Arc<dyn Server>>,
    list: Vec<Station>,
}

impl Stations {
    /// None yet: they come from the server once connected.
    pub fn new() -> Self {
        Self {
            server: None,
            list: Vec::new(),
        }
    }

    /// Keeps the stations of `server`, which are `saved`, as it said when connected to.
    pub fn connect(&mut self, server: Arc<dyn Server>, saved: ApiResult<Vec<RadioStation>>) {
        self.server = Some(server);
        self.keep(saved);
    }

    /// Reads the stations from the server again: they changed.
    pub fn reload(&mut self) {
        let Some(server) = &self.server else {
            return;
        };
        let saved = server.stations();
        self.keep(saved);
    }

    fn keep(&mut self, saved: ApiResult<Vec<RadioStation>>) {
        match saved {
            Ok(saved) => {
                self.list = saved
                    .into_iter()
                    .map(|station| Station {
                        name: station.name,
                        url: station.url,
                    })
                    .collect();
            }
            Err(e) => log::warn!("could not read the radio stations: {e}"),
        }
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

    /// Tells the server the stations, the window not waiting for it: the list here is changed
    /// already, and the server is told in order.
    fn save(&self) {
        let Some(server) = self.server.clone() else {
            return;
        };
        let stations: Vec<RadioStation> = self
            .list
            .iter()
            .map(|station| RadioStation {
                name: station.name.clone(),
                url: station.url.clone(),
            })
            .collect();
        crate::errands::run(move || {
            if let Err(e) = server.save_stations(&stations) {
                log::warn!("could not save the radio stations: {e}");
            }
        });
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

    #[test]
    fn adds_stations_by_address_and_shows_which_plays() {
        let mut stations = Stations::new();
        stations.add("", "radio.example.com:8000/live").unwrap();
        stations
            .add("Night Jazz", "https://jazz.example.org/")
            .unwrap();
        assert_eq!(
            stations.add("Nothing", "  ").unwrap_err(),
            "No address given"
        );
        let added = &stations.list[0];
        assert_eq!(added.name, "radio.example.com:8000");
        assert_eq!(added.url, "http://radio.example.com:8000/live");

        let rows = stations.rows(Some("https://jazz.example.org/"));
        let Some(Value::Struct(last)) = rows.last() else {
            panic!("a row per station");
        };
        assert_eq!(last.get_field("address"), Some(&string("jazz.example.org")));
        assert_eq!(last.get_field("playing"), Some(&Value::Bool(true)));
        assert_eq!(host("https://somafm.com/groovesalad.pls"), "somafm.com");

        stations.remove(0);
        assert_eq!(stations.list.len(), 1);
    }
}
