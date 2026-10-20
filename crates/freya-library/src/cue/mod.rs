//! CUE sheets: a disc ripped as one audio file, with a small text file next to it that says
//! where each track starts. Freya lists and plays such a file as its tracks.
//!
//! Only what a music player needs is read: the files, the tracks with where they start
//! (`INDEX 01`), titles and performers, and the genre, year and disc number that rippers write
//! as `REM` lines. The gap before a track (`INDEX 00`) is played at the end of the track
//! before, as most players do.

pub(crate) mod encoding;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::files;
use crate::tags::TagInfo;
pub use freya_protocol::Part;

/// One track of a CUE sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct CueTrack {
    pub path: PathBuf,
    pub part: Part,
    /// As the sheet has them. The duration is unknown for the last track of a file until the
    /// file is read.
    pub tags: TagInfo,
}

impl CueTrack {
    /// The track's tags, completed from those of its audio file (`file`): the length of a
    /// file's last track, and the album, year, genre and disc where the sheet does not say.
    /// The file's title and artist are left alone; they are the first track's, or the disc's.
    pub fn tags_with(&self, file: &TagInfo) -> TagInfo {
        let tags = self.tags.clone();
        // The file's artist id, unless the sheet names another artist.
        let file_artist = tags.album_artist.is_none() || tags.album_artist == file.album_artist;
        TagInfo {
            album: tags.album.or_else(|| file.album.clone()),
            album_artist: tags.album_artist.or_else(|| file.album_artist.clone()),
            artist_mbid: tags
                .artist_mbid
                .or_else(|| file.artist_mbid.clone().filter(|_| file_artist)),
            genre: tags.genre.or_else(|| file.genre.clone()),
            year: tags.year.or(file.year),
            disc: tags.disc.or(file.disc),
            duration: self.part.duration(file.duration),
            ..tags
        }
    }
}

/// What one CUE sheet makes of its audio files.
#[derive(Clone, Debug, PartialEq)]
pub struct Disc {
    pub sheet: PathBuf,
    /// The audio files that are cut into tracks, in the sheet's order.
    pub files: Vec<PathBuf>,
    pub tracks: Vec<CueTrack>,
}

/// An audio file to list as it is, or a disc of tracks cut from audio files.
#[derive(Clone, Debug, PartialEq)]
pub enum Found {
    File(PathBuf),
    Disc(Disc),
}

impl Found {
    /// The file this is read from: the audio file, or the disc's sheet.
    pub fn source(&self) -> &Path {
        match self {
            Found::File(path) => path,
            Found::Disc(disc) => &disc.sheet,
        }
    }
}

pub fn is_sheet(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cue"))
}

/// Sorts audio files and CUE sheets, as [`files::collect_with_sheets`] finds them, into what
/// to list: each audio file that a sheet cuts into tracks is replaced by its disc, in the
/// place of the disc's first file. When two sheets cut the same file, one named after the
/// file wins, otherwise the first. Sheets that cannot be read or cut nothing are left out.
pub fn apply_sheets(found: Vec<PathBuf>) -> Vec<Found> {
    let (sheets, audio): (Vec<PathBuf>, Vec<PathBuf>) =
        found.into_iter().partition(|path| is_sheet(path));
    let mut discs: Vec<Disc> = files::side_by_side(&sheets, |sheet| read_disc(sheet))
        .into_iter()
        .flatten()
        .collect();
    // A stable sort: otherwise the order they were found in.
    discs.sort_by_key(|disc| !disc.named_after_its_file());
    let mut cut = HashSet::new();
    discs.retain(|disc| {
        let free = disc.files.iter().all(|file| !cut.contains(file));
        if free {
            cut.extend(disc.files.iter().cloned());
        }
        free
    });

    let mut discs: Vec<Option<Disc>> = discs.into_iter().map(Some).collect();
    let first_files: HashMap<PathBuf, usize> = discs
        .iter()
        .enumerate()
        .filter_map(|(i, disc)| Some((disc.as_ref()?.files.first()?.clone(), i)))
        .collect();
    let mut out = Vec::new();
    for path in audio {
        if let Some(&i) = first_files.get(&path) {
            out.extend(discs[i].take().map(Found::Disc));
        } else if !cut.contains(&path) {
            out.push(Found::File(path));
        }
    }
    // Discs whose audio files were not among what was found, like a sheet added on its own.
    out.extend(discs.into_iter().flatten().map(Found::Disc));
    out
}

