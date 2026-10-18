//! The fixtures in `tests/fixtures/ape` were made with another encoder from the signal below
//! and from the WAV fixtures; see the README there.

use std::fs::File;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Timestamp;

use super::ApeDecoder;
use super::header::Header;
use crate::source::{CODECS, PROBE};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The synthetic test signal: two triangle waves and some noise, in integers.
fn signal(frame: u32, channel: u32, bits: u32) -> i32 {
    let period = 97 + 54 * channel;
    let t = (frame % period) as i32;
    let amp = 1i32 << (bits - 3);
    let triangle = (2 * t - period as i32).abs() * 2 * amp / period as i32 - amp;
    let mut x = (frame * 2 + channel + 1).wrapping_mul(0x9E37_79B9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 13;
    let spread = 1u32 << (bits - 6);
    triangle + (x % spread) as i32 - (spread / 2) as i32
}

/// The synthetic fixtures are 6000 frames long, in frames of 2048.
const FRAMES: u32 = 6000;
const BLOCKS_PER_FRAME: u32 = 2048;

fn open_source(
    source: Box<dyn MediaSource>,
    ext: &str,
) -> (Box<dyn FormatReader>, Box<dyn AudioDecoder>) {
    let mss = MediaSourceStream::new(source, Default::default());
    let mut hint = Hint::new();
    hint.with_extension(ext);
    let format = PROBE
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .unwrap();
    let track = format.default_track(TrackType::Audio).unwrap();
    let params = track.codec_params.as_ref().unwrap().audio().unwrap();
    let decoder = CODECS
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .unwrap();
    (format, decoder)
}

fn open(path: &Path) -> (Box<dyn FormatReader>, Box<dyn AudioDecoder>) {
    let ext = path.extension().unwrap().to_str().unwrap();
    open_source(Box::new(File::open(path).unwrap()), ext)
}

/// Decodes a whole file into interleaved samples; every APE frame is checked against its CRC.
fn decode(format: &mut dyn FormatReader, decoder: &mut dyn AudioDecoder) -> Vec<i32> {
    let mut samples = Vec::new();
    while let Some(packet) = format.next_packet().unwrap() {
        let mut block: Vec<i32> = Vec::new();
        decoder
            .decode(&packet)
            .unwrap_or_else(|e| panic!("packet at {:?}: {e}", packet.pts))
            .copy_to_vec_interleaved(&mut block);
        samples.extend(block);
    }
    samples
}

fn decode_file(path: &Path) -> Vec<i32> {
    let (mut format, mut decoder) = open(path);
    decode(format.as_mut(), decoder.as_mut())
}

/// The synthetic signal, interleaved and scaled to 32 bits as decoders return it.
fn expected(channels: u32, bits: u32) -> Vec<i32> {
    (0..FRAMES)
        .flat_map(|f| (0..channels).map(move |c| signal(f, c, bits) << (32 - bits)))
        .collect()
}

#[test]
fn decodes_every_compression_level_exactly() {
    for level in ["fast", "normal", "high", "extra-high", "insane"] {
        let samples = decode_file(&fixture(&format!("ape/stereo-16-{level}.ape")));
        assert!(samples == expected(2, 16), "{level}");
    }
}

#[test]
fn decodes_every_bit_depth_exactly() {
    for (name, channels, bits) in [
        ("stereo-24-insane", 2, 24),
        ("stereo-8-high", 2, 8),
        ("mono-8-normal", 1, 8),
        ("mono-24-fast", 1, 24),
    ] {
        let samples = decode_file(&fixture(&format!("ape/{name}.ape")));
        assert!(samples == expected(channels, bits), "{name}");
    }
}

#[test]
fn matches_the_wav_files_it_was_made_from() {
    for (ape, wav) in [
        ("sine.ape", "sine.wav"),
        ("ape/sine-96k-24bit.ape", "sine-96k-24bit.wav"),
        ("ape/mono-22k.ape", "mono-22k.wav"),
    ] {
        let decoded = decode_file(&fixture(ape));
        assert!(!decoded.is_empty());
        assert!(decoded == decode_file(&fixture(wav)), "{ape}");
    }
}

#[test]
fn decodes_silent_and_identical_channel_frames() {
    // The first frame is silent, the second has the left channel in both.
    let mut expected = expected(2, 16);
    for (f, pair) in expected.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        match f as u32 / BLOCKS_PER_FRAME {
            0 => pair.fill(0),
            1 => pair[1] = pair[0],
            _ => {}
        }
    }
    assert!(decode_file(&fixture("ape/stereo-16-flags.ape")) == expected);
}

#[test]
fn reads_the_header() {
    let (format, _) = open(&fixture("ape/stereo-24-insane.ape"));
    let track = format.default_track(TrackType::Audio).unwrap();
    assert_eq!(track.num_frames, Some(u64::from(FRAMES)));
    let params = track.codec_params.as_ref().unwrap().audio().unwrap();
    assert_eq!(params.sample_rate, Some(44_100));
    assert_eq!(params.bits_per_sample, Some(24));
    assert_eq!(params.channels.as_ref().map(|c| c.count()), Some(2));
}

