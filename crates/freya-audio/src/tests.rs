//! Engine tests against a null output that pulls samples as fast as possible.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::null_output::NullOutput;
use crate::{Event, Media, MediaTags, PlaybackState, Player, TrackInfo};

const TIMEOUT: Duration = Duration::from_secs(10);

fn headless(rate: u32) -> (Player, NullOutput) {
    let (player, renderer) = Player::spawn(rate, 2);
    (player, NullOutput::start(renderer, 2, None))
}

/// Writes a 16-bit stereo sine WAV file.
fn write_wav(name: &str, rate: u32, frames: u32) -> PathBuf {
    let path = std::env::temp_dir().join(format!("freya-{}-{name}.wav", std::process::id()));
    let data_len = frames * 4;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&2u16.to_le_bytes()); // channels
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        let t = i as f32 / rate as f32;
        let s = ((t * 440.0 * std::f32::consts::TAU).sin() * 8000.0) as i16;
        bytes.extend_from_slice(&s.to_le_bytes());
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(&path, bytes).unwrap();
    path
}

fn next_event(player: &Player) -> Event {
    player
        .events()
        .recv_timeout(TIMEOUT)
        .expect("event within timeout")
}

fn started_name(event: Event) -> String {
    match event {
        Event::TrackStarted(t) => file_name(&t.path),
        other => panic!("expected TrackStarted, got {other:?}"),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

fn consumed(player: &Player) -> u64 {
    player.shared.consumed.load(Ordering::Acquire)
}

#[test]
fn plays_and_resamples_to_end() {
    let wav = write_wav("resample", 44_100, 44_100);
    let (player, _out) = headless(48_000);

    player.play(&wav);
    assert_eq!(started_name(next_event(&player)), file_name(&wav));
    assert!(matches!(next_event(&player), Event::Finished));

    // One second of audio is exactly one second at the output rate.
    assert_eq!(consumed(&player), 48_000);
    assert_eq!(player.status().state, PlaybackState::Stopped);
}

#[test]
fn gapless_sequence_has_exact_length() {
    let a = write_wav("gapless-a", 48_000, 24_000);
    let b = write_wav("gapless-b", 48_000, 12_345);
    let (player, _out) = headless(48_000);

    player.set_next(Some(b.clone().into()));
    player.play(&a);
    assert_eq!(started_name(next_event(&player)), file_name(&a));
    assert_eq!(started_name(next_event(&player)), file_name(&b));
    assert!(matches!(next_event(&player), Event::Finished));
    assert_eq!(consumed(&player), 24_000 + 12_345);
}

#[test]
fn seek_jumps_to_position() {
    let wav = write_wav("seek", 48_000, 48_000 * 3);
    let (player, _out) = headless(48_000);

    player.set_paused(true);
    player.play(&wav);
    // `play` unpauses; pause again so position stays put until we seek.
    started_name(next_event(&player));
    player.set_paused(true);
    player.seek(Duration::from_secs(2));

    let deadline = Instant::now() + TIMEOUT;
    loop {
        let pos = player.status().position;
        if pos >= Duration::from_secs(2) {
            assert!(pos < Duration::from_millis(2100), "position {pos:?}");
            break;
        }
        assert!(Instant::now() < deadline, "seek never happened");
        std::thread::sleep(Duration::from_millis(5));
    }

    let before = consumed(&player);
    player.set_paused(false);
    assert!(matches!(next_event(&player), Event::Finished));
    // Only the last second is played after the seek.
    assert_eq!(consumed(&player) - before, 48_000);
}

/// While a seek is under way, the track still plays: a skin showing the status never falls
/// back to showing nothing playing.
#[test]
fn the_track_stays_current_while_seeking() {
    let wav = write_wav("seek-status", 48_000, 48_000 * 60);
    let (player, _out) = headless(48_000);
    player.play(&wav);
    started_name(next_event(&player));
    player.set_paused(true);

    for i in 0..200u64 {
        let target = Duration::from_millis((i * 7919) % 59_000);
        player.seek(target);
        let until = Instant::now() + Duration::from_millis(5);
        while Instant::now() < until {
            let status = player.status();
            assert_eq!(status.state, PlaybackState::Paused, "seek {i}");
            assert!(status.track.is_some(), "seek {i}");
        }
    }
    // The last seek lands where it was asked to.
    let deadline = Instant::now() + TIMEOUT;
    let last = Duration::from_millis((199 * 7919) % 59_000);
    while player.status().position.abs_diff(last) > Duration::from_millis(50) {
        assert!(
            Instant::now() < deadline,
            "at {:?}",
            player.status().position
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn part(path: &Path, start_ms: u64, end_ms: Option<u64>) -> Media {
    Media {
        path: path.to_owned(),
        start: Duration::from_millis(start_ms),
        end: end_ms.map(Duration::from_millis),
        tags: MediaTags::default(),
        id: 0,
        remote: None,
    }
}

fn started(event: Event) -> Arc<TrackInfo> {
    match event {
        Event::TrackStarted(t) => t,
        other => panic!("expected TrackStarted, got {other:?}"),
    }
}

#[test]
fn plays_a_part_of_a_file_with_its_own_tags() {
    let wav = write_wav("part", 48_000, 48_000);
    let (player, _out) = headless(48_000);

    player.play(Media {
        tags: MediaTags {
            title: Some("Second".into()),
            ..Default::default()
        },
        ..part(&wav, 250, Some(500))
    });
    let track = started(next_event(&player));
    assert_eq!(track.title.as_deref(), Some("Second"));
    assert_eq!(track.start, Duration::from_millis(250));
    assert_eq!(track.duration, Some(Duration::from_millis(250)));
    assert!(matches!(next_event(&player), Event::Finished));
    assert_eq!(consumed(&player), 12_000);
}

#[test]
fn parts_of_one_file_follow_each_other_without_a_gap() {
    // At the output's rate, and resampled: either way, not a sample is lost or added.
    for rate in [48_000, 44_100] {
        let wav = write_wav(&format!("parts-{rate}"), rate, rate);
        let (player, _out) = headless(48_000);

        player.set_next(Some(part(&wav, 400, None)));
        player.play(part(&wav, 0, Some(400)));
        let first = started(next_event(&player));
        assert_eq!(first.duration, Some(Duration::from_millis(400)));
        let second = started(next_event(&player));
        assert_eq!(second.start, Duration::from_millis(400));
        assert_eq!(second.duration, Some(Duration::from_millis(600)));
        assert!(matches!(next_event(&player), Event::Finished));
        assert_eq!(consumed(&player), 48_000, "from {rate} Hz");
    }
}

#[test]
fn a_part_of_a_compressed_file_starts_where_it_should() {
    for name in ["sine.flac", "sine.ape"] {
        let file = fixture(name);
        // On its own, from its start: the file is opened and sought.
        let (player, _out) = headless(48_000);
        player.play(part(&file, 300, None));
        started(next_event(&player));
        assert!(matches!(next_event(&player), Event::Finished));
        assert!(
            consumed(&player).abs_diff(33_600) <= 1,
            "{name}: {}",
            consumed(&player)
        );

        // After the part before it: decoding goes on, across the packet the boundary is in.
        let (player, _out) = headless(48_000);
        player.set_next(Some(part(&file, 300, None)));
        player.play(part(&file, 0, Some(300)));
        started(next_event(&player));
        started(next_event(&player));
        assert!(matches!(next_event(&player), Event::Finished));
        assert_eq!(consumed(&player), 48_000, "{name}");
    }
}

#[test]
fn seeking_in_a_part_counts_from_its_start() {
    let wav = write_wav("part-seek", 48_000, 48_000 * 4);
    let (player, _out) = headless(48_000);

    player.play(part(&wav, 1000, Some(3000)));
    started(next_event(&player));
    player.set_paused(true);
    player.seek(Duration::from_secs(1));

    let deadline = Instant::now() + TIMEOUT;
    loop {
        let pos = player.status().position;
        if pos >= Duration::from_secs(1) {
            assert!(pos < Duration::from_millis(1100), "position {pos:?}");
            break;
        }
        assert!(Instant::now() < deadline, "seek never happened");
        std::thread::sleep(Duration::from_millis(5));
    }

    let before = consumed(&player);
    player.set_paused(false);
    assert!(matches!(next_event(&player), Event::Finished));
    // The part's last second, up to its end and not the file's.
    assert_eq!(consumed(&player) - before, 48_000);
}

#[test]
fn missing_file_reports_error() {
    let (player, _out) = headless(48_000);
    player.play(Media {
        id: 7,
        ..Media::from("/nonexistent/file.mp3")
    });
    // With the id asked for, so the caller knows which of its entries failed.
    assert!(matches!(next_event(&player), Event::Unplayable(7, _)));
    assert_eq!(player.status().state, PlaybackState::Stopped);
}

#[test]
fn a_file_without_a_sample_rate_is_unplayable() {
    // A WAV header claiming 0 Hz made the rate converter panic, which killed the engine.
    let bad = write_wav("zero-rate", 0, 100);
    let good = write_wav("after-zero-rate", 48_000, 4_800);
    let (player, _out) = headless(48_000);
    player.play(Media {
        id: 3,
        ..Media::from(&bad)
    });
    assert!(matches!(next_event(&player), Event::Unplayable(3, _)));
    // The engine is still there to play the next file.
    player.play(&good);
    assert_eq!(started_name(next_event(&player)), file_name(&good));
}

#[test]
fn seeking_past_the_end_goes_on_with_the_next_track() {
    // Holding → near the end once dropped what was buffered, kept the old position, and could
    // even replay the track from its start.
    let a = write_wav("past-end-a", 48_000, 48_000 * 2);
    let b = write_wav("past-end-b", 48_000, 48_000);
    let (player, _out) = headless(48_000);
    player.play(&a);
    assert_eq!(started_name(next_event(&player)), file_name(&a));
    player.set_next(Some(Media::from(&b)));
    player.seek(Duration::from_secs(5));
    assert_eq!(started_name(next_event(&player)), file_name(&b));
    assert!(matches!(next_event(&player), Event::Finished));

    // With nothing after it, playback finishes.
    player.play(&a);
    assert_eq!(started_name(next_event(&player)), file_name(&a));
    player.seek(Duration::from_secs(9));
    assert!(matches!(next_event(&player), Event::Finished));
    assert_eq!(player.status().state, PlaybackState::Stopped);
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Plays a fixture to the end and returns its info and the number of output frames.
fn play_fixture(name: &str) -> (Arc<crate::TrackInfo>, u64) {
    let (player, _out) = headless(48_000);
    player.play(fixture(name));
    let info = match next_event(&player) {
        Event::TrackStarted(info) => info,
        other => panic!("{name}: expected TrackStarted, got {other:?}"),
    };
    match next_event(&player) {
        Event::Finished => {}
        other => panic!("{name}: expected Finished, got {other:?}"),
    }
    (info, consumed(&player))
}

#[test]
fn decodes_all_formats() {
    // (file, codec, seconds, allowed length error in output frames)
    let cases = [
        ("sine.wav", "pcm_s16le", 1.0, 0),
        ("sine-96k-24bit.wav", "pcm_s24le", 0.25, 0),
        ("mono-22k.wav", "pcm_s16le", 1.0, 0),
        ("sine.flac", "flac", 1.0, 0),
        ("sine-alac.m4a", "alac", 1.0, 0),
        ("sine.ape", "ape", 1.0, 0),
        ("sine.ogg", "vorbis", 1.0, 0),
        // LAME writes encoder delay/padding, so MP3 should be close to exact.
        ("sine.mp3", "mp3", 1.0, 1200),
        // Symphonia has no gapless support for AAC: priming and padding remain.
        ("sine-aac.m4a", "aac", 1.0, 4096),
        // Opus through libopus; Ogg's granule positions trim the pre-skip and padding.
        ("sine.opus", "opus", 1.0, 960),
    ];
    for (name, codec, secs, tolerance) in cases {
        let (info, frames) = play_fixture(name);
        assert_eq!(info.codec, codec, "{name}");
        let expected = (secs * 48_000.0) as u64;
        assert!(
            frames.abs_diff(expected) <= tolerance,
            "{name}: {frames} frames, expected {expected} ± {tolerance}"
        );
        let duration = info.duration.expect("duration").as_secs_f64();
        assert!((duration - secs).abs() < 0.1, "{name}: duration {duration}");
    }
}

#[test]
fn reads_opus_tags() {
    let (info, _) = play_fixture("sine.opus");
    assert_eq!(info.display_title(), "Freya - Sine Test");
    assert_eq!(info.sample_rate, 48_000);
}

#[test]
fn reads_tags() {
    for name in ["sine.mp3", "sine.ape"] {
        let (info, _) = play_fixture(name);
        assert_eq!(info.title.as_deref(), Some("Sine Test"), "{name}");
        assert_eq!(info.artist.as_deref(), Some("Freya"), "{name}");
        assert_eq!(info.display_title(), "Freya - Sine Test", "{name}");
    }
}

/// A radio station on localhost that sends `body` once, then hangs up.
fn radio_station(headers: &'static str, body: Vec<u8>) -> String {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/stream", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let Ok((connection, _)) = listener.accept() else {
            return;
        };
        let mut reader = BufReader::new(connection);
        let mut asked_for_titles = false;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            asked_for_titles |= line.to_ascii_lowercase().starts_with("icy-metadata: 1");
        }
        assert!(asked_for_titles, "Freya asks for titles");
        let mut connection = reader.into_inner();
        let _ = connection.write_all(headers.as_bytes());
        let _ = connection.write_all(&body);
    });
    url
}

/// Interleaves ICY title blocks into `audio` every `interval` bytes; `titles[i]` follows
/// the i-th interval, and after the last title come empty blocks, as stations send.
fn with_titles(audio: &[u8], interval: usize, titles: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, piece) in audio.chunks(interval).enumerate() {
        out.extend_from_slice(piece);
        if piece.len() < interval {
            break;
        }
        let Some(title) = titles.get(i) else {
            out.push(0);
            continue;
        };
        let mut block = format!("StreamTitle='{title}';").into_bytes();
        block.resize(block.len().div_ceil(16) * 16, 0);
        out.push((block.len() / 16) as u8);
        out.extend(block);
    }
    out
}

const RADIO_HEADERS: &str = "HTTP/1.0 200 OK\r\nContent-Type: audio/mpeg\r\n\
    icy-name: Test Radio\r\nicy-br: 128\r\nicy-metaint: 2048\r\n\r\n";

#[test]
fn radio_titles_follow_the_audio() {
    let audio = std::fs::read(fixture("sine.mp3")).unwrap();
    let titles = [
        "Artist One - First Song",
        "Artist One - First Song",
        "Artist One - First Song",
        "Artist Two - Second Song",
    ];
    let url = radio_station(RADIO_HEADERS, with_titles(&audio, 2048, &titles));

    let mut source = crate::source::Source::open(&url.as_str().into(), &Default::default())
        .expect("open stream");
    assert_eq!(source.info().station.as_deref(), Some("Test Radio"));
    assert_eq!(source.info().bitrate_kbps, Some(128));
    assert_eq!(source.info().duration, None);

    let mut seen = Vec::new();
    let mut frames = 0;
    while let Some(chunk) = source.next_chunk().expect("decode") {
        frames += chunk.samples.len() / chunk.channels;
        if let Some(info) = source.take_info_change() {
            seen.push((
                info.artist.clone().unwrap_or_default(),
                info.title.clone().unwrap_or_default(),
                frames,
            ));
        }
    }
    let names: Vec<(&str, &str)> = seen
        .iter()
        .map(|(a, t, _)| (a.as_str(), t.as_str()))
        .collect();
    assert_eq!(
        names,
        [("Artist One", "First Song"), ("Artist Two", "Second Song")],
        "each change once, in order"
    );
    // The second title was sent after 4 × 2048 bytes, about half of the one-second file.
    let second_at = seen[1].2 as f64 / 44_100.0;
    assert!(
        (0.35..0.75).contains(&second_at),
        "second title at {second_at:.2} s"
    );
}

#[test]
fn radio_plays_through_the_player() {
    let audio = std::fs::read(fixture("sine.mp3")).unwrap();
    let body = with_titles(&audio, 2048, &["Someone - Something"]);
    let url = radio_station(RADIO_HEADERS, body);
    let (player, _out) = headless(48_000);
    player.play(&url);
    let Event::TrackStarted(info) = next_event(&player) else {
        panic!("expected TrackStarted");
    };
    assert_eq!(info.station.as_deref(), Some("Test Radio"));
    assert_eq!(info.path.to_str(), Some(url.as_str()));
    assert!(matches!(next_event(&player), Event::Finished));
}

#[test]
fn a_silent_station_does_not_hold_up_the_next_track() {
    // Sends its headers, then nothing at all.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/stream", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut connection, _) = listener.accept().unwrap();
        let _ = connection.read(&mut [0; 1024]);
        let _ = connection.write_all(RADIO_HEADERS.as_bytes());
        std::thread::sleep(Duration::from_secs(30));
    });
    let wav = write_wav("after-radio", 48_000, 4_800);
    let (player, _out) = headless(48_000);
    player.play(&url);
    std::thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    player.play(&wav);
    assert_eq!(
        started_name(next_event(&player)),
        file_name(&wav),
        "no error first"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// Plays real files cut into parts the way a CUE sheet cuts a disc, at the files' own rate:
/// one after the other, then one part on its own from its start. `FREYA_APE_FILES` lists the
/// files, one per line; they must be 44.1 kHz.
#[test]
#[ignore = "needs real files"]
fn plays_real_files_in_parts_without_a_gap() {
    const RATE: u64 = 44_100;
    let files = std::env::var("FREYA_APE_FILES").expect("FREYA_APE_FILES");
    for path in files.lines().filter(|line| !line.is_empty()) {
        let path = Path::new(path);
        let source = crate::source::Source::open(&path.into(), &Default::default()).unwrap();
        let length = source.info().duration;
        let (player, _out) = headless(RATE as u32);
        // Parts of 3:01.27, a length in CD frames, so they start anywhere in the file's frames.
        let part_ms = 181_360;
        player.play(part(path, 0, Some(part_ms)));
        started(next_event(&player));
        let mut start = part_ms;
        let mut parts = 1;
        loop {
            let end = start + part_ms;
            let last = length.is_some_and(|l| Duration::from_millis(end) >= l);
            player.set_next(Some(part(path, start, (!last).then_some(end))));
            started(next_event(&player));
            parts += 1;
            start = end;
            if last {
                break;
            }
        }
        assert!(matches!(next_event(&player), Event::Finished));
        let total = consumed(&player);
        let expected = (length.unwrap().as_secs_f64() * RATE as f64).round() as u64;
        assert!(
            total.abs_diff(expected) <= 1,
            "{path:?}: {total} of {expected}"
        );

        // The fourth part on its own: opened and sought to its start.
        let (player, _out) = headless(RATE as u32);
        player.play(part(path, 3 * part_ms, Some(4 * part_ms)));
        started(next_event(&player));
        assert!(matches!(next_event(&player), Event::Finished));
        assert_eq!(consumed(&player), part_ms * RATE / 1000, "{path:?}");
        eprintln!("{path:?}: {parts} parts, {total} frames");
    }
}

/// The frames a file plays, through the source.
fn frames_played(path: &Path) -> u64 {
    let mut source = crate::source::Source::open(&path.into(), &Default::default()).unwrap();
    let mut frames = 0;
    while let Some(chunk) = source.next_chunk().unwrap() {
        frames += (chunk.samples.len() / chunk.channels) as u64;
    }
    frames
}

#[test]
fn a_damaged_packet_plays_as_silence_of_its_length() {
    // It was left out, so all after it came early: a CUE track then started off its mark.
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ape/stereo-16-normal.ape");
    let mut bytes = std::fs::read(&fixture).unwrap();
    let pos = bytes.len() / 2;
    bytes[pos] ^= 0x10;
    let damaged = std::env::temp_dir().join(format!("freya-{}-damaged.ape", std::process::id()));
    std::fs::write(&damaged, bytes).unwrap();
    assert_eq!(frames_played(&damaged), frames_played(&fixture));
    let _ = std::fs::remove_file(damaged);
}

#[test]
fn connecting_to_a_station_can_be_cut_short() {
    // The connection used to block for its timeouts, so Stop, another station or quitting
    // waited up to 20 s, and Stop then came back as an error that skipped to the next entry.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/stream", listener.local_addr().unwrap());
    // Accepts and never answers.
    std::thread::spawn(move || {
        let _connection = listener.accept();
        std::thread::sleep(Duration::from_secs(30));
    });
    let interrupt = crate::stream::Interrupt::default();
    let raise = interrupt.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        raise.raise();
    });
    let started = Instant::now();
    let result = crate::source::Source::open(&url.as_str().into(), &interrupt);
    assert!(matches!(result, Err(crate::Error::Interrupted)));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_stream_stays_ended_for_every_read() {
    // The end was reported once; a decoder reading again, in the middle of a frame, waited
    // for the station for 15 s.
    use std::io::Read;
    let url = radio_station(
        "HTTP/1.0 200 OK\r\nContent-Type: audio/mpeg\r\n\r\n",
        vec![0x55; 1000],
    );
    let (mut stream, _, _) =
        crate::stream::Stream::open(&url, crate::stream::Interrupt::default()).unwrap();
    let mut out = [0; 4096];
    while stream.read(&mut out).unwrap() > 0 {}
    let started = Instant::now();
    for _ in 0..3 {
        assert_eq!(stream.read(&mut out).unwrap(), 0);
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

/// A server of `file` that answers Range requests, as Freya's does, one connection a request;
/// with `fail_every`, every that many requests is dropped unanswered.
fn file_server(file: PathBuf, fail_every: Option<usize>) -> String {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/audio", listener.local_addr().unwrap());
    let bytes = std::fs::read(file).unwrap();
    std::thread::spawn(move || {
        for (n, connection) in listener.incoming().enumerate() {
            let Ok(connection) = connection else {
                continue;
            };
            let mut reader = BufReader::new(connection);
            let mut range = None;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                    let (from, to) = value.trim().split_once('-').unwrap();
                    range = Some((from.parse::<usize>().unwrap(), to.parse::<usize>().unwrap()));
                }
            }
            if fail_every.is_some_and(|every| n % every == every - 1) {
                continue;
            }
            let (from, to) = range.expect("a range is asked for");
            let to = to.min(bytes.len() - 1);
            let mut connection = reader.into_inner();
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {from}-{to}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len(),
                to + 1 - from
            );
            let _ = connection.write_all(head.as_bytes());
            let _ = connection.write_all(&bytes[from..=to]);
        }
    });
    url
}

