//! The directory's stations, kept to be found by their tags and names at once.

use std::collections::{HashMap, HashSet};

use freya_protocol::{DirectoryStation, RadioQuery, RadioResults, RadioTag, RadioTagKind};

use super::browser::Listed;
use super::featured::Featured;
use super::tags::{self, HIGH_BITRATE, Tags};

/// The codecs Freya plays streams of, as Radio Browser names them, and as shown.
const PLAYABLE: &[(&str, &str)] = &[
    ("MP3", "MP3"),
    ("AAC", "AAC"),
    ("AAC+", "AAC"),
    ("OGG", "OGG"),
    ("FLAC", "FLAC"),
];
/// At most this many stations are given at once.
pub const MOST: u32 = 200;
/// At most this many of each kind of tag are told of with what is found.
const TAGS_OF_A_KIND: usize = 12;

struct Entry {
    station: DirectoryStation,
    /// Its name and its tags' names, as [`tags::fold`] leaves them, to look for words in.
    words: String,
}

/// The stations, the ones Freya picked first, then the best known.
pub struct Index {
    entries: Vec<Entry>,
    /// Where each tag's stations are in `entries`, in order.
    by_tag: HashMap<String, Vec<u32>>,
    /// Where each station is, by its id.
    by_id: HashMap<String, u32>,
    tags: Vec<RadioTag>,
    /// How many came from Radio Browser.
    listed: u32,
}

impl Index {
    /// The stations Freya picked, then those of `listed` Freya plays and does not have already:
    /// one of each name in a country, by the address of its stream too.
    pub fn build(names: &mut Tags, featured: &[Featured], mut listed: Vec<Listed>) -> Self {
        let mut entries: Vec<Entry> = Vec::new();
        for station in featured {
            let station = station.to_station(names);
            entries.push(entry(names, station));
        }
        let mut seen_names: HashSet<String> = HashSet::new();
        let mut seen_urls: HashSet<String> = entries
            .iter()
            .map(|e| e.station.url.to_ascii_lowercase())
            .collect();
        listed.sort_by_key(|s| std::cmp::Reverse(s.rank()));
        let mut from_listed = 0;
        for station in listed {
            let Some(station) = station.to_station(names) else {
                continue;
            };
            let name = format!(
                "{}|{}",
                tags::fold(&station.name),
                station
                    .tags
                    .iter()
                    .find(|t| t.starts_with("country:"))
                    .map_or("", String::as_str)
            );
            if !seen_urls.insert(station.url.to_ascii_lowercase()) || !seen_names.insert(name) {
                continue;
            }
            entries.push(entry(names, station));
            from_listed += 1;
        }
        let mut by_tag: HashMap<String, Vec<u32>> = HashMap::new();
        let mut by_id = HashMap::new();
        for (at, entry) in entries.iter().enumerate() {
            let at = at as u32;
            by_id.insert(entry.station.id.clone(), at);
            for tag in &entry.station.tags {
                by_tag.entry(tag.clone()).or_default().push(at);
            }
        }
        let mut tags: Vec<RadioTag> = by_tag
            .iter()
            .filter_map(|(id, stations)| {
                Some(RadioTag {
                    id: id.clone(),
                    kind: RadioTagKind::of(id)?,
                    name: names.name(id).to_owned(),
                    stations: stations.len() as u32,
                })
            })
            .collect();
        tags.sort_by(|a, b| {
            (a.kind, std::cmp::Reverse(a.stations), &a.name).cmp(&(
                b.kind,
                std::cmp::Reverse(b.stations),
                &b.name,
            ))
        });
        Self {
            entries,
            by_tag,
            by_id,
            tags,
            listed: from_listed,
        }
    }

    pub fn len(&self) -> u32 {
        self.entries.len() as u32
    }

    /// How many came from Radio Browser.
    pub fn listed(&self) -> u32 {
        self.listed
    }

    pub fn tags(&self) -> Vec<RadioTag> {
        self.tags.clone()
    }

    pub fn get(&self, id: &str) -> Option<&DirectoryStation> {
        let at = *self.by_id.get(id)?;
        Some(&self.entries[at as usize].station)
    }

