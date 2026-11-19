//! Finding which release in MusicBrainz a folder of music is: by what its files and its name
//! say, and how well each release's tracks fit the files, their lengths above all.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use freya_protocol::{ProposalState, ReleaseCandidate, TagDraft, TagField, TagMap};
use serde_json::Value;

use crate::library::organize::names::{comparable, split_edition};

/// Releases looked at closely for a folder, each one more question to MusicBrainz.
const LOOKED_AT: usize = 4;
/// How well a release must fit to be saved without being looked at, or to be suggested.
const CONFIDENT: f64 = 0.9;
const SUGGESTED: f64 = 0.6;
/// How close the next release may fit for the choice to be unclear.
const CLOSE: f64 = 0.02;
/// How much a release gains from saying the edition the folder's name does.
const EDITION_BONUS: f64 = 0.03;
/// Within this of a track's length, a file fits it perfectly; past the other, not at all.
const SAME_LENGTH: Duration = Duration::from_secs(2);
const OTHER_LENGTH: Duration = Duration::from_secs(20);

/// A file to be matched to a release's track.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileToMatch {
    pub path: PathBuf,
    pub title: Option<String>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub duration: Option<Duration>,
}

/// A folder to be matched to a release.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FolderToMatch {
    pub artist: Option<String>,
    /// The album's title, by its files or its folder's name, without its edition.
    pub album: Option<String>,
    pub edition: Option<String>,
    /// The release its files say they are, if any.
    pub release_id: Option<String>,
    /// Releases its sound was recognized as on, the likeliest first.
    pub heard_on: Vec<String>,
    pub files: Vec<FileToMatch>,
}

/// A release, as MusicBrainz tells it, with its tracks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Release {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: String,
    pub group_id: String,
    pub date: String,
    pub first_date: String,
    pub country: String,
    pub disambiguation: String,
    pub label: String,
    pub format: String,
    pub discs: u32,
    pub tracks: Vec<ReleaseTrack>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReleaseTrack {
    pub disc: u32,
    pub number: u32,
    pub title: String,
    pub artist: String,
    pub artist_id: String,
    pub length: Option<Duration>,
    pub recording_id: String,
    pub track_id: String,
}

/// A release as a search finds it: enough to choose which to look at closely.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub id: String,
    pub tracks: u32,
    pub score: u32,
}

/// Where releases are asked for, paced by whoever implements it.
pub trait Source {
    fn search(&self, query: &str) -> Result<Vec<Found>, String>;
    fn release(&self, id: &str) -> Result<Release, String>;
}

/// What came of identifying a folder.
#[derive(Clone, Debug, PartialEq)]
pub struct Identified {
    pub state: ProposalState,
    pub chosen: Option<String>,
    pub candidates: Vec<ReleaseCandidate>,
    pub drafts: Vec<TagDraft>,
}

