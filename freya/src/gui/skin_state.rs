//! Turning app state into values for the skin API, and writing only what changed.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use freya_audio::{PlaybackState, Status, TrackInfo};
use freya_library::{Entry, Playlist};
use slint::{Model, ModelRc, VecModel};
use slint_interpreter::{SharedString, Struct, Value};

use super::skin_window::SkinWindow;

/// Position is reported to the skin in steps of this size: the time shown changes once a
/// second, and every change redraws the whole window. On a 500 px progress bar a three-minute
/// track moves under 3 px per step.
const POSITION_STEP: Duration = Duration::from_secs(1);

/// Writes values into one of the skin's globals, skipping those that did not change.
pub struct GlobalWriter {
    global: &'static str,
    shown: Vec<(&'static str, Value)>,
    /// Whether the skin exports this global at all.
    pub available: bool,
}

impl GlobalWriter {
    pub fn new(ui: &SkinWindow, global: &'static str) -> Self {
        Self {
            global,
            shown: Vec::new(),
            available: ui.has_global(global),
        }
    }

    pub fn write(&mut self, ui: &SkinWindow, values: Vec<(&'static str, Value)>) {
        if !self.available {
            return;
        }
        for (name, value) in values {
            if self
                .shown
                .iter()
                .any(|(n, v)| *n == name && same(v, &value))
            {
                continue;
            }
            if let Err(e) = ui.set_global_property(self.global, name, value.clone()) {
                log::warn!("skin API {}.{name}: {e}", self.global);
            }
            self.shown.retain(|(n, _)| *n != name);
            self.shown.push((name, value));
        }
    }
}

/// Whether the skin already shows `b` when it shows `a`. Slint counts no image as equal to
/// another, not even two empty ones; a value holding one would be written again on every
/// refresh, and the skin would rebuild all that depends on it, such as a grid's tiles.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Image(a), Value::Image(b)) => {
            a == b || (a.size().is_empty() && b.size().is_empty())
        }
        (Value::Struct(a), Value::Struct(b)) => {
            a.iter().count() == b.iter().count()
                && a.iter()
                    .all(|(name, value)| b.get_field(name).is_some_and(|v| same(value, v)))
        }
        _ => a == b,
    }
}

/// Rows of a list the skin shows. Only rows that changed are updated, so large lists stay
/// cheap and the skin keeps its scroll position.
pub struct ModelRows {
    model: Rc<VecModel<Value>>,
    shown: Vec<Value>,
}

impl ModelRows {
    pub fn new() -> Self {
        Self {
            model: Rc::new(VecModel::default()),
            shown: Vec::new(),
        }
    }

    pub fn model_value(&self) -> Value {
        Value::Model(ModelRc::from(Rc::clone(&self.model)))
    }

    pub fn sync(&mut self, rows: Vec<Value>) {
        if rows.len() == self.shown.len() {
            for (i, (new, old)) in rows.iter().zip(&self.shown).enumerate() {
                if !same(new, old) {
                    self.model.set_row_data(i, new.clone());
                }
            }
        } else {
            self.model.set_vec(rows.clone());
        }
        debug_assert_eq!(self.model.row_count(), rows.len());
        self.shown = rows;
    }

    /// Replaces row `i`, which is shown.
    fn set(&mut self, i: usize, row: Value) {
        self.model.set_row_data(i, row.clone());
        self.shown[i] = row;
    }
}

/// The playlist rows shown by the skin.
pub struct Rows {
    rows: ModelRows,
    /// The entries the rows were made from, and whether each was the current one.
    made_from: Vec<(Entry, bool)>,
}

impl Rows {
    pub fn new() -> Self {
        Self {
            rows: ModelRows::new(),
            made_from: Vec::new(),
        }
    }

    pub fn model_value(&self) -> Value {
        self.rows.model_value()
    }

    /// Makes again only the rows whose entries changed, while the entries are as many as
    /// before: making all of them for every key that moves the selection took 40 ms with
    /// 50,000.
    pub fn sync(&mut self, playlist: &Playlist) {
        let entries = playlist.entries();
        let current = playlist.current();
        if entries.len() == self.made_from.len() {
            for (i, entry) in entries.iter().enumerate() {
                let is_current = current == Some(i);
                let (made, was_current) = &self.made_from[i];
                if made != entry || *was_current != is_current {
                    self.rows.set(i, track_row(i, entry, is_current));
                    self.made_from[i] = (entry.clone(), is_current);
                }
            }
            return;
        }
        let rows = entries
            .iter()
            .enumerate()
            .map(|(i, e)| track_row(i, e, current == Some(i)))
            .collect();
        self.rows.sync(rows);
        self.made_from = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.clone(), current == Some(i)))
            .collect();
    }
}

