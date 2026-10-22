//! Reading and writing M3U/M3U8 and PLS playlist files.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cue::Part;
use crate::playlist::Entry;

/// Playlists downloaded from radio directories are small; anything bigger is not one.
const MAX_DOWNLOAD: u64 = 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15);

/// Loads a playlist file. The format is chosen by extension (`.pls`, otherwise M3U).
pub fn load(path: &Path) -> io::Result<Vec<Entry>> {
    let bytes = std::fs::read(path)?;
    let text = decode_text(&bytes);
    let base = path.parent().unwrap_or(Path::new(""));
    Ok(if has_extension(path, "pls") || is_pls(&text) {
        parse_pls(&text, base)
    } else {
        parse_m3u(&text, base)
    })
}

/// Whether a URL points at a playlist (like a station's `.pls` in a radio directory) rather
/// than at a stream.
pub fn is_playlist_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    is_playlist(Path::new(path))
}

/// Downloads a playlist, as radio directories offer them, and reads its entries. Blocks
/// until done, so call it off the UI thread.
pub fn fetch(url: &str) -> Result<Vec<Entry>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .build()
        .into();
    let mut response = agent.get(url).call().map_err(|e| e.to_string())?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    let text = decode_text(&bytes);
    // HLS playlists list short segments of one stream, not stations.
    if text.contains("#EXT-X-") {
        return Err("HLS streams are not supported yet".into());
    }
    let entries = if is_pls(&text) {
        parse_pls(&text, Path::new(""))
    } else {
        parse_m3u(&text, Path::new(""))
    };
    // Relative entries mean nothing without the files next to the playlist; nor is anything but
    // a stream on the web taken from a playlist from there, such as a path on a Windows share.
    let entries: Vec<Entry> = entries
        .into_iter()
        .filter(|e| is_web(&e.path.to_string_lossy()))
        .collect();
    if entries.is_empty() {
        return Err("the playlist lists no streams".into());
    }
    Ok(entries)
}

/// Whether `path` is an address on the web: `http://` or `https://`.
fn is_web(path: &str) -> bool {
    let lower = path.get(..8).unwrap_or(path).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn is_pls(text: &str) -> bool {
    text.trim_start()
        .get(..10)
        .is_some_and(|start| start.eq_ignore_ascii_case("[playlist]"))
}

/// Saves entries as `.pls` or (any other extension) extended M3U in UTF-8.
pub fn save(path: &Path, entries: &[Entry]) -> io::Result<()> {
    let text = if has_extension(path, "pls") {
        write_pls(entries)
    } else {
        write_m3u(entries)
    };
    // Through a new file renamed over the old, so a crash or a full disk never leaves half a
    // playlist.
    let partial = path.with_extension("partial");
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, path)
}

