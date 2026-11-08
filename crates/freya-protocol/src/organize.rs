//! Organizing the music's files: what their tags say, and what is to be changed in them.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// A field of a music file's tags.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TagField {
    Title,
    Artist,
    Album,
    AlbumArtist,
    Genre,
    /// When this release came out: a year, or a date as `2016-04-15`.
    Date,
    /// When the music first came out, for a reissue.
    OriginalDate,
    Track,
    TrackTotal,
    Disc,
    DiscTotal,
    /// What tells this edition of the album from its others: "2016 Remastered".
    Edition,
    RecordingId,
    ReleaseTrackId,
    ReleaseId,
    ReleaseGroupId,
    ArtistId,
    AlbumArtistId,
}

impl TagField {
    /// Every field, in the order they are shown.
    pub const ALL: [TagField; 18] = [
        TagField::Title,
        TagField::Artist,
        TagField::Album,
        TagField::AlbumArtist,
        TagField::Edition,
        TagField::Genre,
        TagField::Date,
        TagField::OriginalDate,
        TagField::Track,
        TagField::TrackTotal,
        TagField::Disc,
        TagField::DiscTotal,
        TagField::RecordingId,
        TagField::ReleaseTrackId,
        TagField::ReleaseId,
        TagField::ReleaseGroupId,
        TagField::ArtistId,
        TagField::AlbumArtistId,
    ];

    /// The field's name as a skin knows it.
    pub fn name(self) -> &'static str {
        match self {
            TagField::Title => "title",
            TagField::Artist => "artist",
            TagField::Album => "album",
            TagField::AlbumArtist => "album_artist",
            TagField::Genre => "genre",
            TagField::Date => "date",
            TagField::OriginalDate => "original_date",
            TagField::Track => "track",
            TagField::TrackTotal => "track_total",
            TagField::Disc => "disc",
            TagField::DiscTotal => "disc_total",
            TagField::Edition => "edition",
            TagField::RecordingId => "recording_id",
            TagField::ReleaseTrackId => "release_track_id",
            TagField::ReleaseId => "release_id",
            TagField::ReleaseGroupId => "release_group_id",
            TagField::ArtistId => "artist_id",
            TagField::AlbumArtistId => "album_artist_id",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|field| field.name() == name)
    }

    /// The field's name, as the user reads it.
    pub fn label(self) -> &'static str {
        match self {
            TagField::Title => "Title",
            TagField::Artist => "Artist",
            TagField::Album => "Album",
            TagField::AlbumArtist => "Album artist",
            TagField::Genre => "Genre",
            TagField::Date => "Date",
            TagField::OriginalDate => "Original date",
            TagField::Track => "Track",
            TagField::TrackTotal => "Tracks",
            TagField::Disc => "Disc",
            TagField::DiscTotal => "Discs",
            TagField::Edition => "Edition",
            TagField::RecordingId => "Recording ID",
            TagField::ReleaseTrackId => "Release track ID",
            TagField::ReleaseId => "Release ID",
            TagField::ReleaseGroupId => "Release group ID",
            TagField::ArtistId => "Artist ID",
            TagField::AlbumArtistId => "Album artist ID",
        }
    }

    /// Whether the field is one of the ids MusicBrainz gives, which nobody types.
    pub fn is_id(self) -> bool {
        matches!(
            self,
            TagField::RecordingId
                | TagField::ReleaseTrackId
                | TagField::ReleaseId
                | TagField::ReleaseGroupId
                | TagField::ArtistId
                | TagField::AlbumArtistId
        )
    }
}

/// Fields of a file's tags and their values. As a change, a blank value takes the field away.
pub type TagMap = BTreeMap<TagField, String>;

/// What is to be changed in a file's tags, not yet written to it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TagDraft {
    pub path: PathBuf,
    /// The fields to change, a blank value taking one away. None takes the draft away.
    pub changes: TagMap,
}

