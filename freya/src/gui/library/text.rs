//! How the library says things: counts, times, names for what has none, and letters to jump to.

use super::*;

/// "47 min 34 s", or "1 h 5 min" from an hour on, as an album's length reads.
pub(super) fn long_time(total: Duration) -> String {
    let seconds = total.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours} h {minutes} min")
    } else {
        format!("{minutes} min {seconds} s")
    }
}

/// The letter a name is jumped to by: its first letter, A to Z, or another one as it is, such
/// as Å; "#" for one that starts with a digit or a sign.
pub(super) fn letter_of(name: &str) -> String {
    match name.chars().find(|c| !c.is_whitespace()) {
        Some(c) if c.is_alphabetic() => c.to_uppercase().collect(),
        _ => "#".to_owned(),
    }
}

/// The letters to jump to in a list of `names` in order: "#" and A to Z always, then other
/// letters the names start with, in the order they come. Each with the first row it starts,
/// else the next letter's (the last row after the last), and whether any name starts with it.
pub(super) fn letter_jumps<'a>(
    names: impl Iterator<Item = Option<&'a str>>,
) -> Vec<(String, usize, bool)> {
    let mut first: Vec<(String, usize)> = Vec::new();
    let mut count = 0;
    for (row, name) in names.enumerate() {
        count = row + 1;
        let letter = letter_of(name.unwrap_or(""));
        if !first.iter().any(|(l, _)| *l == letter) {
            first.push((letter, row));
        }
    }
    if count == 0 {
        return Vec::new();
    }
    let mut letters: Vec<String> = std::iter::once("#".to_owned())
        .chain(('A'..='Z').map(String::from))
        .collect();
    for (letter, _) in &first {
        if !letters.contains(letter) {
            letters.push(letter.clone());
        }
    }
    let row_of = |letter: &str| first.iter().find(|(l, _)| l == letter).map(|(_, r)| *r);
    let mut jumps: Vec<(String, usize, bool)> = Vec::new();
    // From the last: an absent letter jumps where the next present one starts.
    let mut next = count - 1;
    for letter in letters.into_iter().rev() {
        let row = row_of(&letter);
        if let Some(row) = row {
            next = row;
        }
        jumps.push((letter, next, row.is_some()));
    }
    jumps.reverse();
    jumps
}

pub(super) fn played_text(played: &Played, by_count: bool, now: SystemTime) -> String {
    if by_count {
        count(played.plays as usize, "play")
    } else {
        ago(played.last, now)
    }
}

/// How long before `now` a time was: "Just now", "5 minutes ago", "3 weeks ago".
pub(super) fn ago(time: SystemTime, now: SystemTime) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let seconds = now.duration_since(time).unwrap_or_default().as_secs();
    let (n, unit) = match seconds {
        s if s < MINUTE => return "Just now".to_owned(),
        s if s < HOUR => (s / MINUTE, "minute"),
        s if s < DAY => (s / HOUR, "hour"),
        s if s < 7 * DAY => (s / DAY, "day"),
        s if s < 30 * DAY => (s / (7 * DAY), "week"),
        s if s < 365 * DAY => (s / (30 * DAY), "month"),
        s => (s / (365 * DAY), "year"),
    };
    format!("{} ago", count(n as usize, unit))
}

pub(super) fn cover_key(album: &Album) -> String {
    Thumbnails::key(album.artist.as_deref(), album.name.as_deref())
}

pub(super) fn artist_name(name: Option<&str>) -> String {
    name.map_or("Unknown artist", freya_protocol::plain_name)
        .to_owned()
}

pub(super) fn album_name(name: Option<&str>) -> String {
    name.unwrap_or("Unknown album").to_owned()
}

pub(super) fn genre_name(name: Option<&str>) -> String {
    name.unwrap_or("Unknown genre").to_owned()
}

pub(super) fn stats_text(stats: &freya_protocol::Stats) -> String {
    if stats.tracks == 0 {
        return String::new();
    }
    format!(
        "{} · {}",
        count(stats.tracks as usize, "track"),
        count(stats.albums as usize, "album")
    )
}

/// "1 track", "1,204 tracks".
pub(super) fn count(n: usize, noun: &str) -> String {
    let plural = if n == 1 { "" } else { "s" };
    format!("{} {noun}{plural}", thousands(n))
}

/// 12345 → "12,345".
pub(super) fn thousands(n: usize) -> String {
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

/// A folder as the user knows it: "~/Music" rather than "/Users/name/Music".
pub(super) fn display_path(path: &Path) -> String {
    let home = directories::BaseDirs::new().map(|d| d.home_dir().to_owned());
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if cfg!(unix) => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}