fn text(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// The releases in an answer to a search.
pub fn parse_search(answer: &Value) -> Vec<Found> {
    let Some(releases) = answer.get("releases").and_then(Value::as_array) else {
        return Vec::new();
    };
    releases
        .iter()
        .map(|release| Found {
            id: text(release, "id"),
            tracks: release
                .get("track-count")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            score: release.get("score").and_then(Value::as_u64).unwrap_or(0) as u32,
        })
        .filter(|found| !found.id.is_empty())
        .collect()
}

/// An artist credit as it reads, and the id of the first artist in it.
fn credit(value: &Value) -> (String, String) {
    let Some(credits) = value.get("artist-credit").and_then(Value::as_array) else {
        return (String::new(), String::new());
    };
    let mut name = String::new();
    for credit in credits {
        name.push_str(&text(credit, "name"));
        name.push_str(
            credit
                .get("joinphrase")
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
    }
    let id = credits
        .first()
        .and_then(|credit| credit.get("artist"))
        .map(|artist| text(artist, "id"))
        .unwrap_or_default();
    (name, id)
}

/// A release in an answer to looking one up.
pub fn parse_release(answer: &Value) -> Option<Release> {
    let id = text(answer, "id");
    if id.is_empty() {
        return None;
    }
    let (artist, artist_id) = credit(answer);
    let group = answer.get("release-group");
    let mut tracks = Vec::new();
    let mut formats: Vec<String> = Vec::new();
    let media = answer
        .get("media")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (at, medium) in media.iter().enumerate() {
        let format = text(medium, "format");
        if !format.is_empty() && !formats.contains(&format) {
            formats.push(format);
        }
        let disc = medium
            .get("position")
            .and_then(Value::as_u64)
            .map_or(at as u32 + 1, |p| p as u32);
        for track in medium
            .get("tracks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let recording = track.get("recording").cloned().unwrap_or(Value::Null);
            let (mut track_artist, mut track_artist_id) = credit(track);
            if track_artist.is_empty() {
                (track_artist, track_artist_id) = credit(&recording);
            }
            let length = track
                .get("length")
                .or_else(|| recording.get("length"))
                .and_then(Value::as_u64)
                .map(Duration::from_millis);
            let title = match text(track, "title") {
                title if title.is_empty() => text(&recording, "title"),
                title => title,
            };
            tracks.push(ReleaseTrack {
                disc,
                number: track.get("position").and_then(Value::as_u64).unwrap_or(0) as u32,
                title,
                artist: track_artist,
                artist_id: track_artist_id,
                length,
                recording_id: text(&recording, "id"),
                track_id: text(track, "id"),
            });
        }
    }
    let label = answer
        .get("label-info")
        .and_then(Value::as_array)
        .and_then(|labels| labels.first())
        .and_then(|info| info.get("label"))
        .map(|label| text(label, "name"))
        .unwrap_or_default();
    Some(Release {
        title: text(answer, "title"),
        artist,
        artist_id,
        group_id: group.map(|g| text(g, "id")).unwrap_or_default(),
        date: text(answer, "date"),
        first_date: group
            .map(|g| text(g, "first-release-date"))
            .unwrap_or_default(),
        country: text(answer, "country"),
        disambiguation: text(answer, "disambiguation"),
        label,
        format: formats.join(", "),
        discs: media.len() as u32,
        tracks,
        id,
    })
}

/// A search for what the folder's files and name say, in MusicBrainz's query syntax.
pub fn search_query(folder: &FolderToMatch) -> Option<String> {
    let album = folder.album.as_deref().filter(|album| !album.is_empty())?;
    let mut query = format!("release:\"{}\"", quoted(album));
    if let Some(artist) = folder.artist.as_deref().filter(|a| !a.is_empty()) {
        query.push_str(&format!(" AND artist:\"{}\"", quoted(artist)));
    }
    Some(query)
}

fn quoted(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// How alike two strings are, 0 to 1, by how many letters must change to make one the other.
fn alike(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 0.0;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let cost = previous + usize::from(x != y);
            previous = row[j + 1];
            row[j + 1] = cost.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    1.0 - row[b.len()] as f64 / longest as f64
}

fn length_fit(file: Option<Duration>, track: Option<Duration>) -> Option<f64> {
    let difference = file?.abs_diff(track?);
    if difference <= SAME_LENGTH {
        return Some(1.0);
    }
    let range = (OTHER_LENGTH - SAME_LENGTH).as_secs_f64();
    Some((1.0 - (difference - SAME_LENGTH).as_secs_f64() / range).max(0.0))
}

/// How well a file fits a track, 0 to 1: what it can tell, its length weighing the most.
fn fit(file: &FileToMatch, track: &ReleaseTrack) -> f64 {
    let mut parts: Vec<(f64, f64)> = Vec::new();
    if let Some(length) = length_fit(file.duration, track.length) {
        parts.push((length, 0.5));
    }
    if let Some(title) = file.title.as_deref().filter(|t| !t.is_empty()) {
        parts.push((alike(&comparable(title), &comparable(&track.title)), 0.35));
    }
    if let Some(number) = file.track {
        let same = number == track.number && file.disc.unwrap_or(track.disc) == track.disc;
        parts.push((f64::from(u8::from(same)), 0.15));
    }
    let weight: f64 = parts.iter().map(|(_, weight)| weight).sum();
    if weight == 0.0 {
        return 0.0;
    }
    parts.iter().map(|(fit, w)| fit * w).sum::<f64>() / weight
}

/// Which track each file is, the file's index and the track's, best fits first; and how well
/// the release fits the folder, 0 to 1.
pub fn pair(folder: &FolderToMatch, release: &Release) -> (Vec<(usize, usize)>, f64) {
    let mut fits: Vec<(f64, usize, usize)> = Vec::new();
    for (f, file) in folder.files.iter().enumerate() {
        for (t, track) in release.tracks.iter().enumerate() {
            fits.push((fit(file, track), f, t));
        }
    }
    fits.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut files_taken = vec![false; folder.files.len()];
    let mut tracks_taken = vec![false; release.tracks.len()];
    let mut pairs = Vec::new();
    let mut total = 0.0;
    for (score, f, t) in fits {
        if score < 0.4 || files_taken[f] || tracks_taken[t] {
            continue;
        }
        files_taken[f] = true;
        tracks_taken[t] = true;
        pairs.push((f, t));
        total += score;
    }
    let most = folder.files.len().max(release.tracks.len());
    let fits = if most == 0 { 0.0 } else { total / most as f64 };
    let bonus = if says_edition(folder, release) {
        EDITION_BONUS
    } else {
        0.0
    };
    (pairs, fits * (1.0 - EDITION_BONUS) + bonus)
}

/// Whether the release says what the folder's edition does: its year, or its words.
fn says_edition(folder: &FolderToMatch, release: &Release) -> bool {
    let Some(edition) = folder.edition.as_deref() else {
        return false;
    };
    let year = release.date.get(..4).unwrap_or("");
    let said = comparable(&format!("{} {}", release.disambiguation, release.title));
    comparable(edition)
        .split(' ')
        .any(|word| (word.len() == 4 && word == year) || (word.len() > 3 && said.contains(word)))
}

/// What the tags of a file should say, for it being the track of the release.
pub fn draft_for(
    file: &FileToMatch,
    track: &ReleaseTrack,
    release: &Release,
    edition: Option<&str>,
) -> TagDraft {
    let total_on_disc = release
        .tracks
        .iter()
        .filter(|t| t.disc == track.disc)
        .count();
    let mut changes = TagMap::new();
    let mut set = |field: TagField, value: String| {
        if !value.is_empty() {
            changes.insert(field, value);
        }
    };
    set(TagField::Title, track.title.clone());
    set(TagField::Artist, track.artist.clone());
    set(TagField::Album, release.title.clone());
    set(TagField::AlbumArtist, release.artist.clone());
    set(TagField::Date, release.date.clone());
    set(TagField::OriginalDate, release.first_date.clone());
    set(TagField::Track, track.number.to_string());
    set(TagField::TrackTotal, total_on_disc.to_string());
    set(TagField::Disc, track.disc.to_string());
    set(TagField::DiscTotal, release.discs.to_string());
    set(TagField::RecordingId, track.recording_id.clone());
    set(TagField::ReleaseTrackId, track.track_id.clone());
    set(TagField::ReleaseId, release.id.clone());
    set(TagField::ReleaseGroupId, release.group_id.clone());
    set(TagField::ArtistId, track.artist_id.clone());
    set(TagField::AlbumArtistId, release.artist_id.clone());
    if let Some(edition) = edition.filter(|e| !e.is_empty()) {
        set(TagField::Edition, edition.to_owned());
    } else {
        set(TagField::Edition, release.disambiguation.clone());
    }
    TagDraft {
        path: file.path.clone(),
        changes,
    }
}

fn drafts_for(
    folder: &FolderToMatch,
    release: &Release,
    pairs: &[(usize, usize)],
) -> Vec<TagDraft> {
    pairs
        .iter()
        .map(|(f, t)| {
            draft_for(
                &folder.files[*f],
                &release.tracks[*t],
                release,
                folder.edition.as_deref(),
            )
        })
        .collect()
}

fn candidate(release: &Release, score: f64) -> ReleaseCandidate {
    ReleaseCandidate {
        release_id: release.id.clone(),
        title: release.title.clone(),
        artist: release.artist.clone(),
        date: release.date.clone(),
        country: release.country.clone(),
        disambiguation: release.disambiguation.clone(),
        label: release.label.clone(),
        format: release.format.clone(),
        tracks: release.tracks.len() as u32,
        score,
    }
}

fn same_recordings(a: &Release, b: &Release) -> bool {
    let recordings = |release: &Release| {
        let mut ids: Vec<String> = release
            .tracks
            .iter()
            .map(|t| t.recording_id.clone())
            .collect();
        ids.sort();
        ids
    };
    recordings(a) == recordings(b)
}

/// A release, which track each file is, and how well it fits.
type Scored = (Release, Vec<(usize, usize)>, f64);

/// Finds which release `folder` is, and what its files' tags should say for that.
pub fn identify(folder: &FolderToMatch, source: &dyn Source) -> Result<Identified, String> {
    let mut ids: Vec<String> = folder
        .release_id
        .iter()
        .chain(&folder.heard_on)
        .cloned()
        .collect();
    if let Some(query) = search_query(folder) {
        let mut found = source.search(&query)?;
        // The same number of tracks first; among those as the search ranks them.
        let files = folder.files.len() as u32;
        found.sort_by_key(|f| (f.tracks != files, std::cmp::Reverse(f.score)));
        ids.extend(found.into_iter().map(|f| f.id));
    }
    let mut seen = HashMap::new();
    ids.retain(|id| seen.insert(id.clone(), ()).is_none());
    ids.truncate(LOOKED_AT);
    let mut scored: Vec<Scored> = Vec::new();
    for id in &ids {
        let release = source.release(id)?;
        let (pairs, score) = pair(folder, &release);
        scored.push((release, pairs, score));
    }
    scored.sort_by(|a, b| b.2.total_cmp(&a.2));
    let candidates = scored.iter().map(|(r, _, s)| candidate(r, *s)).collect();
    let Some((best, pairs, score)) = scored.first() else {
        return Ok(Identified {
            state: ProposalState::None,
            chosen: None,
            candidates,
            drafts: Vec::new(),
        });
    };
    if *score < SUGGESTED {
        return Ok(Identified {
            state: ProposalState::None,
            chosen: None,
            candidates,
            drafts: Vec::new(),
        });
    }
    let unclear = scored.get(1).is_some_and(|(next, _, next_score)| {
        score - next_score < CLOSE
            && (!same_recordings(best, next)
                || (folder.edition.is_some() && !says_edition(folder, best)))
    });
    let state = if *score >= CONFIDENT && !unclear {
        ProposalState::Ready
    } else {
        ProposalState::Review
    };
    Ok(Identified {
        state,
        chosen: Some(best.id.clone()),
        drafts: drafts_for(folder, best, pairs),
        candidates,
    })
}

/// What the files' tags should say for `folder` being the release `id`, the user having chosen.
pub fn choose(
    folder: &FolderToMatch,
    id: &str,
    source: &dyn Source,
) -> Result<Vec<TagDraft>, String> {
    let release = source.release(id)?;
    let (pairs, _) = pair(folder, &release);
    Ok(drafts_for(folder, &release, &pairs))
}

/// The album's title and edition, by its tags or else its folder's name.
pub fn album_and_edition(
    tagged: Option<&str>,
    folder: &freya_protocol::FolderName,
) -> (Option<String>, Option<String>) {
    let edition = folder.edition.clone();
    match tagged.filter(|album| !album.is_empty()) {
        Some(album) => {
            let (title, in_title) = split_edition(album);
            (Some(title), edition.or(in_title))
        }
        None => (
            Some(folder.title.clone()).filter(|t| !t.is_empty()),
            edition,
        ),
    }
}

#[cfg(test)]
mod tests;
