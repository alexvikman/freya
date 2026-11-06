//! The server's log, for an admin's Activity page: asked of the server every few seconds while
//! the page shows it, on the errands' thread, the window not waiting for it.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use freya_client::Http;
use freya_protocol::{LogLine, Remote, ServerControl};
use slint_interpreter::Value;

use super::app::LOG_SHOWN;
use super::skin_state;

/// How often the server is asked again while its log is shown.
const ASK_EVERY: Duration = Duration::from_secs(3);

/// The server's answer, once it came.
type Answer = Arc<Mutex<Option<Result<Vec<LogLine>, String>>>>;

#[derive(Default)]
pub struct ServerLog {
    lines: Vec<LogLine>,
    error: Option<String>,
    asked: Option<Instant>,
    answer: Answer,
    /// The level the rows were last made for; none when they are to be made again.
    shown: Option<log::Level>,
}

impl ServerLog {
    /// Asks the server for its log when it is time to, and gives the rows to show at `level` or
    /// worse when they changed, the newest first.
    pub fn poll(
        &mut self,
        remote: Option<Arc<Remote<Http>>>,
        level: log::Level,
    ) -> Option<Vec<Value>> {
        let answered = self.answer.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(answered) = answered {
            match answered {
                Ok(lines) if lines != self.lines => {
                    self.lines = lines;
                    self.error = None;
                    self.shown = None;
                }
                Ok(_) => {}
                Err(e) => {
                    self.error = Some(e);
                    self.shown = None;
                }
            }
        }
        if let Some(remote) = remote
            && self.asked.is_none_or(|at| at.elapsed() >= ASK_EVERY)
        {
            self.asked = Some(Instant::now());
            let answer = Arc::clone(&self.answer);
            crate::errands::run(move || {
                let lines = remote
                    .server_log(LOG_SHOWN)
                    .map_err(|e| format!("The server's log could not be read: {e}"));
                *answer.lock().unwrap_or_else(|e| e.into_inner()) = Some(lines);
            });
        }
        if self.shown == Some(level) {
            return None;
        }
        self.shown = Some(level);
        let error = self.error.iter().map(|e| row("", "error", e));
        let lines = self
            .lines
            .iter()
            .rev()
            .filter(|line| level_of(&line.level) <= level)
            .take(LOG_SHOWN)
            .map(|line| row(&local_time(&line.time), &line.level, &line.message));
        Some(error.chain(lines).collect())
    }

    /// Starts over: asked at once the next time, the rows made again.
    pub fn forget(&mut self) {
        *self = Self::default();
    }
}

fn row(time: &str, level: &str, message: &str) -> Value {
    Value::Struct(slint_interpreter::Struct::from_iter([
        ("time".to_owned(), skin_state::string(time)),
        ("level".to_owned(), skin_state::string(level)),
        ("message".to_owned(), skin_state::string(message)),
    ]))
}

/// A level as the server names it.
fn level_of(name: &str) -> log::Level {
    match name {
        "error" => log::Level::Error,
        "warning" => log::Level::Warn,
        "info" => log::Level::Info,
        _ => log::Level::Debug,
    }
}

/// The server's time, in UTC, as Freya's own log shows times: local, "26-11-06 19:41:33".
fn local_time(utc: &str) -> String {
    utc.parse::<jiff::Timestamp>()
        .map(|at| {
            at.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| utc.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_servers_levels_and_times_are_read() {
        assert_eq!(level_of("warning"), log::Level::Warn);
        assert_eq!(level_of("debug"), log::Level::Debug);
        assert_eq!(local_time("not a time"), "not a time");
        assert_eq!(
            local_time("2026-11-06T18:41:33Z").len(),
            "26-11-06 19:41:33".len()
        );
    }
}
