//! Organizing the music's files: what their tags say, and what is to be changed in them.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::SystemTime;

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