pub fn is_playlist(path: &Path) -> bool {
    ["m3u", "m3u8", "pls"]
        .iter()
        .any(|ext| has_extension(path, ext))
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// UTF-8 or UTF-16 by their byte order marks, UTF-8 when valid, and otherwise the Windows
/// code page old `.m3u` files were written in, as for CUE sheets.
fn decode_text(bytes: &[u8]) -> String {
    crate::cue::encoding::decode(bytes)
}

/// Resolves a path from a playlist relative to the playlist's folder. `file://` addresses
/// become the paths they name; other URLs are kept as is.
fn resolve(entry: &str, base: &Path) -> PathBuf {
    if let Some(path) = file_url_path(entry) {
        return path;
    }
    if entry.contains("://") {
        return PathBuf::from(entry);
    }
    // Playlists written on Windows use backslashes.
    let entry = if cfg!(windows) {
        entry.to_owned()
    } else {
        entry.replace('\\', "/")
    };
    let path = PathBuf::from(entry);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

/// The path a `file://` address names, as players like VLC and Rhythmbox write them:
/// `file:///home/me/Song%20One.mp3`, or `file:///C:/Music/x.mp3` on Windows.
pub fn file_url_path(entry: &str) -> Option<PathBuf> {
    let scheme = entry.get(..7)?;
    if !scheme.eq_ignore_ascii_case("file://") {
        return None;
    }
    let rest = &entry[7..];
    // The host is empty or "localhost"; the path starts at the next slash.
    let path = &rest[rest.find('/')?..];
    let path = percent_decode(path);
    // "/C:/Music" on Windows is "C:/Music".
    let bytes = path.as_bytes();
    let drive =
        bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':';
    Some(PathBuf::from(if drive && cfg!(windows) {
        &path[1..]
    } else {
        &path[..]
    }))
}

/// `%XX` escapes as the bytes they stand for, read as UTF-8.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Sets an entry's title from a playlist. Files get "Artist - Title" split as Winamp writes
/// it; a stream's title names the station and is kept whole.
fn set_title(entry: &mut Entry, text: &str) {
    if entry.path.to_string_lossy().contains("://") {
        entry.title = Some(text.trim().to_owned()).filter(|t| !t.is_empty());
    } else {
        (entry.artist, entry.title) = split_title(text);
    }
}

/// Splits "Artist - Title" as written by Winamp into its parts.
fn split_title(text: &str) -> (Option<String>, Option<String>) {
    match text.split_once(" - ") {
        Some((artist, title)) => (Some(artist.trim().into()), Some(title.trim().into())),
        None => (
            None,
            Some(text.trim().into()).filter(|t: &String| !t.is_empty()),
        ),
    }
}

/// Freya's own M3U line for a track of a CUE sheet: the part of the file that follows,
/// `start,end` in seconds, with the end left out when it is the file's. Other players ignore
/// it and play the whole file.
const PART_LINE: &str = "#FREYA-PART:";

fn parse_m3u(text: &str, base: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut pending: Option<(Option<Duration>, String)> = None;
    let mut part = None;
    for line in text.lines().map(str::trim) {
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            let (secs, title) = info.split_once(',').unwrap_or((info, ""));
            let duration = secs
                .trim()
                .parse::<i64>()
                .ok()
                .filter(|&s| s > 0)
                .map(|s| Duration::from_secs(s as u64));
            pending = Some((duration, title.to_owned()));
        } else if let Some(text) = line.strip_prefix(PART_LINE) {
            part = parse_part(text);
        } else if line.is_empty() || line.starts_with('#') {
            continue;
        } else {
            let mut entry = Entry::new(resolve(line, base));
            if let Some((duration, title)) = pending.take() {
                entry.duration = duration;
                set_title(&mut entry, &title);
            }
            if let Some(part) = part.take() {
                // The file's tags would describe the whole disc; the title here is the track's.
                entry.part = Some(part);
                entry.duration = part.duration(None).or(entry.duration);
                entry.tags_loaded = entry.duration.is_some();
            }
            entries.push(entry);
        }
    }
    entries
}

fn write_m3u(entries: &[Entry]) -> String {
    let mut out = String::from("#EXTM3U\n");
    for e in entries {
        let secs = e.duration.map_or(-1, |d| d.as_secs() as i64);
        out.push_str(&format!("#EXTINF:{secs},{}\n", e.display_title()));
        if let Some(part) = e.part {
            let end = part.end.map(seconds).unwrap_or_default();
            out.push_str(&format!("{PART_LINE}{},{end}\n", seconds(part.start)));
        }
        out.push_str(&e.path.to_string_lossy());
        out.push('\n');
    }
    out
}

/// `start,end` in seconds, as written by [`write_m3u`].
fn parse_part(text: &str) -> Option<Part> {
    let time = |s: &str| {
        let secs = s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|s| s.is_finite() && *s >= 0.0)?;
        Some(Duration::from_millis((secs * 1000.0).round() as u64))
    };
    let (start, end) = text.split_once(',')?;
    let end = match end.trim() {
        "" => None,
        end => Some(time(end)?),
    };
    Some(Part {
        start: time(start)?,
        end,
    })
}

fn seconds(time: Duration) -> String {
    format!("{:.3}", time.as_secs_f64())
}

/// One numbered entry of a PLS file: its File, Title and Length.
type PlsSlot = (Option<String>, Option<String>, Option<Duration>);