/// One time drafts were written to files.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TagSave {
    pub id: i64,
    pub at: SystemTime,
    /// The folder saved, or the first of them.
    pub folder: PathBuf,
    pub files: u32,
    /// Whether it was undone.
    pub undone: bool,
}

/// A file a save or an undo could not write, and why. The file is as it was.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SaveFailure {
    pub path: PathBuf,
    pub why: String,
}

/// How a save or an undo went.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SaveOutcome {
    /// The save it made, to undo it by; none when no file changed.
    pub save: Option<i64>,
    pub saved: u32,
    pub failed: Vec<SaveFailure>,
}

/// How far a folder of music has come.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FolderState {
    /// Nothing is known of which release it is.
    Unidentified,
    /// A release was suggested that the user should look at.
    Review,
    /// Changes wait to be saved.
    Ready,
    /// Its files say which release they are.
    Identified,
}

/// What a folder's name says.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderName {
    pub year: Option<u32>,
    pub title: String,
    pub edition: Option<String>,
}

/// A folder of music, the unit that is organized: an album's folder, with its discs' folders.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct OrganizeFolder {
    pub path: PathBuf,
    /// The folder it is in, by name: most often the artist's.
    pub group: String,
    /// The folder's own name.
    pub name: String,
    pub parsed: FolderName,
    pub files: u32,
    pub state: FolderState,
    /// Whether its files are of different albums, not one album's.
    pub mixed: bool,
}

/// A file of a folder, as its tags are on disk.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct OrganizeFile {
    pub path: PathBuf,
    pub fields: TagMap,
    /// What is to be changed in it.
    pub draft: TagMap,
    pub duration: Option<Duration>,
    /// Why tags cannot be written in it, if so.
    pub not_taggable: Option<String>,
}

/// One of the editions of an album.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AlbumEdition {
    /// The folder it is in, which it is chosen by.
    pub folder: PathBuf,
    /// What tells it from the others: "2016 Remastered".
    pub label: String,
    pub tracks: u32,
    /// When it came out.
    pub year: Option<u32>,
    /// Whether it is the one the album shows.
    pub preferred: bool,
}

/// A release a folder may be.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ReleaseCandidate {
    pub release_id: String,
    pub title: String,
    pub artist: String,
    /// When it came out, as MusicBrainz gives it.
    pub date: String,
    pub country: String,
    /// What MusicBrainz says tells it from others: "2016 remaster".
    pub disambiguation: String,
    pub label: String,
    pub format: String,
    pub tracks: u32,
    /// How well it fits the folder, 0 to 1.
    pub score: f64,
}

/// What came of asking which release a folder is.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ProposalState {
    /// Nothing fit well enough.
    None,
    /// One fits best, but the user should look at it.
    Review,
    /// One fits well enough to save.
    Ready,
}

/// The releases a folder may be, the best first, and the one its drafts are of.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FolderProposal {
    pub folder: PathBuf,
    pub state: ProposalState,
    pub chosen: Option<String>,
    pub candidates: Vec<ReleaseCandidate>,
}

/// What the server is doing with the folders it was asked to go through.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct OrganizeStatus {
    /// Folders waiting, the one being worked on among them.
    pub waiting: u32,
    /// Folders done since it last had nothing to do.
    pub done: u32,
    /// The folder being worked on.
    pub current: Option<PathBuf>,
    /// What it could not do most lately.
    pub error: Option<String>,
}

/// The file name pattern suggested: an artist's folder, with albums by the year they first
/// came out.
pub const SUGGESTED_PATTERN: &str =
    "{album_artist}/({original_year}) {album}[ ({edition})]/[{disc}-]{track} {title}";

/// A file to go somewhere else, by a file name pattern.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PlannedMove {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Why it cannot go there; a folder with one of these is left as it is.
    pub conflict: Option<String>,
    /// Whether it goes along with the music: a cover, a log, a CUE sheet.
    pub companion: bool,
}