/// A `TrackRow`: the `index`th entry of a list, `current` when it is the one playing.
pub fn track_row(index: usize, entry: &Entry, current: bool) -> Value {
    Value::Struct(Struct::from_iter([
        ("number".to_owned(), Value::Number(index as f64 + 1.0)),
        ("title".to_owned(), string(entry.display_title())),
        (
            "duration".to_owned(),
            string(entry.duration.map(format_time).unwrap_or_default()),
        ),
        ("selected".to_owned(), Value::Bool(entry.selected)),
        ("current".to_owned(), Value::Bool(current)),
    ]))
}

/// Values for the `Player` global.
pub fn player_values(
    status: &Status,
    track: Option<&Arc<TrackInfo>>,
    playlist: &Playlist,
    volume: f32,
) -> Vec<(&'static str, Value)> {
    let track = status.track.as_ref().or(track);
    let playing = status.state != PlaybackState::Stopped;
    let entry = playlist.current_entry();
    // Stopped, the entry Play would start; with none, nothing, not the track played last.
    let (title, artist, album) = match (track, entry) {
        (Some(t), _) if playing => (
            t.title.clone().unwrap_or_else(|| t.display_title()),
            t.artist.clone().unwrap_or_default(),
            t.album.clone().unwrap_or_default(),
        ),
        (_, Some(e)) => (
            e.title.clone().unwrap_or_else(|| e.display_title()),
            e.artist.clone().unwrap_or_default(),
            e.album.clone().unwrap_or_default(),
        ),
        _ => (String::new(), String::new(), String::new()),
    };
    let length = track.and_then(|t| t.duration).filter(|_| playing);
    let position = if playing {
        let steps = status.position.as_millis() / POSITION_STEP.as_millis();
        POSITION_STEP * steps as u32
    } else {
        Duration::ZERO
    };
    let state = match status.state {
        PlaybackState::Stopped => "stopped",
        PlaybackState::Playing => "playing",
        PlaybackState::Paused => "paused",
    };
    let remaining = length.map_or(Duration::ZERO, |l| l.saturating_sub(position));
    let live = playing && track.is_some_and(|t| freya_audio::is_url(&t.path.to_string_lossy()));
    vec![
        ("title", string(title)),
        ("artist", string(artist)),
        ("album", string(album)),
        (
            "format-info",
            string(
                track
                    .filter(|_| playing)
                    .map(format_info)
                    .unwrap_or_default(),
            ),
        ),
        (
            "state",
            Value::EnumerationValue("PlaybackState".into(), state.into()),
        ),
        ("position", Value::Number(position.as_secs_f64())),
        (
            "length",
            Value::Number(length.map_or(0.0, |l| l.as_secs_f64())),
        ),
        ("position-text", string(format_time(position))),
        (
            "length-text",
            string(format_time(length.unwrap_or_default())),
        ),
        (
            "remaining-text",
            string(format!("-{}", format_time(remaining))),
        ),
        ("volume", Value::Number(f64::from(volume))),
        ("live", Value::Bool(live)),
    ]
}

/// Values for the `Playlist` global, except the rows.
pub fn playlist_values(
    playlist: &Playlist,
    shuffle: bool,
    repeat: bool,
) -> Vec<(&'static str, Value)> {
    vec![
        ("summary", string(summary(playlist))),
        (
            "cursor",
            Value::Number(playlist.cursor().map_or(-1.0, |c| c as f64)),
        ),
        ("shuffle", Value::Bool(shuffle)),
        ("repeat", Value::Bool(repeat)),
    ]
}

/// "12 tracks · 47:13", with "+" when some lengths are unknown.
pub fn summary(playlist: &Playlist) -> String {
    match playlist.len() {
        0 => String::new(),
        n => {
            let (total, known) = playlist.total_duration();
            let tracks = if n == 1 { "track" } else { "tracks" };
            format!(
                "{n} {tracks} · {}{}",
                format_time(total),
                if known { "" } else { "+" }
            )
        }
    }
}

pub fn string(s: impl AsRef<str>) -> Value {
    Value::String(SharedString::from(s.as_ref()))
}