/// PLS has no way to say which part of a file to play, so a track of a CUE sheet is saved as
/// its whole file.
fn parse_pls(text: &str, base: &Path) -> Vec<Entry> {
    // Keys are numbered: File1, Title1, Length1, ... Kept by number rather than in a list as
    // long as the highest one, which a file names freely: `File2000000000000=` would ask for
    // terabytes.
    let mut slots: std::collections::BTreeMap<usize, PlsSlot> = std::collections::BTreeMap::new();
    for line in text.lines().map(str::trim) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let (field, number) =
            key.split_at(key.find(|c: char| c.is_ascii_digit()).unwrap_or(key.len()));
        let Ok(n) = number.parse::<usize>() else {
            continue;
        };
        if n == 0 {
            continue;
        }
        let slot = slots.entry(n).or_default();
        let value = value.trim().to_owned();
        match field {
            "file" => slot.0 = Some(value),
            "title" => slot.1 = Some(value),
            "length" => {
                slot.2 = value
                    .parse::<i64>()
                    .ok()
                    .filter(|&s| s > 0)
                    .map(|s| Duration::from_secs(s as u64))
            }
            _ => {}
        }
    }
    slots
        .into_values()
        .filter_map(|(file, title, duration)| {
            let mut entry = Entry::new(resolve(&file?, base));
            entry.duration = duration;
            if let Some(title) = title {
                set_title(&mut entry, &title);
            }
            Some(entry)
        })
        .collect()
}

