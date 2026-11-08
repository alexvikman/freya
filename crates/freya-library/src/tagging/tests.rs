use std::path::PathBuf;

use freya_decode::{CODECS, PROBE};
use lofty::ape::{ApeFile, ApeItem};
use lofty::flac::FlacFile;
use lofty::id3::v2::Id3v2Version;
use lofty::mp4::{Atom, AtomData, AtomIdent, Mp4File};
use lofty::mpeg::MpegFile;
use lofty::ogg::{OggPictureStorage, OpusFile, VorbisFile};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::tag::ItemValue;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use super::*;

/// Every format among the fixtures that tags can be written in.
const FORMATS: [&str; 8] = [
    "sine.mp3",
    "sine.flac",
    "sine.ogg",
    "sine.opus",
    "sine.wav",
    "sine-aac.m4a",
    "sine-alac.m4a",
    "sine.ape",
];

struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("freya-tagging-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn copy(&self, fixture: &str) -> PathBuf {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../freya-decode/tests/fixtures")
            .join(fixture);
        let path = self.0.join(fixture);
        fs::copy(source, &path).unwrap();
        path
    }

    /// The names of what is in the folder, hidden files too.
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn all_fields() -> TagMap {
    let mut fields = TagMap::new();
    for (field, value) in [
        (TagField::Title, "Battery"),
        (TagField::Artist, "Metallica"),
        (TagField::Album, "Master of Puppets"),
        (TagField::AlbumArtist, "Metallica"),
        (TagField::Genre, "Thrash Metal"),
        (TagField::Date, "2016"),
        (TagField::OriginalDate, "1986"),
        (TagField::Track, "1"),
        (TagField::TrackTotal, "8"),
        (TagField::Disc, "1"),
        (TagField::DiscTotal, "1"),
        (TagField::Edition, "2016 Remastered"),
        (
            TagField::RecordingId,
            "11111111-1111-4111-8111-111111111111",
        ),
        (
            TagField::ReleaseTrackId,
            "22222222-2222-4222-8222-222222222222",
        ),
        (TagField::ReleaseId, "33333333-3333-4333-8333-333333333333"),
        (
            TagField::ReleaseGroupId,
            "44444444-4444-4444-8444-444444444444",
        ),
        (TagField::ArtistId, "55555555-5555-4555-8555-555555555555"),
        (
            TagField::AlbumArtistId,
            "66666666-6666-4666-8666-666666666666",
        ),
    ] {
        fields.insert(field, value.to_owned());
    }
    fields
}

/// The file's samples, decoded, each as its bits, and how many there are.
fn decoded(path: &Path) -> Vec<u32> {
    let file = File::open(path).unwrap();
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut format = PROBE
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .unwrap();
    let track = format.default_track(TrackType::Audio).unwrap();
    let id = track.id;
    let params = track.codec_params.as_ref().unwrap().audio().unwrap();
    let mut decoder = CODECS
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .unwrap();
    let mut samples = Vec::new();
    while let Ok(Some(packet)) = format.next_packet() {
        if packet.track_id != id {
            continue;
        }
        let audio = decoder.decode(&packet).unwrap();
        let mut read = vec![0.0f32; audio.samples_interleaved()];
        audio.copy_to_slice_interleaved(&mut read);
        samples.extend(read.iter().map(|s| s.to_bits()));
    }
    samples
}

#[test]
fn every_field_reads_back_in_every_format() {
    let folder = Folder::new("fields");
    let fields = all_fields();
    for name in FORMATS {
        let path = folder.copy(name);
        write_fields(&path, &fields).unwrap_or_else(|e| panic!("{name}: {e}"));
        let read = read_fields(&path).unwrap();
        for (field, value) in &fields {
            assert_eq!(read.get(field), Some(value), "{name}: {field:?}");
        }
    }
}

#[test]
fn the_music_is_the_same_sample_for_sample() {
    let folder = Folder::new("music");
    for name in FORMATS {
        let path = folder.copy(name);
        let before = decoded(&path);
        assert!(!before.is_empty(), "{name}");
        write_fields(&path, &all_fields()).unwrap();
        assert!(decoded(&path) == before, "{name}");
    }
}

#[test]
fn only_what_was_asked_changes() {
    let folder = Folder::new("only");
    for name in FORMATS {
        let path = folder.copy(name);
        write_fields(&path, &all_fields()).unwrap();
        let before = read_fields(&path).unwrap();
        let change = TagMap::from([(TagField::Title, "Welcome Home".to_owned())]);
        write_fields(&path, &change).unwrap();
        let mut wanted = before;
        wanted.insert(TagField::Title, "Welcome Home".to_owned());
        assert_eq!(read_fields(&path).unwrap(), wanted, "{name}");
    }
}

