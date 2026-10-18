//! Connects to an internet radio stream and decodes it for a while without playing it,
//! printing the station and every song title it announces.
//!
//! Usage: cargo run -p freya-audio --example stream_info -- <url> [seconds]

use std::time::{Duration, Instant};

use freya_audio::{Event, Player};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let url = args.first().expect("usage: stream_info <url> [seconds]");
    let seconds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20);

    // Plays into nothing at real-time speed, so titles arrive as a listener would see them.
    let player = Player::silent(48_000);
    let started = Instant::now();
    player.play(url);
    let mut last_title = None;
    while started.elapsed() < Duration::from_secs(seconds) {
        if let Ok(event) = player.events().recv_timeout(Duration::from_millis(200)) {
            match event {
                Event::TrackStarted(info) => println!(
                    "{:>5.1} s  started: {:?} · {} · {:?} kbps · {} Hz",
                    started.elapsed().as_secs_f32(),
                    info.station,
                    info.codec,
                    info.bitrate_kbps,
                    info.sample_rate
                ),
                Event::Error(e) | Event::Unplayable(_, e) => println!("error: {e}"),
                Event::Finished => println!("finished"),
            }
        }
        let status = player.status();
        let title = status.track.map(|t| t.display_title());
        if title != last_title {
            println!(
                "{:>5.1} s  now playing: {}",
                started.elapsed().as_secs_f32(),
                title.clone().unwrap_or_default()
            );
            last_title = title;
        }
    }
}
