//! The server's radio directory, as the radio view browses it: the first page's rows and tags,
//! and the stations found by the tags and words chosen, a page at a time. The server is asked
//! on a thread of its own, and what it answers is taken on the next tick.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use freya_protocol::{
    DirectoryStation, RadioDirectoryStatus, RadioQuery, RadioResults, RadioShelf, RadioTag,
    RadioTagKind, Server,
};
use slint_interpreter::{Struct, Value};

use super::skin_state::string;

/// How many stations are asked for at once.
const PAGE: u32 = 60;
/// The kinds of tags, as their rows are shown, and how many of each the first page offers.
const KINDS: &[(RadioTagKind, &str, usize)] = &[
    (RadioTagKind::Quality, "Quality", 3),
    (RadioTagKind::Genre, "Genres", 30),
    (RadioTagKind::Country, "Countries", 30),
    (RadioTagKind::Language, "Languages", 20),
    (RadioTagKind::Era, "Decades", 7),
    (RadioTagKind::Format, "Formats", 7),
];

enum Answer {
    /// The directory's state, first page and tags.
    Home(freya_protocol::ApiResult<(RadioDirectoryStatus, Vec<RadioShelf>, Vec<RadioTag>)>),
    /// The stations of the search asked as `asked`, from `offset` on.
    Found {
        asked: u64,
        offset: u32,
        results: freya_protocol::ApiResult<RadioResults>,
    },
}

pub struct Directory {
    server: Option<Arc<dyn Server>>,
    status: RadioDirectoryStatus,
    shelves: Vec<RadioShelf>,
    /// Every tag of the directory.
    tags: Vec<RadioTag>,
    /// The words and tags looked for.
    text: String,
    chosen: Vec<String>,
    found: Vec<DirectoryStation>,
    total: u32,
    /// The tags of the stations found besides those chosen.
    refine: Vec<RadioTag>,
    /// The search asked last; answers to one before are left.
    asked: u64,
    /// Whether an answer is awaited for it.
    asking: bool,
    answers: (Sender<Answer>, Receiver<Answer>),
    /// Goes up with every change, so the skin's rows are only made again after one.
    version: u64,
}

impl Directory {
    pub fn new() -> Self {
        Self {
            server: None,
            status: RadioDirectoryStatus::default(),
            shelves: Vec::new(),
            tags: Vec::new(),
            text: String::new(),
            chosen: Vec::new(),
            found: Vec::new(),
            total: 0,
            refine: Vec::new(),
            asked: 0,
            asking: false,
            answers: channel(),
            version: 0,
        }
    }

    /// Browses the directory of `server` from now on.
    pub fn connect(&mut self, server: Arc<dyn Server>) {
        self.server = Some(server);
        self.reload();
    }

    /// Asks the server for the directory again: it changed, or the server is new.
    pub fn reload(&mut self) {
        let Some(server) = self.server.clone() else {
            return;
        };
        let answers = self.answers.0.clone();
        std::thread::spawn(move || {
            let home = server
                .radio_directory()
                .and_then(|status| Ok((status, server.radio_shelves()?, server.radio_tags()?)));
            let _ = answers.send(Answer::Home(home));
        });
        if self.is_searching() {
            self.search(0);
        }
    }

    /// Tells the server that the station `id` was played, the window not waiting for it.
    pub fn played(&self, id: &str) {
        if let Some(server) = self.server.clone() {
            let id = id.to_owned();
            crate::errands::run(move || {
                if let Err(e) = server.radio_played(&id) {
                    log::debug!("could not tell the server of a play: {e}");
                }
            });
        }
    }

    /// Whether words or tags are looked for, rather than the first page shown.
    pub fn is_searching(&self) -> bool {
        !self.text.trim().is_empty() || !self.chosen.is_empty()
    }

    pub fn find(&mut self, text: &str) {
        if self.text != text {
            self.text = text.to_owned();
            self.start();
        }
    }

    /// Looks for the tag `id` too, or no longer.
    pub fn toggle_tag(&mut self, id: &str) {
        match self.chosen.iter().position(|t| t == id) {
            Some(at) => {
                self.chosen.remove(at);
            }
            None => self.chosen.push(id.to_owned()),
        }
        self.start();
    }

    /// Looks for the tags of the first page's `index`th row.
    pub fn show_shelf(&mut self, index: usize) {
        let Some(shelf) = self.shelves.get(index) else {
            return;
        };
        if shelf.tags.is_empty() {
            return;
        }
        self.chosen = shelf.tags.clone();
        self.text.clear();
        self.start();
    }

