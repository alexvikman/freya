//! Recents: what was played and added lately, as a feed by day, out of the way of the library's
//! sections: the albums and playlists played (each with the songs played, folded), the albums
//! that came in and the playlists made, and the radio stations listened to; and, apart, the
//! albums and playlists played most. Asked of the server when shown, and again when it says
//! the plays or the library changed.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use freya_media::thumbnails::Thumbnails;
use freya_protocol::{
    AlbumAdded, AlbumPlayed, LibraryApi, PlayedAt, PlaylistPlayed, StationPlayed, Track,
};
use slint_interpreter::{Struct, Value};

use super::skin_state;

/// How far back the feed goes.
const DAYS: u64 = 30;
/// How many plays, stations and albums added it reads at most.
const MOST: usize = 2000;
/// How many albums and playlists Most Played lists.
const MOST_PLAYED: usize = 100;

/// What the feed shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Played,
    Added,
    Radio,
    MostPlayed,
}

impl Filter {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "all" => Self::All,
            "played" => Self::Played,
            "added" => Self::Added,
            "radio" => Self::Radio,
            "most-played" => Self::MostPlayed,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Played => "played",
            Self::Added => "added",
            Self::Radio => "radio",
            Self::MostPlayed => "most-played",
        }
    }
}

/// What the server told of lately.
#[derive(Clone, Debug, Default)]
pub struct Read {
    pub history: Vec<PlayedAt>,
    pub stations: Vec<StationPlayed>,
    pub added: Vec<AlbumAdded>,
    pub albums: Vec<AlbumPlayed>,
    pub playlists: Vec<PlaylistPlayed>,
}

impl Read {
    /// Asks `api` for what the feed shows since `now` less [`DAYS`].
    fn ask(api: &dyn LibraryApi, now: SystemTime) -> Result<Self, String> {
        let since = now - Duration::from_secs(DAYS * 24 * 3600);
        let error = |e: freya_protocol::ApiError| e.to_string();
        Ok(Self {
            history: api.history(since, MOST).map_err(error)?,
            stations: api.station_history(since, MOST).map_err(error)?,
            added: api.albums_added(since, MOST).map_err(error)?,
            albums: api.most_played_albums(MOST_PLAYED).map_err(error)?,
            playlists: api.most_played_playlists(MOST_PLAYED).map_err(error)?,
        })
    }
}

/// What a row of the feed opens.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Album {
        artist: Option<String>,
        album: Option<String>,
    },
    Playlist(String),
    Station {
        name: String,
        url: String,
    },
}

/// One thing in the feed: something played, or added, on a day.
#[derive(Clone, Debug)]
struct Group {
    target: Target,
    /// "played", "added" or "radio".
    kind: &'static str,
    day: jiff::civil::Date,
    latest: SystemTime,
    /// The songs played, the latest first, each once; and how many plays there were.
    songs: Vec<Track>,
    plays: usize,
    cover: String,
}

/// A playlist the user made, for the feed: its name and when.
pub type Made = (String, SystemTime);

type Answer = Arc<Mutex<Option<Result<Read, String>>>>;

#[derive(Default)]
pub struct Recents {
    filter: Filter,
    read: Read,
    made: Vec<Made>,
    error: String,
    /// Groups unfolded, by what they open and their day.
    unfolded: HashSet<(Target, jiff::civil::Date)>,
    answer: Answer,
    asking: bool,
    /// What was read is old: asked again when next shown.
    stale: bool,
    asked_once: bool,
    /// What each row opens, by row.
    targets: Vec<Option<(Target, jiff::civil::Date)>>,
    shown: bool,
}

impl Recents {
    pub fn set_filter(&mut self, name: &str) {
        if let Some(filter) = Filter::from_name(name)
            && filter != self.filter
        {
            self.filter = filter;
            self.shown = false;
        }
    }

    /// The plays or the library changed: read again when shown.
    pub fn changed(&mut self) {
        self.stale = true;
    }

    /// The playlists the user made, for the feed.
    pub fn set_made(&mut self, made: Vec<Made>) {
        if made != self.made {
            self.made = made;
            self.shown = false;
        }
    }