fn write_pls(entries: &[Entry]) -> String {
    let mut out = String::from("[playlist]\n");
    for (i, e) in entries.iter().enumerate() {
        let n = i + 1;
        out.push_str(&format!("File{n}={}\n", e.path.to_string_lossy()));
        out.push_str(&format!("Title{n}={}\n", e.display_title()));
        let secs = e.duration.map_or(-1, |d| d.as_secs() as i64);
        out.push_str(&format!("Length{n}={secs}\n"));
    }
    out.push_str(&format!("NumberOfEntries={}\nVersion=2\n", entries.len()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_extended_m3u_with_relative_paths() {
        let text = "#EXTM3U\r\n#EXTINF:225,Artist - Song\r\nsub\\song.mp3\r\n\r\n/abs/other.flac\r\nhttp://radio.example/stream\r\n";
        let entries = parse_m3u(text, Path::new("/lists"));
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].artist.as_deref(), Some("Artist"));
        assert_eq!(entries[0].title.as_deref(), Some("Song"));
        assert_eq!(entries[0].duration, Some(Duration::from_secs(225)));
        if !cfg!(windows) {
            assert_eq!(entries[0].path, Path::new("/lists/sub/song.mp3"));
            assert_eq!(entries[1].path, Path::new("/abs/other.flac"));
        }
        assert_eq!(entries[2].path, Path::new("http://radio.example/stream"));
    }

    #[test]
    fn parses_pls_out_of_order() {
        let text =
            "[playlist]\nTitle2=Two\nFile2=b.mp3\nFile1=a.mp3\nLength1=10\nNumberOfEntries=2\n";
        let entries = parse_pls(text, Path::new("/x"));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, Path::new("/x").join("a.mp3"));
        assert_eq!(entries[0].duration, Some(Duration::from_secs(10)));
        assert_eq!(entries[1].title.as_deref(), Some("Two"));
    }

    #[test]
    fn huge_pls_numbers_take_no_memory() {
        // These once asked for terabytes, or overflowed the list's size.
        let text = "[playlist]\nFile2000000000000=http://x/a\nFile18446744073709551615=http://x/b\nFile3=http://x/c\n";
        let entries = parse_pls(text, Path::new("/x"));
        let paths: Vec<_> = entries.iter().map(|e| e.path.clone()).collect();
        assert_eq!(
            paths,
            ["http://x/c", "http://x/a", "http://x/b"].map(std::path::PathBuf::from)
        );
    }

    #[test]
    fn round_trips_through_files() {
        let dir = std::env::temp_dir().join(format!("freya-pl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Entry::new(dir.join("Ä song.mp3"));
        e.artist = Some("Åsa".into());
        e.title = Some("Låt".into());
        e.duration = Some(Duration::from_secs(61));
        for name in ["list.m3u8", "list.pls"] {
            let path = dir.join(name);
            save(&path, std::slice::from_ref(&e)).unwrap();
            let loaded = load(&path).unwrap();
            assert_eq!(loaded, vec![e.clone()], "{name}");
        }
    }

    #[test]
    fn keeps_the_part_of_a_file_a_track_is() {
        let ms = Duration::from_millis;
        let dir = std::env::temp_dir().join(format!("freya-pl-part-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let track = |start, end, title: &str| {
            let tags = crate::TagInfo {
                title: Some(title.into()),
                artist: Some("Roy Brown".into()),
                ..Default::default()
            };
            let part = Part { start, end };
            Entry::with_tags(dir.join("disc.flac"), Some(part), &tags)
        };
        let entries = [
            track(ms(161_853), Some(ms(372_520)), "Good rockin' tonight"),
            track(ms(372_520), None, "Last"),
        ];
        let path = dir.join("list.m3u8");
        save(&path, &entries).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded[0], entries[0]);
        assert_eq!(loaded[1].part, entries[1].part);
        assert_eq!(loaded[1].title.as_deref(), Some("Last"));
        // The last track's length comes from the file.
        assert!(!loaded[1].tags_loaded);
        assert_eq!(parse_part("1.5,x"), None);
    }

    #[test]
    fn keeps_station_titles_whole() {
        let text = "[playlist]\nnumberofentries=2\nFile1=https://ice1.somafm.com/groovesalad-128-mp3\nTitle1=SomaFM: Groove Salad (#1) - A nicely chilled plate\nLength1=-1\nFile2=https://ice2.somafm.com/groovesalad-128-mp3\nVersion=2\n";
        assert!(is_pls(text));
        let entries = parse_pls(text, Path::new(""));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].artist, None);
        assert_eq!(
            entries[0].title.as_deref(),
            Some("SomaFM: Groove Salad (#1) - A nicely chilled plate")
        );
        assert_eq!(entries[0].duration, None);
    }

    #[test]
    fn recognizes_playlist_urls() {
        assert!(is_playlist_url("https://somafm.com/groovesalad.pls"));
        assert!(is_playlist_url("http://example.com/radio.m3u?token=abc"));
        assert!(!is_playlist_url(
            "https://ice1.somafm.com/groovesalad-128-mp3"
        ));
    }

    #[test]
    fn decodes_latin1() {
        assert_eq!(decode_text(b"caf\xe9"), "café");
    }

    #[test]
    fn decodes_the_windows_code_page_and_utf16() {
        // Windows players wrote curly quotes and dashes from 0x80 to 0x9F, which Latin-1
        // turned into control characters.
        assert_eq!(decode_text(b"Don\x92t Stop \x96 Live"), "Don’t Stop – Live");
        // Some write UTF-16, which came out as bytes with zeros between.
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(
                "#EXTM3U\nsång.mp3"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        assert_eq!(decode_text(&utf16), "#EXTM3U\nsång.mp3");
    }

    #[test]
    fn file_addresses_are_the_files_they_name() {
        // As VLC and Rhythmbox write them; they were kept as addresses, which do not play.
        let text =
            "#EXTM3U\nfile:///home/me/Music/Song%20One%20%C3%A5.mp3\nfile://localhost/srv/b.flac\n";
        let paths: Vec<PathBuf> = parse_m3u(text, Path::new("/elsewhere"))
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(
            paths,
            [
                PathBuf::from("/home/me/Music/Song One å.mp3"),
                PathBuf::from("/srv/b.flac")
            ]
        );
        assert_eq!(
            file_url_path("FILE:///C:/Music/x.mp3"),
            Some(PathBuf::from(if cfg!(windows) {
                "C:/Music/x.mp3"
            } else {
                "/C:/Music/x.mp3"
            }))
        );
        assert_eq!(percent_decode("100%"), "100%");
    }
}