#[test]
fn seeks_to_the_frame_that_holds_the_position() {
    let path = fixture("ape/stereo-16-normal.ape");
    let (mut format, mut decoder) = open(&path);
    let seeked = format
        .seek(
            SeekMode::Accurate,
            SeekTo::Timestamp {
                ts: Timestamp::new(5000),
                track_id: 0,
            },
        )
        .unwrap();
    assert_eq!(seeked.required_ts, Timestamp::new(5000));
    assert_eq!(seeked.actual_ts, Timestamp::new(4096));
    let rest = decode(format.as_mut(), decoder.as_mut());
    assert!(rest == expected(2, 16)[4096 * 2..]);
}

/// Decodes a file with one byte changed, and returns whether each packet decoded.
fn decode_damaged(name: &str, at: f64) -> Vec<bool> {
    let mut bytes = std::fs::read(fixture(name)).unwrap();
    let pos = (bytes.len() as f64 * at) as usize;
    bytes[pos] ^= 0x10;
    let (mut format, mut decoder) = open_source(Box::new(Cursor::new(bytes)), "ape");
    std::iter::from_fn(|| format.next_packet().unwrap())
        .map(|packet| decoder.decode(&packet).is_ok())
        .collect()
}

#[test]
fn a_damaged_frame_fails_its_check() {
    // The second of three frames, each in one packet.
    assert_eq!(
        decode_damaged("ape/stereo-16-normal.ape", 0.5),
        [true, false, true]
    );
    // One frame in 11 packets: reported once, where the damage shows or at the frame's end;
    // the rest of the frame plays as silence.
    let results = decode_damaged("sine.ape", 0.4);
    assert_eq!(results.len(), 11);
    assert_eq!(results.iter().filter(|ok| !**ok).count(), 1, "{results:?}");
}

#[test]
fn a_damaged_24_bit_frame_leaves_the_rest_alone() {
    // A frame that failed its check was taken for one from an early encoder, and the frames
    // after it were decoded with that encoder's rounding, which turns loud passages into
    // noise. Here the second frame's stored CRC is wrong, so only the check fails.
    let mut bytes = std::fs::read(fixture("ape/stereo-24-insane.ape")).unwrap();
    let header = Header::read(&mut Cursor::new(&bytes)).unwrap();
    // The frames are one stream of little-endian words read from the top byte down, so the
    // CRC's top byte is the last of its word.
    let (first, start) = (header.frame_offsets[0], header.frame_offsets[1]);
    let skip = (start - first) % 4;
    bytes[(start - skip + (skip ^ 3)) as usize] ^= 0x02;
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("ape");
    let mut format = PROBE
        .probe(
            &hint,
            mss,
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
    let mut decoder = ApeDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();
    let results: Vec<bool> = std::iter::from_fn(|| format.next_packet().unwrap())
        .map(|packet| decoder.decode(&packet).is_ok())
        .collect();
    let first_bad = results.iter().position(|ok| !ok).expect("damage found");
    assert!(results[first_bad + 1..].iter().all(|ok| *ok), "{results:?}");
    assert!(!decoder.interim_rounding());
}

#[test]
fn refuses_files_before_version_3_95() {
    let mut bytes = b"MAC ".to_vec();
    bytes.extend_from_slice(&3930u16.to_le_bytes());
    bytes.resize(64, 0);
    assert!(Header::read(&mut Cursor::new(bytes)).is_err());
}

#[test]
fn refuses_frame_data_larger_than_any_file() {
    // The descriptor's 64-bit frame data length, at bytes 24 to 32, overflowed the end offset.
    let mut bytes = std::fs::read(fixture("ape/stereo-16-normal.ape")).unwrap();
    assert_eq!(&bytes[..4], b"MAC ");
    bytes[24..32].fill(0xff);
    assert!(Header::read(&mut Cursor::new(bytes)).is_err());
}

/// Decodes real files, every frame checked against its CRC. `FREYA_APE_FILES` lists them, one
/// per line.
#[test]
#[ignore = "needs real APE files"]
fn decodes_real_files() {
    let files = std::env::var("FREYA_APE_FILES").expect("FREYA_APE_FILES");
    for path in files.lines().filter(|line| !line.is_empty()) {
        let started = std::time::Instant::now();
        let (mut format, mut decoder) = open(Path::new(path));
        let mut frames = 0;
        while let Some(packet) = format.next_packet().unwrap() {
            let decoded = decoder
                .decode(&packet)
                .unwrap_or_else(|e| panic!("{path}: packet at {:?}: {e}", packet.pts));
            frames += decoded.frames() as u64;
        }
        let track = format.default_track(TrackType::Audio).unwrap();
        assert_eq!(Some(frames), track.num_frames, "{path}");
        eprintln!("{path}: {frames} frames in {:.1?}", started.elapsed());
    }
}
