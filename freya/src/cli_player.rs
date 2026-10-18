//! A minimal interactive terminal player, for exercising the audio engine.

use std::io::{Write, stdout};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind};
use crossterm::{cursor, terminal};
use freya_audio::{Event, Media, PlaybackState, Player, Status, TrackInfo};

const SEEK_STEP: Duration = Duration::from_secs(5);
const VOLUME_STEP: f32 = 0.05;

pub fn run(paths: &[PathBuf]) -> anyhow::Result<()> {
    let files = freya_library::files::collect(paths);
    if files.is_empty() {
        bail!("no playable files found");
    }

    let player = Player::new().context("could not start audio output")?;
    let mut queue = Queue::new(files);
    player.play(&queue.files[0]);

    let _raw = RawMode::enable()?;
    println!("space pause · ←/→ seek · ↑/↓ volume · n/p next/prev · q quit\r");
    // Radio stations change the title while playing; each new one gets its own line.
    let mut shown_title = String::new();

    loop {
        while let Ok(event) = player.events().try_recv() {
            match event {
                Event::TrackStarted(track) => {
                    queue.started(&track.path);
                    player.set_next(queue.next().map(Media::from));
                    print_track(&track);
                    shown_title = track.display_title();
                }
                Event::Unplayable(_, msg) => {
                    print_line(&format!("error: {msg}"));
                    // A failed `play` leaves the engine stopped; move on to the next file.
                    if player.status().state == PlaybackState::Stopped {
                        match queue.after_requested() {
                            Some(path) => player.play(path),
                            None => return Ok(()),
                        }
                    }
                }
                Event::Error(msg) => print_line(&format!("error: {msg}")),
                Event::Finished => match queue.after_requested() {
                    // The queued next track failed to open; try the one after it.
                    Some(path) => player.play(path),
                    None => return Ok(()),
                },
            }
        }

        player.rest_device_when_idle();
        let status = player.status();
        if let Some(title) = status.track.as_ref().map(|t| t.display_title())
            && title != shown_title
        {
            print_line(&format!("♪ {title}"));
            shown_title = title;
        }
        draw_status(&status, player.volume());

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let TermEvent::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let status = player.status();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Char(' ') => player.toggle_pause(),
            KeyCode::Right => player.seek(status.position + SEEK_STEP),
            KeyCode::Left => player.seek(status.position.saturating_sub(SEEK_STEP)),
            KeyCode::Up => player.set_volume(player.volume() + VOLUME_STEP),
            KeyCode::Down => player.set_volume(player.volume() - VOLUME_STEP),
            KeyCode::Char('n') => {
                if let Some(path) = queue.jump(1) {
                    player.play(path);
                }
            }
            KeyCode::Char('p') => {
                if let Some(path) = queue.jump(-1) {
                    player.play(path);
                }
            }
            _ => {}
        }
    }
    print_line("");
    Ok(())
}

/// Tracks where we are in the file list.
struct Queue {
    files: Vec<PathBuf>,
    /// Index of the track currently heard.
    playing: usize,
    /// Index of the last track handed to the engine (played or queued as next).
    requested: usize,
}

impl Queue {
    fn new(files: Vec<PathBuf>) -> Self {
        Self {
            files,
            playing: 0,
            requested: 0,
        }
    }

    fn started(&mut self, path: &Path) {
        let index = if self.files.get(self.requested).is_some_and(|f| f == path) {
            Some(self.requested)
        } else {
            self.files.iter().position(|f| f == path)
        };
        if let Some(i) = index {
            self.playing = i;
            self.requested = i;
        }
    }

    /// The track after the playing one; marks it as requested.
    fn next(&mut self) -> Option<&Path> {
        let i = self.playing + 1;
        let path = self.files.get(i)?;
        self.requested = i;
        Some(path)
    }

    /// The track after the last requested one; marks it as requested.
    fn after_requested(&mut self) -> Option<&Path> {
        let i = self.requested + 1;
        let path = self.files.get(i)?;
        self.requested = i;
        Some(path)
    }

    fn jump(&mut self, delta: isize) -> Option<&Path> {
        let i = self.playing.checked_add_signed(delta)?;
        let path = self.files.get(i)?;
        self.requested = i;
        Some(path)
    }
}

fn print_track(track: &TrackInfo) {
    let mut details = vec![track.codec.clone()];
    if let Some(kbps) = track.bitrate_kbps {
        details.push(format!("{kbps} kbps"));
    }
    details.push(format!("{:.1} kHz", f64::from(track.sample_rate) / 1000.0));
    print_line(&format!(
        "♪ {}  [{}]",
        track.display_title(),
        details.join(", ")
    ));
}

fn print_line(text: &str) {
    let mut out = stdout();
    let _ = crossterm::execute!(out, terminal::Clear(terminal::ClearType::CurrentLine));
    let _ = write!(out, "\r{text}\r\n");
}

fn draw_status(status: &Status, volume: f32) {
    let icon = match status.state {
        PlaybackState::Playing => "▶",
        PlaybackState::Paused => "‖",
        PlaybackState::Stopped => "■",
    };
    let total = status
        .track
        .as_ref()
        .and_then(|t| t.duration)
        .map_or_else(|| "--:--".into(), format_time);
    let mut out = stdout();
    let _ = crossterm::execute!(
        out,
        cursor::MoveToColumn(0),
        terminal::Clear(terminal::ClearType::CurrentLine)
    );
    let _ = write!(
        out,
        "{icon} {} / {total}   vol {:3.0}%",
        format_time(status.position),
        volume * 100.0
    );
    let _ = out.flush();
}

fn format_time(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// Restores the terminal when dropped, including on early return.
struct RawMode;

impl RawMode {
    fn enable() -> anyhow::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        println!();
    }
}
