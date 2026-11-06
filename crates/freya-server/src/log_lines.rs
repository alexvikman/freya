//! The server's log as an admin reads it in Freya: its last lines, each with its time and
//! level, as the server wrote them.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use freya_protocol::LogLine;

/// How much of the end of the file is read at most.
const MOST_BYTES: u64 = 1024 * 1024;
/// How many lines are given at most.
pub const MOST_LINES: usize = 2000;

/// The last `lines` entries of the log at `file`, the newest last. A message of several lines
/// is one entry.
pub fn read(file: &Path, lines: usize) -> Vec<LogLine> {
    let Ok(mut opened) = std::fs::File::open(file) else {
        return Vec::new();
    };
    let length = opened.metadata().map_or(0, |m| m.len());
    let from = length.saturating_sub(MOST_BYTES);
    if opened.seek(SeekFrom::Start(from)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if opened.take(MOST_BYTES).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut entries = parse(&text);
    // Read from the middle of the file, the first entry may be the end of one.
    if from > 0 && !entries.is_empty() {
        entries.remove(0);
    }
    let keep = lines.min(MOST_LINES);
    entries.split_off(entries.len().saturating_sub(keep))
}

/// The entries of `text`, as env_logger writes them: "[2026-11-06T18:41:33Z INFO  freya_server]
/// message". A line that does not start so goes on the message before it.
fn parse(text: &str) -> Vec<LogLine> {
    let mut entries: Vec<LogLine> = Vec::new();
    for line in text.lines() {
        match head(line) {
            Some((time, level, message)) => entries.push(LogLine {
                time: time.to_owned(),
                level: level.to_owned(),
                message: message.to_owned(),
            }),
            None => {
                if let Some(last) = entries.last_mut() {
                    last.message.push('\n');
                    last.message.push_str(line);
                }
            }
        }
    }
    entries
}

/// A line's time, level as Freya names levels, and message, when it starts an entry.
fn head(line: &str) -> Option<(&str, &'static str, &str)> {
    let rest = line.strip_prefix('[')?;
    let (inside, message) = rest.split_once("] ")?;
    let mut words = inside.split_whitespace();
    let time = words.next()?;
    let level = match words.next()? {
        "ERROR" => "error",
        "WARN" => "warning",
        "INFO" => "info",
        "DEBUG" | "TRACE" => "debug",
        _ => return None,
    };
    time.contains('T').then_some((time, level, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_is_read_by_entry() {
        let text = "[2026-11-06T18:41:33Z INFO  freya_library::scan] library scan took 2s\n\
                    [2026-11-06T18:41:34Z WARN  freya_server] could not read a.flac:\n\
                    \x20 the file ended early\n\
                    [2026-11-06T18:41:35Z ERROR freya_server::serve] no answer";
        let entries = parse(text);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].time, "2026-11-06T18:41:33Z");
        assert_eq!(entries[0].level, "info");
        assert_eq!(entries[0].message, "library scan took 2s");
        assert_eq!(entries[1].level, "warning");
        assert_eq!(
            entries[1].message,
            "could not read a.flac:\n  the file ended early"
        );
        assert_eq!(entries[2].level, "error");

        let dir = std::env::temp_dir().join(format!("freya-log-lines-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("server.log");
        std::fs::write(&file, text).unwrap();
        let last = read(&file, 2);
        assert_eq!(last.len(), 2);
        assert_eq!(last[1].message, "no answer");
        assert!(read(&dir.join("none.log"), 10).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