#[test]
fn a_blank_value_takes_the_field_away() {
    let folder = Folder::new("blank");
    for name in FORMATS {
        let path = folder.copy(name);
        write_fields(&path, &all_fields()).unwrap();
        let change = TagMap::from([(TagField::Genre, String::new())]);
        write_fields(&path, &change).unwrap();
        assert_eq!(
            read_fields(&path).unwrap().get(&TagField::Genre),
            None,
            "{name}"
        );
    }
}

const TITLE_ONLY: fn() -> TagMap = || TagMap::from([(TagField::Title, "Changed".to_owned())]);

#[test]
fn fields_freya_has_no_name_for_stay_in_id3() {
    let folder = Folder::new("id3");
    let path = folder.copy("sine.mp3");
    let parse = ParseOptions::new();
    let mut file = File::options().read(true).write(true).open(&path).unwrap();
    let mut audio = <MpegFile as AudioFile>::read_from(&mut file, parse).unwrap();
    let tag = audio.id3v2_mut().unwrap();
    tag.insert_user_text("KEEP ME".to_owned(), "kept".to_owned());
    file.rewind().unwrap();
    audio.save_to(&mut file, WriteOptions::default()).unwrap();
    drop(file);

    write_fields(&path, &TITLE_ONLY()).unwrap();

    let mut file = File::open(&path).unwrap();
    let audio = <MpegFile as AudioFile>::read_from(&mut file, parse).unwrap();
    assert_eq!(
        audio.id3v2().unwrap().get_user_text("KEEP ME"),
        Some("kept")
    );
}

#[test]
fn a_tag_of_the_older_id3_stays_the_older() {
    let folder = Folder::new("id3v23");
    let path = folder.copy("sine.mp3");
    let parse = ParseOptions::new();
    let mut file = File::options().read(true).write(true).open(&path).unwrap();
    let audio = <MpegFile as AudioFile>::read_from(&mut file, parse).unwrap();
    let mut write = WriteOptions::default();
    write.use_id3v23(true);
    file.rewind().unwrap();
    audio.save_to(&mut file, write).unwrap();
    drop(file);

    write_fields(&path, &TITLE_ONLY()).unwrap();

    let mut file = File::open(&path).unwrap();
    let audio = <MpegFile as AudioFile>::read_from(&mut file, parse).unwrap();
    assert_eq!(audio.id3v2().unwrap().original_version(), Id3v2Version::V3);
}

#[test]
fn fields_freya_has_no_name_for_stay_in_vorbis_comments() {
    let folder = Folder::new("vorbis");
    let parse = ParseOptions::new();

    let path = folder.copy("sine.flac");
    let mut file = File::options().read(true).write(true).open(&path).unwrap();
    let mut audio = <FlacFile as AudioFile>::read_from(&mut file, parse).unwrap();
    audio
        .vorbis_comments_mut()
        .unwrap()
        .insert("KEEPME".to_owned(), "kept".to_owned());
    file.rewind().unwrap();
    audio.save_to(&mut file, WriteOptions::default()).unwrap();
    drop(file);
    write_fields(&path, &TITLE_ONLY()).unwrap();
    let mut file = File::open(&path).unwrap();
    let audio = <FlacFile as AudioFile>::read_from(&mut file, parse).unwrap();
    assert_eq!(audio.vorbis_comments().unwrap().get("KEEPME"), Some("kept"));

    let path = folder.copy("sine.ogg");
    let mut file = File::options().read(true).write(true).open(&path).unwrap();
    let mut audio = <VorbisFile as AudioFile>::read_from(&mut file, parse).unwrap();
    audio
        .vorbis_comments_mut()
        .insert("KEEPME".to_owned(), "kept".to_owned());
    file.rewind().unwrap();
    audio.save_to(&mut file, WriteOptions::default()).unwrap();
    drop(file);
    write_fields(&path, &TITLE_ONLY()).unwrap();
    let mut file = File::open(&path).unwrap();
    let audio = <VorbisFile as AudioFile>::read_from(&mut file, parse).unwrap();
    assert_eq!(audio.vorbis_comments().get("KEEPME"), Some("kept"));

    let path = folder.copy("sine.opus");
    let mut file = File::options().read(true).write(true).open(&path).unwrap();
    let mut audio = <OpusFile as AudioFile>::read_from(&mut file, parse).unwrap();
    audio
        .vorbis_comments_mut()
        .insert("KEEPME".to_owned(), "kept".to_owned());
    file.rewind().unwrap();
    audio.save_to(&mut file, WriteOptions::default()).unwrap();
    drop(file);
    write_fields(&path, &TITLE_ONLY()).unwrap();
    let mut file = File::open(&path).unwrap();
    let audio = <OpusFile as AudioFile>::read_from(&mut file, parse).unwrap();
    assert_eq!(audio.vorbis_comments().get("KEEPME"), Some("kept"));
}