/// Reads a CUE sheet and finds its audio files. `None` if it cannot be read or cuts no file
/// into tracks.
pub fn read_disc(sheet: &Path) -> Option<Disc> {
    let bytes = std::fs::read(sheet).ok()?;
    let mut parsed = Sheet::parse(&encoding::decode(&bytes));
    if parsed.disc.is_none() {
        parsed.disc = disc_in_name(sheet, parsed.title.as_deref());
    }
    let folder = sheet.parent()?;
    let audio: Vec<PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| files::is_audio(path))
        .collect();

    let mut files = Vec::new();
    let mut tracks = Vec::new();
    for (index, name) in parsed.files.iter().enumerate() {
        let mut cues: Vec<&SheetTrack> = parsed
            .tracks
            .iter()
            .filter(|t| t.file == index && t.start.is_some())
            .collect();
        cues.sort_by_key(|t| t.start);
        // One track from the very start is the whole file, which is better listed as itself,
        // with its own tags: some sheets list a folder of files that are tracks already.
        if cues.is_empty() || (cues.len() == 1 && cues[0].start == Some(Duration::ZERO)) {
            continue;
        }
        let Some(path) = find_file(name, sheet, parsed.files.len() == 1, &audio) else {
            log::debug!("{}: no audio file for {name:?}", sheet.display());
            continue;
        };
        for (i, cue) in cues.iter().enumerate() {
            let start = cue.start.unwrap_or_default();
            let end = cues.get(i + 1).and_then(|next| next.start);
            let part = Part { start, end };
            tracks.push(CueTrack {
                path: path.clone(),
                part,
                tags: parsed.tags(cue, part),
            });
        }
        files.push(path);
    }
    (!tracks.is_empty()).then(|| Disc {
        sheet: sheet.to_owned(),
        files,
        tracks,
    })
}

impl Disc {
    /// Whether the sheet is named like one of its files: `Album.cue` or `Album.flac.cue` for
    /// `Album.flac`.
    fn named_after_its_file(&self) -> bool {
        let sheet = lowercase_stem(&self.sheet);
        self.files
            .iter()
            .any(|file| sheet == lowercase_stem(file) || sheet == lowercase_name(file))
    }
}