    /// Back to the first page.
    pub fn clear(&mut self) {
        self.text.clear();
        self.chosen.clear();
        self.start();
    }

    /// Asks for more of the stations found, when there are more and none are being asked for.
    pub fn more(&mut self) {
        if self.is_searching() && !self.asking && (self.found.len() as u32) < self.total {
            self.search(self.found.len() as u32);
        }
    }

    /// A new search, from its first station.
    fn start(&mut self) {
        self.asked += 1;
        self.found.clear();
        self.total = 0;
        self.refine.clear();
        self.version += 1;
        if self.is_searching() {
            self.search(0);
        } else {
            self.asking = false;
        }
    }

    fn search(&mut self, offset: u32) {
        let Some(server) = self.server.clone() else {
            return;
        };
        let query = RadioQuery {
            text: self.text.clone(),
            tags: self.chosen.clone(),
            offset,
            limit: PAGE,
        };
        let asked = self.asked;
        let answers = self.answers.0.clone();
        self.asking = true;
        std::thread::spawn(move || {
            let results = server.radio_search(&query);
            let _ = answers.send(Answer::Found {
                asked,
                offset,
                results,
            });
        });
    }

    /// Takes what the server answered. Returns whether anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(answer) = self.answers.1.try_recv() {
            match answer {
                Answer::Home(Ok((status, shelves, tags))) => {
                    self.status = status;
                    self.shelves = shelves;
                    self.tags = tags;
                    changed = true;
                }
                Answer::Home(Err(e)) => log::warn!("could not read the radio directory: {e}"),
                Answer::Found {
                    asked,
                    offset,
                    results,
                } if asked == self.asked => {
                    self.asking = false;
                    match results {
                        Ok(results) => {
                            if offset == 0 {
                                self.found = results.stations;
                                self.refine = results.tags;
                            } else if offset as usize == self.found.len() {
                                self.found.extend(results.stations);
                            }
                            self.total = results.total;
                        }
                        Err(e) => log::warn!("could not search the radio directory: {e}"),
                    }
                    changed = true;
                }
                Answer::Found { .. } => {}
            }
        }
        if changed {
            self.version += 1;
        }
        changed
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// The `index`th station shown: of those found, or of the first page's rows one after the
    /// other.
    pub fn get(&self, index: usize) -> Option<&DirectoryStation> {
        if self.is_searching() {
            self.found.get(index)
        } else {
            self.shelves.iter().flat_map(|s| &s.stations).nth(index)
        }
    }

    /// What the skin shows of it: the stations shown, as `DirectoryStationRow`s, and the first
    /// page's rows, given which stations the server keeps, whether a station plays, and the
    /// logos at hand; the tags to choose and those chosen.
    pub fn rows(
        &self,
        saved: &HashSet<String>,
        playing: &dyn Fn(&str) -> bool,
        logo: &mut dyn FnMut(&str) -> Option<slint::Image>,
    ) -> Rows {
        let mut row = |station: &DirectoryStation| {
            let logo = station.logo.as_deref().and_then(&mut *logo);
            Value::Struct(Struct::from_iter([
                ("logo".to_owned(), Value::Image(logo.unwrap_or_default())),
                ("name".to_owned(), string(&station.name)),
                ("detail".to_owned(), string(self.detail(station))),
                ("quality".to_owned(), string(quality(station))),
                (
                    "saved".to_owned(),
                    Value::Bool(saved.contains(&station.url)),
                ),
                ("playing".to_owned(), Value::Bool(playing(&station.url))),
                ("featured".to_owned(), Value::Bool(station.featured)),
            ]))
        };
        let searching = self.is_searching();
        let mut shelves = Vec::new();
        let found: Vec<Value> = if searching {
            self.found.iter().map(&mut row).collect()
        } else {
            let mut first = 0;
            let mut rows = Vec::new();
            for shelf in &self.shelves {
                shelves.push(Value::Struct(Struct::from_iter([
                    ("title".to_owned(), string(&shelf.title)),
                    ("first".to_owned(), Value::Number(first as f64)),
                    (
                        "count".to_owned(),
                        Value::Number(shelf.stations.len() as f64),
                    ),
                    ("tagged".to_owned(), Value::Bool(!shelf.tags.is_empty())),
                ])));
                first += shelf.stations.len();
                rows.extend(shelf.stations.iter().map(&mut row));
            }
            rows
        };
        let offered = if searching { &self.refine } else { &self.tags };
        let tags: Vec<Value> = KINDS
            .iter()
            .filter_map(|&(kind, title, most)| {
                let rows: Vec<Value> = offered
                    .iter()
                    .filter(|t| t.kind == kind)
                    .take(most)
                    .map(|t| tag_row(t, false))
                    .collect();
                (!rows.is_empty()).then(|| {
                    Value::Struct(Struct::from_iter([
                        ("kind".to_owned(), string(kind.prefix())),
                        ("title".to_owned(), string(title)),
                        ("tags".to_owned(), super::skin_state::model(rows)),
                    ]))
                })
            })
            .collect();
        let chosen = self
            .chosen
            .iter()
            .map(|id| tag_row(&self.tag(id), true))
            .collect();
        Rows {
            found,
            shelves,
            tags,
            chosen,
        }
    }

    /// How the directory and the search go, for the skin.
    pub fn values(&self) -> Vec<(&'static str, Value)> {
        let total = if self.is_searching() {
            f64::from(self.total)
        } else {
            -1.0
        };
        vec![
            ("directory", string(&self.status.state)),
            ("summary", string(self.summary())),
            ("total", Value::Number(total)),
            ("query", string(&self.text)),
            ("searching", Value::Bool(self.asking)),
        ]
    }

    /// The tag `id`, as the directory names it.
    fn tag(&self, id: &str) -> RadioTag {
        self.tags
            .iter()
            .chain(&self.refine)
            .find(|t| t.id == id)
            .cloned()
            .unwrap_or_else(|| RadioTag {
                id: id.to_owned(),
                kind: RadioTagKind::of(id).unwrap_or(RadioTagKind::Genre),
                name: id.split_once(':').map_or(id, |(_, name)| name).to_owned(),
                stations: 0,
            })
    }

    /// "Jazz · Italy · Italian": a station's tags but its quality, the first few.
    fn detail(&self, station: &DirectoryStation) -> String {
        let mut names: Vec<String> = Vec::new();
        let mut genres = 0;
        for id in &station.tags {
            let kind = RadioTagKind::of(id);
            if kind == Some(RadioTagKind::Quality) {
                continue;
            }
            if matches!(kind, Some(RadioTagKind::Genre)) {
                genres += 1;
                if genres > 3 {
                    continue;
                }
            }
            names.push(self.tag(id).name);
            if names.len() == 5 {
                break;
            }
        }
        names.join(" · ")
    }

    /// "45,012 stations", or how the directory goes.
    fn summary(&self) -> String {
        let stations = self.status.stations;
        match self.status.state.as_str() {
            "loading" if stations == 0 => "Fetching stations…".to_owned(),
            "loading" => format!("{} stations · fetching more…", thousands(stations)),
            "off" => format!("{} stations picked by Freya", thousands(stations)),
            _ if stations == 0 => String::new(),
            _ => format!("{} stations", thousands(stations)),
        }
    }
}

