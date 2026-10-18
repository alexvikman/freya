//! Freya's log: printed as `RUST_LOG` says, and kept for the Activity page, the newest last.
//! What Freya itself logs is kept down to debug messages; other crates' only when an error.
//! Information and worse also go to a file in Freya's data folder, so the page shows what
//! happened in earlier runs too.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// Entries kept for the page.
const KEEP: usize = 5000;
/// The file is cut down to its last half once larger.
const FILE_MAX: u64 = 2 * 1024 * 1024;

/// One thing logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Local time: "26-10-18 21:36:37".
    pub time: String,
    pub level: Level,
    pub message: String,
}

struct Kept {
    entries: VecDeque<Entry>,
    /// Counted up with each entry, so the page knows when to read them again.
    count: u64,
}

static KEPT: Mutex<Kept> = Mutex::new(Kept {
    entries: VecDeque::new(),
    count: 0,
});

struct Logger {
    printed: env_logger::Logger,
    file: Option<Mutex<File>>,
}

/// Whether an entry is kept for the page.
fn kept(metadata: &Metadata<'_>) -> bool {
    if metadata.target().starts_with("freya") {
        metadata.level() <= Level::Debug
    } else {
        metadata.level() <= Level::Error
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        kept(metadata) || self.printed.enabled(metadata)
    }

    fn log(&self, record: &Record<'_>) {
        if self.printed.matches(record) {
            self.printed.log(record);
        }
        if !kept(record.metadata()) {
            return;
        }
        let entry = Entry {
            time: now(),
            level: record.level(),
            message: record.args().to_string(),
        };
        if entry.level <= Level::Info
            && let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            let _ = writeln!(file, "{}", line_of(&entry));
        }
        push(entry);
    }

    fn flush(&self) {
        self.printed.flush();
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            let _ = file.flush();
        }
    }
}

/// Logs as `RUST_LOG` says, warnings by default, and keeps the log for the page and in the
/// file at `path`, reading what earlier runs logged there first.
pub fn init(path: Option<&Path>) {
    let printed =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).build();
    let max = printed.filter().max(LevelFilter::Debug);
    let file = path.and_then(|path| match open(path) {
        Ok(file) => Some(Mutex::new(file)),
        Err(e) => {
            eprintln!("Freya could not keep its log in {}: {e}", path.display());
            None
        }
    });
    if log::set_boxed_logger(Box::new(Logger { printed, file })).is_ok() {
        log::set_max_level(max);
    }
}

/// The log file at `path`, its earlier entries read in, cut down when large.
fn open(path: &Path) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)?;
    let size = file.metadata()?.len();
    let from = size.saturating_sub(FILE_MAX / 2);
    file.seek(SeekFrom::Start(from))?;
    let mut lines: Vec<String> = BufReader::new(&file)
        .lines()
        .map_while(Result::ok)
        .collect();
    // The first line may be cut in two.
    if from > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    for line in &lines {
        if let Some(entry) = entry_of(line) {
            push(entry);
        }
    }
    if size > FILE_MAX {
        let mut kept = lines.join("\n");
        kept.push('\n');
        std::fs::write(path, kept)?;
        file = std::fs::OpenOptions::new().append(true).open(path)?;
    }
    Ok(file)
}

fn push(entry: Entry) {
    let Ok(mut kept) = KEPT.lock() else {
        return;
    };
    if kept.entries.len() == KEEP {
        kept.entries.pop_front();
    }
    kept.entries.push_back(entry);
    kept.count += 1;
}

/// How many entries were ever kept: it changes when one is.
pub fn count() -> u64 {
    KEPT.lock().map_or(0, |kept| kept.count)
}

/// The entries at `level` or worse, the newest first, at most `limit`.
pub fn entries(level: Level, limit: usize) -> Vec<Entry> {
    let Ok(kept) = KEPT.lock() else {
        return Vec::new();
    };
    kept.entries
        .iter()
        .rev()
        .filter(|entry| entry.level <= level)
        .take(limit)
        .cloned()
        .collect()
}

fn now() -> String {
    jiff::Zoned::now().strftime("%y-%m-%d %H:%M:%S").to_string()
}

/// An entry as a line of the file: its time, level and message, by tabs; a message's line
/// breaks and backslashes escaped.
fn line_of(entry: &Entry) -> String {
    let message = entry.message.replace('\\', "\\\\").replace('\n', "\\n");
    format!("{}\t{}\t{message}", entry.time, entry.level)
}

fn entry_of(line: &str) -> Option<Entry> {
    let mut parts = line.splitn(3, '\t');
    let time = parts.next()?.to_owned();
    let level = parts.next()?.parse().ok()?;
    let mut message = String::new();
    let mut chars = parts.next()?.chars();
    while let Some(c) = chars.next() {
        match (c, c == '\\') {
            (_, true) => match chars.next() {
                Some('n') => message.push('\n'),
                Some(other) => message.push(other),
                None => {}
            },
            (c, false) => message.push(c),
        }
    }
    Some(Entry {
        time,
        level,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_read_back_from_its_line() {
        let entry = Entry {
            time: "26-10-18 21:36:37".into(),
            level: Level::Warn,
            message: "two\nlines with a \\ and a\ttab".into(),
        };
        let line = line_of(&entry);
        assert!(!line.contains('\n'));
        assert_eq!(entry_of(&line), Some(entry));
        assert_eq!(entry_of("not a line"), None);
    }

    #[test]
    fn keeps_the_newest_first_by_level() {
        let before = count();
        for (level, message) in [
            (Level::Info, "one"),
            (Level::Debug, "two"),
            (Level::Error, "three"),
        ] {
            push(Entry {
                time: String::new(),
                level,
                message: message.into(),
            });
        }
        assert_eq!(count(), before + 3);
        let shown: Vec<String> = entries(Level::Info, 10)
            .into_iter()
            .map(|e| e.message)
            .filter(|m| ["one", "two", "three"].contains(&m.as_str()))
            .collect();
        assert_eq!(shown, ["three", "one"]);
    }
}
