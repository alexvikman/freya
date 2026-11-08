//! Writing tags into music files. A file is never written in place: a copy beside it is, is
//! read back to see that it says what it should and is still the same music, and then takes
//! the file's place in one step. Only the fields asked for change; everything else in the tags
//! stays, pictures and fields Freya has no name for too.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek};
use std::path::{Path, PathBuf};
use std::time::Duration;

use freya_protocol::{TagField, TagMap};
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::prelude::ItemKey;
use lofty::probe::Probe;
use lofty::tag::{MergeTag, SplitTag, Tag};

/// How far a file's length may be from what it was, after only its tags changed.
const LENGTH_TOLERANCE: Duration = Duration::from_millis(100);

/// Why a file's tags could not be written. The file is as it was.
#[derive(Debug)]
pub enum WriteError {
    /// Not a kind of file Freya can write tags in.
    NotTaggable(String),
    Io(io::Error),
    Tags(String),
    /// What was written did not read back as it should.
    Mismatch(String),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::NotTaggable(why) => f.write_str(why),
            WriteError::Io(e) => write!(f, "{e}"),
            WriteError::Tags(e) => write!(f, "{e}"),
            WriteError::Mismatch(why) => write!(f, "the written file was not right: {why}"),
        }
    }
}

impl std::error::Error for WriteError {}

impl From<io::Error> for WriteError {
    fn from(e: io::Error) -> Self {
        WriteError::Io(e)
    }
}

impl From<lofty::error::FileParseError> for WriteError {
    fn from(e: lofty::error::FileParseError) -> Self {
        WriteError::Tags(e.to_string())
    }
}

impl From<lofty::error::FileEncodingError> for WriteError {
    fn from(e: lofty::error::FileEncodingError) -> Self {
        WriteError::Tags(e.to_string())
    }
}

/// The key a field has in the tags of any format. An edition is kept as the set's subtitle,
/// the one field every format has for words about the release as a whole.
fn key_of(field: TagField) -> ItemKey {
    match field {
        TagField::Title => ItemKey::TrackTitle,
        TagField::Artist => ItemKey::TrackArtist,
        TagField::Album => ItemKey::AlbumTitle,
        TagField::AlbumArtist => ItemKey::AlbumArtist,
        TagField::Genre => ItemKey::Genre,
        TagField::Date => ItemKey::RecordingDate,
        TagField::OriginalDate => ItemKey::OriginalReleaseDate,
        TagField::Track => ItemKey::TrackNumber,
        TagField::TrackTotal => ItemKey::TrackTotal,
        TagField::Disc => ItemKey::DiscNumber,
        TagField::DiscTotal => ItemKey::DiscTotal,
        TagField::Edition => ItemKey::SetSubtitle,
        TagField::RecordingId => ItemKey::MusicBrainzRecordingId,
        TagField::ReleaseTrackId => ItemKey::MusicBrainzTrackId,
        TagField::ReleaseId => ItemKey::MusicBrainzReleaseId,
        TagField::ReleaseGroupId => ItemKey::MusicBrainzReleaseGroupId,
        TagField::ArtistId => ItemKey::MusicBrainzArtistId,
        TagField::AlbumArtistId => ItemKey::MusicBrainzReleaseArtistId,
    }
}

/// Every field the file's tags have, or none when it cannot be read.
pub fn read_fields(path: &Path) -> Option<TagMap> {
    let options = ParseOptions::new().read_cover_art(false);
    let file = Probe::open(path).ok()?.options(options).read().ok()?;
    let mut fields = TagMap::new();
    let Some(tag) = file.primary_tag().or_else(|| file.first_tag()) else {
        return Some(fields);
    };
    for field in TagField::ALL {
        let value = tag.get_string(key_of(field)).map(str::trim).unwrap_or("");
        if !value.is_empty() {
            fields.insert(field, value.to_owned());
        }
    }
    Some(fields)
}