/// The skin's rows of the directory.
pub struct Rows {
    pub found: Vec<Value>,
    pub shelves: Vec<Value>,
    pub tags: Vec<Value>,
    pub chosen: Vec<Value>,
}

/// A `RadioTagRow`.
fn tag_row(tag: &RadioTag, chosen: bool) -> Value {
    Value::Struct(Struct::from_iter([
        ("id".to_owned(), string(&tag.id)),
        ("name".to_owned(), string(&tag.name)),
        ("kind".to_owned(), string(tag.kind.prefix())),
        (
            "stations".to_owned(),
            Value::Number(f64::from(tag.stations)),
        ),
        ("chosen".to_owned(), Value::Bool(chosen)),
    ]))
}

/// "Hi-Res", "Lossless" or "320 kbps".
fn quality(station: &DirectoryStation) -> String {
    if station.tags.iter().any(|t| t == "quality:hi-res") {
        "Hi-Res".to_owned()
    } else if station.tags.iter().any(|t| t == "quality:lossless") {
        "Lossless".to_owned()
    } else if station.bitrate > 0 {
        format!("{} kbps", station.bitrate)
    } else {
        String::new()
    }
}

/// "45,012".
fn thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_qualities_read_well() {
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(45_012), "45,012");
        assert_eq!(thousands(1_234_567), "1,234,567");
        let mut station = DirectoryStation {
            id: "x".into(),
            name: "X".into(),
            url: "https://x.example.org/".into(),
            homepage: None,
            logo: None,
            tags: vec!["quality:lossless".into(), "genre:jazz".into()],
            codec: "FLAC".into(),
            bitrate: 0,
            featured: false,
        };
        assert_eq!(quality(&station), "Lossless");
        station.tags.insert(0, "quality:hi-res".into());
        assert_eq!(quality(&station), "Hi-Res");
        station.tags.clear();
        station.bitrate = 320;
        assert_eq!(quality(&station), "320 kbps");
    }
}