/// The frames `media` plays from `at` on, through the source.
fn frames_from(media: &Media, at: Duration) -> u64 {
    let mut source = crate::source::Source::open(media, &Default::default()).unwrap();
    if !at.is_zero() {
        source.seek(at).unwrap();
    }
    let mut frames = 0;
    while let Some(chunk) = source.next_chunk().unwrap() {
        frames += (chunk.samples.len() / chunk.channels) as u64;
    }
    frames
}

#[test]
fn a_file_read_from_a_server_plays_as_the_file_itself() {
    for name in ["sine.flac", "sine.mp3", "sine.ogg"] {
        let local = Media::from(fixture(name));
        let remote = Media {
            remote: Some(crate::RemoteSource {
                url: file_server(fixture(name), None),
                token: None,
                agent: None,
            }),
            ..local.clone()
        };
        assert_eq!(
            frames_from(&remote, Duration::ZERO),
            frames_from(&local, Duration::ZERO),
            "{name}"
        );
        let middle = Duration::from_millis(600);
        assert_eq!(
            frames_from(&remote, middle),
            frames_from(&local, middle),
            "{name} sought"
        );
    }
}

#[test]
fn a_dropped_request_is_made_again() {
    let local = Media::from(fixture("sine.flac"));
    let remote = Media {
        remote: Some(crate::RemoteSource {
            url: file_server(fixture("sine.flac"), Some(3)),
            token: None,
            agent: None,
        }),
        ..local.clone()
    };
    assert_eq!(
        frames_from(&remote, Duration::ZERO),
        frames_from(&local, Duration::ZERO)
    );
}

#[test]
fn a_server_that_never_answers_is_given_up_on() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/audio", listener.local_addr().unwrap());
    // Every connection taken and closed at once.
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            drop(connection)
        }
    });
    let media = Media {
        remote: Some(crate::RemoteSource {
            url,
            token: None,
            agent: None,
        }),
        ..Media::from(fixture("sine.flac"))
    };
    let started = std::time::Instant::now();
    assert!(crate::source::Source::open(&media, &Default::default()).is_err());
    assert!(started.elapsed() < Duration::from_secs(10));
}