/// Why tags cannot be written in the file at `path`, if so. Asked without reading all of it.
pub fn why_not_taggable(path: &Path) -> Option<String> {
    let kind = Probe::open(path)
        .ok()
        .and_then(|probe| probe.guess_file_type().ok())
        .and_then(|probe| probe.file_type());
    match kind {
        Some(kind) if can_write(kind) => None,
        _ => Some(format!(
            "Freya cannot write tags in .{} files",
            path.extension().unwrap_or_default().to_string_lossy()
        )),
    }
}

fn can_write(kind: FileType) -> bool {
    matches!(
        kind,
        FileType::Aac
            | FileType::Aiff
            | FileType::Ape
            | FileType::Flac
            | FileType::Mpc
            | FileType::Mpeg
            | FileType::Mp4
            | FileType::Opus
            | FileType::Speex
            | FileType::Vorbis
            | FileType::Wav
            | FileType::WavPack
    )
}

/// Changes `changes` in the tags of the file at `path`, a blank value taking the field away.
/// On an error the file is as it was.
pub fn write_fields(path: &Path, changes: &TagMap) -> Result<(), WriteError> {
    if let Some(why) = why_not_taggable(path) {
        return Err(WriteError::NotTaggable(why));
    }
    let copy = Copy::of(path)?;
    write_in_place(copy.path(), changes)?;
    verify(path, copy.path(), changes)?;
    copy.take_place_of(path)
}

/// A copy of a file, hidden beside it so scans pass it by and it is on the same disk, gone
/// again unless it took the file's place.
struct Copy {
    path: PathBuf,
    kept: bool,
}