    /// The stations `query` finds, and the tags they have besides.
    pub fn search(&self, query: &RadioQuery) -> RadioResults {
        let words: Vec<String> = tags::fold(&query.text)
            .split_whitespace()
            .take(10)
            .map(str::to_owned)
            .collect();
        let found = self.matching(&query.tags, &words);
        let limit = query.limit.min(MOST) as usize;
        let stations = found
            .iter()
            .skip(query.offset as usize)
            .take(limit)
            .map(|&at| self.entries[at as usize].station.clone())
            .collect();
        let mut counts: HashMap<&str, u32> = HashMap::new();
        for &at in &found {
            for tag in &self.entries[at as usize].station.tags {
                if !query.tags.contains(tag) {
                    *counts.entry(tag.as_str()).or_default() += 1;
                }
            }
        }
        let mut tags: Vec<RadioTag> = self
            .tags
            .iter()
            .filter_map(|tag| {
                Some(RadioTag {
                    stations: *counts.get(tag.id.as_str())?,
                    ..tag.clone()
                })
            })
            .collect();
        tags.sort_by(|a, b| {
            (a.kind, std::cmp::Reverse(a.stations)).cmp(&(b.kind, std::cmp::Reverse(b.stations)))
        });
        let mut of_kind: HashMap<RadioTagKind, usize> = HashMap::new();
        tags.retain(|tag| {
            let n = of_kind.entry(tag.kind).or_default();
            *n += 1;
            *n <= TAGS_OF_A_KIND
        });
        RadioResults {
            stations,
            total: found.len() as u32,
            tags,
        }
    }

    /// Where the stations with every one of `tags` and `words` are, in order.
    fn matching(&self, tags: &[String], words: &[String]) -> Vec<u32> {
        let mut lists: Vec<&Vec<u32>> = Vec::new();
        for tag in tags.iter().take(10) {
            match self.by_tag.get(tag) {
                Some(list) => lists.push(list),
                None => return Vec::new(),
            }
        }
        lists.sort_by_key(|list| list.len());
        let has_words = |at: u32| {
            let haystack = &self.entries[at as usize].words;
            words.iter().all(|word| haystack.contains(word.as_str()))
        };
        match lists.split_first() {
            None => (0..self.len()).filter(|&at| has_words(at)).collect(),
            Some((fewest, rest)) => fewest
                .iter()
                .copied()
                .filter(|at| rest.iter().all(|list| list.binary_search(at).is_ok()))
                .filter(|&at| has_words(at))
                .collect(),
        }
    }
}

fn entry(names: &Tags, station: DirectoryStation) -> Entry {
    let mut words = tags::fold(&station.name);
    for tag in &station.tags {
        words.push(' ');
        words.push_str(&tags::fold(names.name(tag)));
    }
    Entry { station, words }
}

impl Listed {
    /// How well known it is: played lately, and voted for.
    fn rank(&self) -> i64 {
        self.clickcount.max(0) * 10 + self.votes.max(0)
    }

    /// The station, if Freya plays it: a stream, not a video or HLS, of a codec Freya decodes,
    /// at a web address.
    fn to_station(&self, names: &mut Tags) -> Option<DirectoryStation> {
        if self.hls != 0 || self.lastcheckok == 0 {
            return None;
        }
        let codec = PLAYABLE
            .iter()
            .find(|(said, _)| said.eq_ignore_ascii_case(self.codec.trim()))?
            .1;
        let url = [&self.url_resolved, &self.url]
            .into_iter()
            .map(|url| url.trim())
            .find(|url| is_web(url))?;
        let name = one_line(&self.name);
        if name.is_empty() {
            return None;
        }
        let bitrate = u32::try_from(self.bitrate).unwrap_or(0);
        let mut tags = Vec::new();
        // FLAC in Ogg is listed as Ogg; such stations say they are FLAC.
        let says_flac = [&self.name, &self.tags].iter().any(|said| {
            tags::fold(said)
                .split(|c: char| !c.is_alphanumeric())
                .any(|word| word == "flac" || word == "lossless")
        });
        if codec == "FLAC" || (codec == "OGG" && says_flac) {
            tags.push("quality:lossless".to_owned());
        } else if bitrate >= HIGH_BITRATE {
            tags.push("quality:high".to_owned());
        }
        tags.extend(names.of_own(&self.tags));
        if let Some(country) = names.country(&self.countrycode, &self.country) {
            tags.push(country);
        }
        for code in self.languagecodes.split(',') {
            if let Some(language) = names.language(code)
                && !tags.iter().any(|t| t == language)
            {
                tags.push(language.to_owned());
            }
        }
        // Most telling first, as their kinds go; as said within a kind.
        tags.sort_by_key(|tag| RadioTagKind::of(tag));
        Some(DirectoryStation {
            id: self.stationuuid.clone(),
            name,
            url: url.to_owned(),
            homepage: Some(self.homepage.trim())
                .filter(|h| is_web(h))
                .map(str::to_owned),
            logo: Some(self.favicon.trim())
                .filter(|f| is_web(f))
                .map(str::to_owned),
            tags,
            codec: codec.to_owned(),
            bitrate,
            featured: false,
        })
    }
}

