use std::path::{Path, PathBuf};

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use super::*;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../freya-decode/tests/fixtures")
        .join(name)
}

fn plan(name: &str, quality: Quality) -> Option<Plan> {
    let path = fixture(name);
    let extension = path.extension().and_then(|e| e.to_str()).map(str::to_owned);
    Plan::new(File::open(&path).unwrap(), extension.as_deref(), quality).unwrap()
}

/// What `bytes` decode to: their rate, channels and samples, interleaved.
fn decode(bytes: Vec<u8>) -> (u32, usize, Vec<f32>) {
    let stream = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("opus");
    let mut format = PROBE
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .unwrap();
    let track = format.default_track(TrackType::Audio).unwrap();
    let params = track
        .codec_params
        .as_ref()
        .unwrap()
        .audio()
        .unwrap()
        .clone();
    let id = track.id;
    let mut decoder = CODECS
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .unwrap();
    let (mut rate, mut channels, mut all) = (0, 0, Vec::new());
    while let Some(packet) = format.next_packet().unwrap() {
        if packet.track_id != id {
            continue;
        }
        let decoded = decoder.decode(&packet).unwrap();
        rate = decoded.spec().rate();
        channels = decoded.spec().channels().count();
        let mut samples: Vec<f32> = Vec::new();
        decoded.copy_to_vec_interleaved(&mut samples);
        all.extend(samples);
    }
    (rate, channels, all)
}

/// The frames, rate and channels of a fixture as decoded.
fn source(name: &str) -> (u64, u32, usize) {
    let path = fixture(name);
    let mut decoding = Decoding::open(
        File::open(&path).unwrap(),
        path.extension().and_then(|e| e.to_str()),
    )
    .unwrap();
    let (rate, channels) = (decoding.rate, decoding.channels);
    let mut frames = 0;
    let mut samples = Vec::new();
    while decoding.next(&mut samples).unwrap() {
        frames += (samples.len() / channels) as u64;
    }
    (frames, rate, channels)
}

#[test]
fn a_file_is_as_long_as_reckoned_and_plays_as_long_as_it_did() {
    for (name, quality) in [
        ("sine.wav", Quality::High),
        ("sine.wav", Quality::Normal),
        ("sine-96k-24bit.wav", Quality::High),
        ("mono-22k.wav", Quality::Normal),
        ("sine.flac", Quality::Normal),
        ("ape/stereo-16-normal.ape", Quality::High),
    ] {
        let plan = plan(name, quality).expect("a lossless file is made smaller");
        let reckoned = plan.length();
        let mut out = Vec::new();
        plan.encode(&mut out).unwrap();
        assert_eq!(out.len() as u64, reckoned, "{name} at {quality:?}");

        let (frames, rate, channels) = source(name);
        let (opus_rate, opus_channels, samples) = decode(out);
        assert_eq!(opus_rate, 48_000);
        assert_eq!(opus_channels, channels.min(2), "{name}");
        let played = (samples.len() / opus_channels) as f64 / 48_000.0;
        let was = frames as f64 / f64::from(rate);
        // Within a packet: the decoder may give the last one whole.
        assert!(
            (played - was).abs() < 0.021,
            "{name}: {played} s against {was} s"
        );
    }
}

#[test]
fn the_music_is_where_it_was() {
    // A sine at the start is a sine at the start: as loud, and not shifted by the encoder's
    // lookahead, which the decoder skips.
    let plan = plan("sine.wav", Quality::High).unwrap();
    let mut out = Vec::new();
    plan.encode(&mut out).unwrap();
    let (_, channels, samples) = decode(out);
    let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
    let middle = samples.len() / 2;
    let opus = rms(&samples[middle..middle + 4800 * channels]);

    let path = fixture("sine.wav");
    let mut decoding = Decoding::open(File::open(&path).unwrap(), Some("wav")).unwrap();
    let mut wav = Vec::new();
    let mut part = Vec::new();
    while decoding.next(&mut part).unwrap() {
        wav.extend_from_slice(&part);
    }
    let middle = wav.len() / 2;
    let original = rms(&wav[middle..middle + 4410 * decoding.channels]);
    assert!(
        (opus - original).abs() < original * 0.1,
        "{opus} against {original}"
    );
}

#[test]
fn the_same_file_encodes_to_the_same_bytes() {
    let encode = || {
        let mut out = Vec::new();
        plan("sine.wav", Quality::Normal)
            .unwrap()
            .encode(&mut out)
            .unwrap();
        out
    };
    assert!(encode() == encode());
}

#[test]
fn a_file_already_small_is_not_made_smaller() {
    // The MP3 is 128 kbit/s, the Vorbis less than 96.
    assert!(plan("sine.mp3", Quality::High).is_none());
    assert!(plan("sine.ogg", Quality::Normal).is_none());
    // Though the MP3 is a third larger than 96 kbit/s, not half.
    assert!(plan("sine.mp3", Quality::Normal).is_none());
}

#[test]
fn qualities_are_named_as_asked_for() {
    for quality in Quality::ALL {
        assert_eq!(Quality::from_name(quality.name()), Some(quality));
    }
    assert_eq!(Quality::from_name("original"), None);
    assert_eq!(Quality::High.bytes_per_second(), 20_000);
    assert_eq!(Quality::Normal.bytes_per_second(), 12_000);
}