impl Copy {
    fn of(path: &Path) -> io::Result<Self> {
        // With the file's extension still last: that is how its format is told.
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let extension = path.extension().unwrap_or_default().to_string_lossy();
        let copy = path.with_file_name(format!(".{name}.freya-{}.{extension}", std::process::id()));
        let this = Self {
            path: copy,
            kept: false,
        };
        fs::copy(path, &this.path)?;
        Ok(this)
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn take_place_of(mut self, path: &Path) -> Result<(), WriteError> {
        File::open(&self.path)?.sync_all()?;
        fs::rename(&self.path, path)?;
        self.kept = true;
        Ok(())
    }
}

impl Drop for Copy {
    fn drop(&mut self) {
        if !self.kept {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// `tag` with `changes` made in it, and all it holds that a [`Tag`] has no place for as it was.
fn edited<T>(tag: T, changes: &TagMap) -> T
where
    T: SplitTag,
    T::Remainder: MergeTag<Merged = T>,
{
    let (rest, mut generic) = tag.split_tag();
    apply(&mut generic, changes);
    rest.merge_tag(generic)
}

fn apply(tag: &mut Tag, changes: &TagMap) {
    for (field, value) in changes {
        let key = key_of(*field);
        tag.remove_key(key);
        let value = value.trim();
        if !value.is_empty() {
            tag.insert_text(key, value.to_owned());
        }
    }
}

fn write_in_place(path: &Path, changes: &TagMap) -> Result<(), WriteError> {
    let kind = Probe::open(path)?
        .guess_file_type()?
        .file_type()
        .filter(|kind| can_write(*kind))
        .ok_or_else(|| WriteError::NotTaggable("not a kind of file with tags".into()))?;
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let parse = ParseOptions::new();
    let write = WriteOptions::default();

    /// A file of format `$format` whose tag is at `$tag`, made when it has none.
    macro_rules! rewrite {
        ($format:ty, $tag:ident, $set:ident) => {{
            let mut audio = <$format as AudioFile>::read_from(&mut file, parse)?;
            let tag = audio.$tag().map(std::mem::take).unwrap_or_default();
            audio.$set(edited(tag, changes));
            file.rewind()?;
            audio.save_to(&mut file, write)?;
        }};
    }
    /// As `rewrite!`, of a format whose files always have the tag.
    macro_rules! rewrite_always {
        ($format:ty, $tag:ident, $set:ident) => {{
            let mut audio = <$format as AudioFile>::read_from(&mut file, parse)?;
            let tag = std::mem::take(audio.$tag());
            audio.$set(edited(tag, changes));
            file.rewind()?;
            audio.save_to(&mut file, write)?;
        }};
    }

    match kind {
        FileType::Mpeg => {
            let mut audio = <lofty::mpeg::MpegFile as AudioFile>::read_from(&mut file, parse)?;
            let tag = audio.id3v2_mut().map(std::mem::take).unwrap_or_default();
            // As the file has it, so a player that reads only ID3v2.3 goes on reading it;
            // unless it is given an edition, whose frame only ID3v2.4 has.
            let older = tag.original_version() == lofty::id3::v2::Id3v2Version::V3
                && !changes.contains_key(&TagField::Edition);
            let mut write = write;
            write.use_id3v23(older);
            audio.set_id3v2(edited(tag, changes));
            file.rewind()?;
            audio.save_to(&mut file, write)?;
        }
        FileType::Aac => rewrite!(lofty::aac::AacFile, id3v2_mut, set_id3v2),
        FileType::Aiff => rewrite!(lofty::iff::aiff::AiffFile, id3v2_mut, set_id3v2),
        FileType::Wav => rewrite!(lofty::iff::wav::WavFile, id3v2_mut, set_id3v2),
        FileType::Flac => rewrite!(
            lofty::flac::FlacFile,
            vorbis_comments_mut,
            set_vorbis_comments
        ),
        FileType::Mp4 => rewrite!(lofty::mp4::Mp4File, ilst_mut, set_ilst),
        FileType::Ape => rewrite!(lofty::ape::ApeFile, ape_mut, set_ape),
        FileType::Mpc => rewrite!(lofty::musepack::MpcFile, ape_mut, set_ape),
        FileType::WavPack => rewrite!(lofty::wavpack::WavPackFile, ape_mut, set_ape),
        FileType::Opus => rewrite_always!(
            lofty::ogg::OpusFile,
            vorbis_comments_mut,
            set_vorbis_comments
        ),
        FileType::Vorbis => rewrite_always!(
            lofty::ogg::VorbisFile,
            vorbis_comments_mut,
            set_vorbis_comments
        ),
        FileType::Speex => rewrite_always!(
            lofty::ogg::SpeexFile,
            vorbis_comments_mut,
            set_vorbis_comments
        ),
        _ => {
            return Err(WriteError::NotTaggable(
                "not a kind of file with tags".into(),
            ));
        }
    }
    Ok(())
}

/// Whether the copy at `written` says what it should, and is still the same music as `original`
/// by what its headers tell: its length, rate and channels.
fn verify(original: &Path, written: &Path, changes: &TagMap) -> Result<(), WriteError> {
    let read = |path: &Path| -> Result<_, WriteError> {
        let options = ParseOptions::new().read_cover_art(false);
        Ok(Probe::open(path)?.options(options).read()?)
    };
    let before = read(original)?;
    let after = read(written)?;
    let (before, after) = (before.properties(), after.properties());
    if before.sample_rate() != after.sample_rate() || before.channels() != after.channels() {
        return Err(WriteError::Mismatch(
            "its sample rate or channels changed".into(),
        ));
    }
    if before.duration().abs_diff(after.duration()) > LENGTH_TOLERANCE {
        return Err(WriteError::Mismatch("its length changed".into()));
    }
    let fields = read_fields(written)
        .ok_or_else(|| WriteError::Mismatch("its tags could not be read".into()))?;
    for (field, value) in changes {
        let wanted = value.trim();
        let got = fields.get(field).map(String::as_str).unwrap_or("");
        if !same_value(*field, wanted, got) {
            return Err(WriteError::Mismatch(format!(
                "{} reads \"{got}\", not \"{wanted}\"",
                field.label()
            )));
        }
    }
    Ok(())
}

/// Whether what was read of a field is what was written. Formats keep numbers as numbers, so
/// "03" reads back as "3".
fn same_value(field: TagField, wanted: &str, got: &str) -> bool {
    let numeric = matches!(
        field,
        TagField::Track | TagField::TrackTotal | TagField::Disc | TagField::DiscTotal
    );
    match (numeric, wanted.parse::<u32>(), got.parse::<u32>()) {
        (true, Ok(wanted), Ok(got)) => wanted == got,
        _ => wanted == got,
    }
}

#[cfg(test)]
mod tests;