/// Whether `url` is a web address, of one line.
pub fn is_web(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && !url.chars().any(char::is_control)
}

/// `text` as one line, without what cannot be shown.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn listed(id: &str, name: &str, tags: &str, country: &str, clicks: i64) -> Listed {
        Listed {
            stationuuid: id.to_owned(),
            name: name.to_owned(),
            url: format!("https://{id}.example.org/stream"),
            tags: tags.to_owned(),
            countrycode: country.to_owned(),
            country: String::new(),
            codec: "MP3".to_owned(),
            bitrate: 128,
            lastcheckok: 1,
            clickcount: clicks,
            ..Listed::default()
        }
    }

    fn index() -> Index {
        let mut names = Tags::new();
        let mut hls = listed("hls", "Some HLS", "jazz", "IT", 1);
        hls.hls = 1;
        let mut video = listed("video", "Some video", "jazz", "IT", 1);
        video.codec = "AAC,H.264".into();
        let mut lossless = listed("flac", "Lossless Jazz", "jazz", "US", 2);
        lossless.codec = "FLAC".into();
        lossless.bitrate = 0;
        let mut ogg = listed("ogg", "Paradise Mellow FLAC", "eclectic", "US", 1);
        ogg.codec = "OGG".into();
        ogg.bitrate = 900;
        let mut again = listed("again", "Radio Jazz Italia", "jazz", "IT", 1);
        again.url = "https://other.example.org/".into();
        Index::build(
            &mut names,
            &[],
            vec![
                listed("a", "Radio Jazz Italia", "Jazz,smooth jazz", "IT", 50),
                listed("b", "Roma Rock", "80s,rock", "IT", 90),
                listed("c", "Nordic Jazz", "jazz", "SE", 10),
                hls,
                video,
                lossless,
                ogg,
                again,
            ],
        )
    }

    fn ids(results: &RadioResults) -> Vec<&str> {
        results.stations.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn finds_stations_by_every_tag_and_word() {
        let index = index();
        assert_eq!(index.len(), 5, "no HLS, video or second station of a name");
        let query = |tags: &[&str], text: &str| RadioQuery {
            text: text.into(),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            offset: 0,
            limit: 10,
        };
        assert_eq!(
            ids(&index.search(&query(&["genre:jazz"], ""))),
            ["a", "c", "flac"]
        );
        let italian_jazz = index.search(&query(&["genre:jazz", "country:it"], ""));
        assert_eq!(ids(&italian_jazz), ["a"]);
        let roma = index.search(&query(&[], "roma"));
        assert_eq!(ids(&roma), ["b"]);
        assert_eq!(
            roma.stations[0].tags,
            ["genre:rock", "era:80s", "country:it"],
            "by kind"
        );
        assert_eq!(
            ids(&index.search(&query(&[], "italy rock"))),
            ["b"],
            "tags' names are words"
        );
        assert!(
            index
                .search(&query(&["genre:nothing"], ""))
                .stations
                .is_empty()
        );
        let lossless = index.search(&query(&["quality:lossless"], ""));
        assert_eq!(ids(&lossless), ["flac", "ogg"]);
        assert_eq!(lossless.stations[0].tags[0], "quality:lossless");

        // What else those found are.
        let jazz = index.search(&query(&["genre:jazz"], ""));
        assert_eq!(jazz.total, 3);
        assert!(jazz.tags.iter().all(|t| t.id != "genre:jazz"));
        let italy = jazz.tags.iter().find(|t| t.id == "country:it").unwrap();
        assert_eq!(italy.stations, 1);

        // A page of them.
        let page = index.search(&RadioQuery {
            offset: 1,
            limit: 1,
            ..query(&["genre:jazz"], "")
        });
        assert_eq!((ids(&page), page.total), (vec!["c"], 3));
    }
}
