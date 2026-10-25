//! The stations Freya picked (in `featured.toml`), first in the directory and on its first page
//! whether Radio Browser is asked or not, and the rows of stations that page shows.

use freya_protocol::DirectoryStation;
use serde::Deserialize;

use super::tags::{HIGH_BITRATE, Tags};

const FEATURED: &str = include_str!("featured.toml");

#[derive(Deserialize)]
struct File {
    shelves: Vec<Shelf>,
    stations: Vec<Featured>,
}

/// A row of stations on the directory's first page.
#[derive(Deserialize, Clone, Debug)]
pub struct Shelf {
    pub title: String,
    /// The tags its stations have, all of them; the best known of the directory's are shown.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Or the stations Freya picked that it shows, by their ids.
    #[serde(default)]
    pub stations: Vec<String>,
}

/// A station Freya picked.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Featured {
    /// Which it is, after "freya:".
    pub id: String,
    pub name: String,
    pub url: String,
    pub homepage: Option<String>,
    pub logo: Option<String>,
    pub codec: String,
    #[serde(default)]
    pub bitrate: u32,
    /// Its ISO 3166 code: "it".
    pub country: Option<String>,
    /// Its ISO 639 codes: "it".
    #[serde(default)]
    pub languages: Vec<String>,
    /// Its tags but its country and languages, most telling first.
    pub tags: Vec<String>,
}

impl Featured {
    pub fn to_station(&self, names: &mut Tags) -> DirectoryStation {
        let mut tags = self.tags.clone();
        if self.codec != "FLAC" && self.bitrate >= HIGH_BITRATE {
            tags.insert(0, "quality:high".to_owned());
        }
        if let Some(country) = self.country.as_deref().and_then(|c| names.country(c, "")) {
            tags.push(country);
        }
        for code in &self.languages {
            if let Some(language) = names.language(code) {
                tags.push(language.to_owned());
            }
        }
        DirectoryStation {
            id: format!("freya:{}", self.id),
            name: self.name.clone(),
            url: self.url.clone(),
            homepage: self.homepage.clone(),
            logo: self.logo.clone(),
            tags,
            codec: self.codec.clone(),
            bitrate: self.bitrate,
            featured: true,
        }
    }
}

/// The first page's rows, and the stations Freya picked.
pub fn load() -> (Vec<Shelf>, Vec<Featured>) {
    match toml::from_str::<File>(FEATURED) {
        Ok(file) => (file.shelves, file.stations),
        Err(e) => {
            log::error!("the stations Freya picked could not be read: {e}");
            (Vec::new(), Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::radio::index::is_web;

    #[test]
    fn the_picked_stations_are_whole() {
        let (shelves, stations) = load();
        assert!(!shelves.is_empty() && !stations.is_empty());
        let mut names = Tags::new();
        let mut ids = HashSet::new();
        for featured in &stations {
            assert!(ids.insert(&featured.id), "{} twice", featured.id);
            assert!(
                featured
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "{}",
                featured.id
            );
            assert!(is_web(&featured.url), "{}", featured.id);
            assert!(featured.url.starts_with("https://") || featured.url.starts_with("http://"));
            assert!(
                ["MP3", "AAC", "OGG", "FLAC"].contains(&featured.codec.as_str()),
                "{}",
                featured.id
            );
            let station = featured.to_station(&mut names);
            assert!(
                station
                    .tags
                    .iter()
                    .any(|t| t.starts_with("genre:") || t.starts_with("format:")),
                "{} has no genre",
                featured.id
            );
            for tag in &station.tags {
                assert!(names.knows(tag), "{}: {tag} is not a tag", featured.id);
            }
            if let Some(country) = &featured.country {
                assert!(
                    station.tags.contains(&format!("country:{country}")),
                    "{}",
                    featured.id
                );
            }
            assert_eq!(
                station
                    .tags
                    .iter()
                    .filter(|t| t.starts_with("language:"))
                    .count(),
                featured.languages.len(),
                "{}",
                featured.id
            );
            let lossless = station
                .tags
                .iter()
                .any(|t| t == "quality:lossless" || t == "quality:hi-res");
            assert_eq!(lossless, featured.codec == "FLAC", "{}", featured.id);
        }
        for shelf in &shelves {
            assert!(
                shelf.tags.is_empty() != shelf.stations.is_empty(),
                "{}",
                shelf.title
            );
            for tag in &shelf.tags {
                assert!(names.knows(tag), "{}: {tag} is not a tag", shelf.title);
            }
            for id in &shelf.stations {
                assert!(ids.contains(id), "{}: no station {id}", shelf.title);
            }
        }
    }
}