#[test]
fn fields_freya_has_no_name_for_stay_in_mp4() {
    let folder = Folder::new("mp4");
    let parse = ParseOptions::new();
    let path = folder.copy("sine-aac.m4a");
    let ident = AtomIdent::Freeform {
        mean: "com.example".into(),
        name: "keep".into(),
    };
    let mut file = File::options().read(true).write(true).open(&path).unwrap();
    let mut audio = <Mp4File as AudioFile>::read_from(&mut file, parse).unwrap();
    let mut tag = audio.ilst_mut().map(std::mem::take).unwrap_or_default();
    tag.insert(Atom::new(ident.clone(), AtomData::UTF8("kept".to_owned())));
    audio.set_ilst(tag);
    file.rewind().unwrap();
    audio.save_to(&mut file, WriteOptions::default()).unwrap();
    drop(file);

    write_fields(&path, &TITLE_ONLY()).unwrap();

    let mut file = File::open(&path).unwrap();
    let audio = <Mp4File as AudioFile>::read_from(&mut file, parse).unwrap();
    let kept = audio.ilst().unwrap().get(&ident).unwrap();
    assert!(matches!(kept.data().next(), Some(AtomData::UTF8(text)) if text == "kept"));
}

#[test]
fn fields_freya_has_no_name_for_stay_in_ape() {
    let folder = Folder::new("ape");
    let parse = ParseOptions::new();
    let path = folder.copy("sine.ape");
    let mut file = File::options().read(true).write(true).open(&path).unwrap();
    let mut audio = <ApeFile as AudioFile>::read_from(&mut file, parse).unwrap();
    audio
        .ape_mut()
        .unwrap()
        .insert(ApeItem::new("KEEPME".to_owned(), ItemValue::Text("kept".to_owned())).unwrap());
    file.rewind().unwrap();
    audio.save_to(&mut file, WriteOptions::default()).unwrap();
    drop(file);

    write_fields(&path, &TITLE_ONLY()).unwrap();

    let mut file = File::open(&path).unwrap();
    let audio = <ApeFile as AudioFile>::read_from(&mut file, parse).unwrap();
    assert!(audio.ape().unwrap().get("KEEPME").is_some());
}

#[test]
fn pictures_stay() {
    let folder = Folder::new("pictures");
    let parse = ParseOptions::new();
    let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4];
    let picture = || {
        Picture::unchecked(jpeg.clone())
            .pic_type(PictureType::CoverFront)
            .mime_type(MimeType::Jpeg)
            .build()
    };

    let path = folder.copy("sine.mp3");
    let mut file = File::options().read(true).write(true).open(&path).unwrap();
    let mut audio = <MpegFile as AudioFile>::read_from(&mut file, parse).unwrap();
    audio.id3v2_mut().unwrap().insert_picture(picture());
    file.rewind().unwrap();
    audio.save_to(&mut file, WriteOptions::default()).unwrap();
    drop(file);
    write_fields(&path, &TITLE_ONLY()).unwrap();
    let read = lofty::read_from_path(&path).unwrap();
    assert_eq!(read.primary_tag().unwrap().pictures().len(), 1, "mp3");

    let path = folder.copy("sine.flac");
    let mut file = File::options().read(true).write(true).open(&path).unwrap();
    let mut audio = <FlacFile as AudioFile>::read_from(&mut file, parse).unwrap();
    audio
        .insert_picture(picture(), None)
        .expect("a picture FLAC can hold");
    file.rewind().unwrap();
    audio.save_to(&mut file, WriteOptions::default()).unwrap();
    drop(file);
    write_fields(&path, &TITLE_ONLY()).unwrap();
    let mut file = File::open(&path).unwrap();
    let audio = <FlacFile as AudioFile>::read_from(&mut file, parse).unwrap();
    assert_eq!(audio.pictures().len(), 1, "flac");
}

#[test]
fn leaves_nothing_beside_the_file() {
    let folder = Folder::new("leftovers");
    let path = folder.copy("sine.flac");
    write_fields(&path, &TITLE_ONLY()).unwrap();
    assert_eq!(folder.names(), ["sine.flac"]);
}

#[test]
fn a_file_that_is_not_music_is_left_as_it_was() {
    let folder = Folder::new("not-music");
    let path = folder.0.join("song.mp3");
    fs::write(&path, b"this is not an mp3 at all").unwrap();
    assert!(write_fields(&path, &TITLE_ONLY()).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"this is not an mp3 at all");
    assert_eq!(folder.names(), ["song.mp3"]);
}

#[test]
fn says_when_a_file_cannot_be_tagged() {
    let folder = Folder::new("untaggable");
    let path = folder.0.join("a.webm");
    fs::write(&path, b"\x1a\x45\xdf\xa3 not much").unwrap();
    assert!(why_not_taggable(&path).is_some());
    assert_eq!(why_not_taggable(&folder.copy("sine.flac")), None);
}