/// When the running program was built, as "2026-10-17" (in UTC): its file's modification time,
/// which a build or an install sets. `None` when the file cannot be read.
pub fn build_date() -> Option<String> {
    let modified = std::fs::metadata(std::env::current_exe().ok()?)
        .ok()?
        .modified()
        .ok()?;
    let seconds = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(civil_date(seconds / 86_400))
}

/// The date `days` after 1970-01-01, as "YYYY-MM-DD" (Howard Hinnant's `civil_from_days`).
fn civil_date(days: u64) -> String {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn format_time(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// "MP3 · 320 kbps · 44.1 kHz · Stereo". For radio the station comes first, unless it is
/// already shown as the title because the station names no song.
pub fn format_info(track: &Arc<TrackInfo>) -> String {
    let mut parts = Vec::new();
    if let Some(station) = track.station.as_ref().filter(|_| track.title.is_some()) {
        parts.push(station.clone());
    }
    parts.push(track.codec.to_uppercase());
    if let Some(kbps) = track.bitrate_kbps {
        parts.push(format!("{kbps} kbps"));
    }
    if track.sample_rate > 0 {
        // As many decimals as needed: 48, 44.1, 22.05, 11.025.
        let khz = format!("{:.3}", f64::from(track.sample_rate) / 1000.0);
        let khz = khz.trim_end_matches('0').trim_end_matches('.');
        parts.push(format!("{khz} kHz"));
    }
    parts.push(
        match track.channels {
            1 => "Mono",
            2 => "Stereo",
            _ => "Surround",
        }
        .to_owned(),
    );
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_with_no_image_is_the_same_again() {
        let page = || {
            Value::Struct(Struct::from_iter([
                ("shown".to_owned(), Value::Bool(false)),
                ("banner".to_owned(), Value::Image(slint::Image::default())),
            ]))
        };
        assert_ne!(page(), page(), "Slint's own comparison");
        assert!(same(&page(), &page()));
        let shown = Value::Struct(Struct::from_iter([
            ("shown".to_owned(), Value::Bool(true)),
            ("banner".to_owned(), Value::Image(slint::Image::default())),
        ]));
        assert!(!same(&page(), &shown));
    }

    #[test]
    fn dates_from_days() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(11_016), "2000-02-29");
        assert_eq!(civil_date(20_743), "2026-10-17");
    }

    #[test]
    fn formats_times_and_info() {
        assert_eq!(format_time(Duration::from_secs(83)), "1:23");
        assert_eq!(format_time(Duration::from_secs(3725)), "1:02:05");
        let track = Arc::new(TrackInfo {
            codec: "flac".into(),
            sample_rate: 44_100,
            channels: 2,
            bitrate_kbps: Some(900),
            ..Default::default()
        });
        assert_eq!(format_info(&track), "FLAC · 900 kbps · 44.1 kHz · Stereo");
        for (rate, text) in [
            (48_000, "48"),
            (22_050, "22.05"),
            (11_025, "11.025"),
            (96_000, "96"),
        ] {
            let t = Arc::new(TrackInfo {
                codec: "pcm".into(),
                sample_rate: rate,
                channels: 1,
                ..Default::default()
            });
            assert_eq!(format_info(&t), format!("PCM · {text} kHz · Mono"));
        }
    }

    #[test]
    fn summary_marks_unknown_lengths() {
        let mut p = Playlist::with_seed(1);
        assert_eq!(summary(&p), "");
        let mut e = Entry::new("/a.mp3");
        e.duration = Some(Duration::from_secs(61));
        p.insert(None, [e, Entry::new("/b.mp3")]);
        assert_eq!(summary(&p), "2 tracks · 1:01+");
    }

    #[test]
    fn rows_update_only_changed_entries() {
        let mut p = Playlist::with_seed(1);
        p.insert(None, ["/a.mp3", "/b.mp3", "/c.mp3"].map(Entry::new));
        let mut rows = Rows::new();
        rows.sync(&p);
        assert_eq!(rows.rows.model.row_count(), 3);
        let before = rows.rows.model.row_data(0);
        p.click(1, freya_library::SelectMode::Single);
        rows.sync(&p);
        assert_eq!(rows.rows.model.row_data(0), before);
        let Some(Value::Struct(row)) = rows.rows.model.row_data(1) else {
            panic!("row is a struct");
        };
        assert_eq!(row.get_field("selected"), Some(&Value::Bool(true)));
    }
}