/// The audio file that a sheet's `FILE` line means. The name is often not quite right: the
/// sheet was written for a `.wav` that was then compressed to `.flac`, or renamed. Tried in
/// turn: the name itself, the name with another extension, and, for a sheet with a single
/// file, a file named like the sheet, or the only audio file in the folder. Names are
/// compared without regard to case. `audio` holds the audio files next to the sheet.
fn find_file(name: &str, sheet: &Path, only_file: bool, audio: &[PathBuf]) -> Option<PathBuf> {
    // Sheets from Windows may give the name with a folder, or with backslashes.
    let name = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .to_lowercase();
    let stem = Path::new(&name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let find = |matches: &dyn Fn(&Path) -> bool| audio.iter().find(|p| matches(p)).cloned();
    find(&|p| lowercase_name(p) == name)
        .or_else(|| find(&|p| lowercase_stem(p) == stem))
        .or_else(|| {
            if !only_file {
                return None;
            }
            let sheet = lowercase_stem(sheet);
            find(&|p| lowercase_stem(p) == sheet || lowercase_name(p) == sheet)
                .or_else(|| (audio.len() == 1).then(|| audio[0].clone()))
        })
}

fn lowercase_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn lowercase_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// The disc number in a sheet's name, like `Album [CD2].cue` or `Album - Disc 2.cue`, for an
/// album of several discs whose sheets do not say. Not when the album's title has it too, as
/// in a series of albums called `... - CD 14`.
fn disc_in_name(sheet: &Path, title: Option<&str>) -> Option<u32> {
    let name = lowercase_stem(sheet);
    let title = title.unwrap_or_default().to_lowercase();
    for word in ["cd", "disc", "disk"] {
        for (at, _) in name.match_indices(word) {
            let starts_word = !name[..at].ends_with(char::is_alphanumeric);
            let rest = name[at + word.len()..].trim_start_matches([' ', '_', '-', '.']);
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let mention = &name[at..name.len() - rest.len() + digits.len()];
            if starts_word && !digits.is_empty() && !title.contains(mention) {
                return digits.parse().ok();
            }
        }
    }
    None
}

/// A CUE sheet as written, before its files are looked for.
#[derive(Debug, Default)]
struct Sheet {
    title: Option<String>,
    performer: Option<String>,
    genre: Option<String>,
    year: Option<u32>,
    disc: Option<u32>,
    /// File names as the `FILE` lines give them.
    files: Vec<String>,
    tracks: Vec<SheetTrack>,
}

#[derive(Debug, Default)]
struct SheetTrack {
    number: u32,
    title: Option<String>,
    performer: Option<String>,
    /// Index into `Sheet::files`: the file the track starts in.
    file: usize,
    /// Where `INDEX 01` puts the track.
    start: Option<Duration>,
}

impl Sheet {
    /// Reads what Freya needs from a sheet and skips everything else, including lines it does
    /// not understand.
    fn parse(contents: &str) -> Self {
        let mut sheet = Self::default();
        for line in contents.lines() {
            let (command, rest) = split_word(line.trim());
            match command.to_ascii_uppercase().as_str() {
                "FILE" => sheet.files.push(file_name(rest)),
                "TRACK" if !sheet.files.is_empty() => sheet.tracks.push(SheetTrack {
                    number: split_word(rest).0.parse().unwrap_or(0),
                    file: sheet.files.len() - 1,
                    ..Default::default()
                }),
                "INDEX" => {
                    let (number, time) = split_word(rest);
                    if let (Some(track), Ok(1)) = (sheet.tracks.last_mut(), number.parse::<u32>()) {
                        track.start = parse_time(time);
                        // The gap before a track may end the file before; the track itself
                        // is in the file its `INDEX 01` follows.
                        track.file = sheet.files.len() - 1;
                    }
                }
                "TITLE" => match sheet.tracks.last_mut() {
                    Some(track) => track.title = text(rest),
                    None => sheet.title = text(rest),
                },
                "PERFORMER" => match sheet.tracks.last_mut() {
                    Some(track) => track.performer = text(rest),
                    None => sheet.performer = text(rest),
                },
                "REM" if sheet.tracks.is_empty() => {
                    let (key, value) = split_word(rest);
                    match key.to_ascii_uppercase().as_str() {
                        "GENRE" => sheet.genre = text(value),
                        "DATE" => sheet.year = leading_number(value).filter(|&y| y >= 1000),
                        "DISCNUMBER" => sheet.disc = leading_number(value),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        sheet
    }

    /// The tags of one track, the sheet's where the track has none of its own.
    fn tags(&self, track: &SheetTrack, part: Part) -> TagInfo {
        let (mut title, mut artist) = (track.title.clone(), track.performer.clone());
        // Some sheets give "Artist - Title" as both the title and the performer.
        if let (Some(both), Some(performer)) = (&title, &artist)
            && both == performer
            && let Some((a, t)) = both.split_once(" - ")
        {
            (artist, title) = (Some(a.trim().to_owned()), Some(t.trim().to_owned()));
        }
        TagInfo {
            title,
            artist: artist.or_else(|| self.performer.clone()),
            album: self.title.clone(),
            album_artist: self.performer.clone(),
            artist_mbid: None,
            genre: self.genre.clone(),
            year: self.year,
            track: Some(track.number).filter(|&n| n > 0),
            disc: self.disc,
            duration: part.duration(None),
        }
    }
}

/// The first word of `line` and the rest, both trimmed.
fn split_word(line: &str) -> (&str, &str) {
    match line.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (line, ""),
    }
}

/// A text value: what is between the quotes, or the rest of the line without them. Quotes
/// inside the text are kept, since sheets do not escape them. `None` when empty.
fn text(value: &str) -> Option<String> {
    let value = value.trim();
    let inner = value
        .strip_prefix('"')
        .map(|v| v.strip_suffix('"').unwrap_or(v))
        .unwrap_or(value);
    Some(inner.trim().to_owned()).filter(|s| !s.is_empty())
}

/// The file name of a `FILE` line, which ends with the file's type: `"Album.flac" WAVE`.
fn file_name(rest: &str) -> String {
    match rest.strip_prefix('"').and_then(|r| r.rsplit_once('"')) {
        Some((name, _)) => name.to_owned(),
        None => match rest.rsplit_once(char::is_whitespace) {
            Some((name, _)) => name.trim().to_owned(),
            None => rest.to_owned(),
        },
    }
}

/// `mm:ss:ff`, with 75 frames to the second, rounded to whole milliseconds.
fn parse_time(text: &str) -> Option<Duration> {
    let mut numbers = text.split(':').map(|n| n.trim().parse::<u64>().ok());
    let (Some(Some(minutes)), Some(Some(seconds)), Some(Some(frames)), None) = (
        numbers.next(),
        numbers.next(),
        numbers.next(),
        numbers.next(),
    ) else {
        return None;
    };
    // Damaged sheets can hold any number; one too large for a duration is no time at all.
    let frames = minutes
        .checked_mul(60)?
        .checked_add(seconds)?
        .checked_mul(75)?
        .checked_add(frames)?;
    Some(Duration::from_millis(
        frames.checked_mul(1000)?.checked_add(37)? / 75,
    ))
}

/// The number a value starts with, as in `2004` or `"1/2"`.
fn leading_number(value: &str) -> Option<u32> {
    let digits: String = value
        .trim_start_matches('"')
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests;