    /// While shown: asks `api` when what it has is old, on the errands' thread for a server
    /// on the network, and gives the rows anew when anything changed.
    pub fn poll(
        &mut self,
        api: Option<Arc<dyn LibraryApi>>,
        on_network: bool,
    ) -> Option<Vec<Value>> {
        let answered = self.answer.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(answered) = answered {
            self.asking = false;
            match answered {
                Ok(read) => {
                    self.read = read;
                    self.error.clear();
                }
                Err(e) => self.error = format!("Recents could not be read: {e}"),
            }
            self.shown = false;
        }
        if (self.stale || !self.asked_once)
            && !self.asking
            && let Some(api) = api
        {
            self.stale = false;
            self.asked_once = true;
            self.asking = true;
            let answer = Arc::clone(&self.answer);
            let ask = move || {
                let read = Read::ask(api.as_ref(), SystemTime::now());
                *answer.lock().unwrap_or_else(|e| e.into_inner()) = Some(read);
            };
            if on_network {
                crate::errands::run(ask);
            } else {
                ask();
                return self.poll(None, on_network);
            }
        }
        if self.shown {
            return None;
        }
        self.shown = true;
        let today = jiff::Zoned::now().date();
        let (rows, targets) = self.make_rows(today);
        self.targets = targets;
        Some(rows)
    }

    /// The rows are given again, as to a new skin.
    pub fn show_again(&mut self) {
        self.shown = false;
    }

    /// Folds or unfolds the songs of the `row`th row.
    pub fn toggle(&mut self, row: usize) {
        if let Some(Some(key)) = self.targets.get(row).cloned() {
            if !self.unfolded.remove(&key) {
                self.unfolded.insert(key);
            }
            self.shown = false;
        }
    }

    /// What the `row`th row opens.
    pub fn target(&self, row: usize) -> Option<Target> {
        Some(self.targets.get(row)?.as_ref()?.0.clone())
    }

    pub fn values(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("filter", skin_state::string(self.filter.name())),
            ("error", skin_state::string(&self.error)),
            ("loading", Value::Bool(self.asking && !self.asked_once)),
        ]
    }

    /// The groups of the feed, by day, the latest first: what was played from an album, a
    /// playlist or a station, and albums added and playlists made.
    fn groups(&self) -> Vec<Group> {
        let mut groups: Vec<Group> = Vec::new();
        let mut at: HashMap<(Target, &'static str, jiff::civil::Date), usize> = HashMap::new();
        let mut add = |groups: &mut Vec<Group>, group: Group, song: Option<&Track>| {
            let key = (group.target.clone(), group.kind, group.day);
            let i = *at.entry(key).or_insert_with(|| {
                groups.push(group);
                groups.len() - 1
            });
            let group = &mut groups[i];
            group.plays += 1;
            if let Some(song) = song
                && !group
                    .songs
                    .iter()
                    .any(|s| s.path == song.path && s.part == song.part)
            {
                group.songs.push(song.clone());
            }
        };
        for played in &self.read.history {
            let track = &played.track;
            let target = match &played.playlist {
                Some(name) => Target::Playlist(name.clone()),
                None => Target::Album {
                    artist: track.album_artist.clone(),
                    album: track.album.clone(),
                },
            };
            let group = Group {
                target,
                kind: "played",
                day: local_day(played.at),
                latest: played.at,
                songs: Vec::new(),
                plays: 0,
                cover: Thumbnails::key(track.album_artist.as_deref(), track.album.as_deref()),
            };
            add(&mut groups, group, Some(track));
        }
        for station in &self.read.stations {
            let group = Group {
                target: Target::Station {
                    name: station.name.clone(),
                    url: station.url.clone(),
                },
                kind: "radio",
                day: local_day(station.at),
                latest: station.at,
                songs: Vec::new(),
                plays: 0,
                cover: String::new(),
            };
            add(&mut groups, group, None);
        }
        for added in &self.read.added {
            let album = &added.album;
            let group = Group {
                target: Target::Album {
                    artist: album.artist.clone(),
                    album: album.name.clone(),
                },
                kind: "added",
                day: local_day(added.at),
                latest: added.at,
                songs: Vec::new(),
                plays: 0,
                cover: Thumbnails::key(album.artist.as_deref(), album.name.as_deref()),
            };
            add(&mut groups, group, None);
        }
        let since = SystemTime::now() - Duration::from_secs(DAYS * 24 * 3600);
        for (name, made) in self.made.iter().filter(|(_, made)| *made >= since) {
            let group = Group {
                target: Target::Playlist(name.clone()),
                kind: "added",
                day: local_day(*made),
                latest: *made,
                songs: Vec::new(),
                plays: 0,
                cover: String::new(),
            };
            add(&mut groups, group, None);
        }
        groups.sort_by(|a, b| b.day.cmp(&a.day).then(b.latest.cmp(&a.latest)));
        groups
    }

    /// The rows the skin shows, and what each opens.
    #[allow(
        clippy::type_complexity,
        reason = "rows and their targets, side by side"
    )]
    fn make_rows(
        &self,
        today: jiff::civil::Date,
    ) -> (Vec<Value>, Vec<Option<(Target, jiff::civil::Date)>>) {
        let mut rows = Vec::new();
        let mut targets = Vec::new();
        if self.filter == Filter::MostPlayed {
            let mut most: Vec<(u32, Value, Target)> = Vec::new();
            for played in &self.read.albums {
                let album = &played.album;
                let title = album.name.clone().unwrap_or_else(|| "Unknown album".into());
                let artist = album
                    .artist
                    .clone()
                    .unwrap_or_else(|| "Unknown artist".into());
                most.push((
                    played.plays,
                    row(
                        "album",
                        &title,
                        &format!("Album · {artist} · {}", plays(played.plays as usize)),
                        &Thumbnails::key(album.artist.as_deref(), album.name.as_deref()),
                        false,
                        false,
                    ),
                    Target::Album {
                        artist: album.artist.clone(),
                        album: album.name.clone(),
                    },
                ));
            }
            for played in &self.read.playlists {
                most.push((
                    played.plays,
                    row(
                        "playlist",
                        &played.name,
                        &format!("Playlist · {}", plays(played.plays as usize)),
                        "",
                        false,
                        false,
                    ),
                    Target::Playlist(played.name.clone()),
                ));
            }
            most.sort_by_key(|most| std::cmp::Reverse(most.0));
            for (_, value, target) in most {
                rows.push(value);
                targets.push(Some((target, today)));
            }
            return (rows, targets);
        }
        let wanted = |group: &Group| match self.filter {
            Filter::All => true,
            Filter::Played => group.kind == "played",
            Filter::Added => group.kind == "added",
            Filter::Radio => group.kind == "radio",
            Filter::MostPlayed => false,
        };
        let mut day = None;
        for group in self.groups().into_iter().filter(wanted) {
            if day != Some(group.day) {
                day = Some(group.day);
                rows.push(row(
                    "day",
                    &day_name(group.day, today),
                    "",
                    "",
                    false,
                    false,
                ));
                targets.push(None);
            }
            let key = (group.target.clone(), group.day);
            let unfolded = self.unfolded.contains(&key);
            let (icon, title, detail) = match (&group.target, group.kind) {
                (Target::Album { artist, album }, "added") => (
                    "album",
                    album.clone().unwrap_or_else(|| "Unknown album".into()),
                    format!(
                        "Album added · {}",
                        artist.as_deref().unwrap_or("Unknown artist")
                    ),
                ),
                (Target::Album { artist, album }, _) => (
                    "album",
                    album.clone().unwrap_or_else(|| "Unknown album".into()),
                    format!(
                        "Album · {} · {} played",
                        artist.as_deref().unwrap_or("Unknown artist"),
                        songs(group.plays)
                    ),
                ),
                (Target::Playlist(name), "added") => {
                    ("playlist", name.clone(), "Playlist made".to_owned())
                }
                (Target::Playlist(name), _) => (
                    "playlist",
                    name.clone(),
                    format!("Playlist · {} played", songs(group.plays)),
                ),
                (Target::Station { name, .. }, _) => (
                    "radio",
                    name.clone(),
                    match group.plays {
                        1 => "Radio".to_owned(),
                        n => format!("Radio · listened to {n} times"),
                    },
                ),
            };
            let foldable = !group.songs.is_empty();
            rows.push(row(icon, &title, &detail, &group.cover, foldable, unfolded));
            targets.push(Some(key.clone()));
            if unfolded {
                for song in &group.songs {
                    let artist = song.artist.clone().unwrap_or_default();
                    rows.push(row(
                        "song",
                        &song.display_title(),
                        &artist,
                        "",
                        false,
                        false,
                    ));
                    targets.push(Some(key.clone()));
                }
            }
        }
        (rows, targets)
    }
}

fn row(
    kind: &str,
    title: &str,
    detail: &str,
    cover: &str,
    foldable: bool,
    unfolded: bool,
) -> Value {
    Value::Struct(Struct::from_iter([
        ("kind".to_owned(), skin_state::string(kind)),
        ("title".to_owned(), skin_state::string(title)),
        ("detail".to_owned(), skin_state::string(detail)),
        ("cover".to_owned(), skin_state::string(cover)),
        ("foldable".to_owned(), Value::Bool(foldable)),
        ("unfolded".to_owned(), Value::Bool(unfolded)),
    ]))
}

fn local_day(at: SystemTime) -> jiff::civil::Date {
    jiff::Timestamp::try_from(at)
        .map(|at| at.to_zoned(jiff::tz::TimeZone::system()).date())
        .unwrap_or_default()
}

/// "Today", "Yesterday", "Wed, Nov 4", or with the year when not this year's.
fn day_name(day: jiff::civil::Date, today: jiff::civil::Date) -> String {
    if day == today {
        return "Today".to_owned();
    }
    if today.yesterday().is_ok_and(|yesterday| yesterday == day) {
        return "Yesterday".to_owned();
    }
    if day.year() == today.year() {
        day.strftime("%a, %b %-d").to_string()
    } else {
        day.strftime("%a, %b %-d, %Y").to_string()
    }
}

fn songs(n: usize) -> String {
    if n == 1 {
        "1 song".to_owned()
    } else {
        format!("{n} songs")
    }
}

fn plays(n: usize) -> String {
    if n == 1 {
        "1 play".to_owned()
    } else {
        format!("{n} plays")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(album: &str, title: &str, path: &str) -> Track {
        Track {
            id: 0,
            path: path.into(),
            part: None,
            title: Some(title.into()),
            artist: Some("Agalloch".into()),
            album: Some(album.into()),
            album_artist: Some("Agalloch".into()),
            year: None,
            track: None,
            disc: None,
            duration: None,
        }
    }

    fn titles(rows: &[Value]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Value::Struct(s) => match s.get_field("title") {
                    Some(Value::String(t)) => t.to_string(),
                    _ => String::new(),
                },
                _ => String::new(),
            })
            .collect()
    }

    #[test]
    fn plays_are_grouped_by_day_and_by_album_or_playlist() {
        let now = SystemTime::now();
        let hour = Duration::from_secs(3600);
        let day = Duration::from_secs(24 * 3600);
        let mut recents = Recents::default();
        recents.read.history = vec![
            PlayedAt {
                track: track("Pale Folklore", "Two", "/m/2.flac"),
                at: now,
                playlist: None,
            },
            PlayedAt {
                track: track("Pale Folklore", "One", "/m/1.flac"),
                at: now - hour / 60,
                playlist: None,
            },
            PlayedAt {
                track: track("The Mantle", "Odal", "/m/odal.flac"),
                at: now - hour / 30,
                playlist: Some("royalcore".into()),
            },
            PlayedAt {
                track: track("The Mantle", "Odal", "/m/odal.flac"),
                at: now - 2 * day,
                playlist: None,
            },
        ];
        let today = local_day(now);
        let (rows, targets) = recents.make_rows(today);
        let shown = titles(&rows);
        assert_eq!(shown[0], "Today");
        assert_eq!(shown[1], "Pale Folklore");
        assert_eq!(shown[2], "royalcore");
        assert_eq!(shown.len(), 5, "{shown:?}");
        assert_eq!(shown[4], "The Mantle");
        assert!(targets[0].is_none(), "a day opens nothing");
        assert!(targets[1].is_some());
        let Value::Struct(album) = &rows[1] else {
            panic!("a row");
        };
        assert_eq!(
            album.get_field("detail"),
            Some(&skin_state::string("Album · Agalloch · 2 songs played"))
        );
        // Unfolded, its songs follow it, the latest first.
        recents.targets = targets;
        recents.toggle(1);
        let (rows, _) = recents.make_rows(today);
        assert_eq!(&titles(&rows)[2..4], ["Two", "One"]);
        // Only what was played from playlists and albums, or radio.
        recents.set_filter("radio");
        assert!(recents.make_rows(today).0.is_empty());
    }

    #[test]
    fn days_are_named_as_people_say_them() {
        let today = jiff::civil::date(2026, 11, 6);
        assert_eq!(day_name(today, today), "Today");
        assert_eq!(day_name(jiff::civil::date(2026, 11, 5), today), "Yesterday");
        assert_eq!(
            day_name(jiff::civil::date(2026, 11, 4), today),
            "Wed, Nov 4"
        );
        assert_eq!(
            day_name(jiff::civil::date(2025, 12, 31), today),
            "Wed, Dec 31, 2025"
        );
    }
}
