//! What a scan does, told in the log as it goes: each step as it starts and ends, and how far
//! along it is every so often, so the log says what the scanner is doing without a line for
//! every file.

use std::time::{Duration, Instant};

use super::ScanEvent;

/// How often a step that is still going is told of.
const EVERY: Duration = Duration::from_secs(10);

/// Tells the log of a scan's events.
pub(super) struct ScanLog {
    /// The step going on, and when it was last told of.
    step: Option<&'static str>,
    told: Instant,
    looked: Instant,
}

impl ScanLog {
    pub(super) fn new() -> Self {
        let long_ago = Instant::now()
            .checked_sub(EVERY)
            .unwrap_or_else(Instant::now);
        Self {
            step: None,
            told: long_ago,
            looked: long_ago,
        }
    }

    pub(super) fn note(&mut self, event: &ScanEvent) {
        let (step, done, total) = match event {
            ScanEvent::Looking(folder) => {
                if self.looked.elapsed() >= EVERY {
                    self.looked = Instant::now();
                    log::info!("scan: looking in {}", folder.display());
                }
                return;
            }
            ScanEvent::Progress { done, total } => ("reading new and changed files", done, total),
            ScanEvent::Covers { done, total } => ("making albums' covers", done, total),
            ScanEvent::Artists { done, total } => {
                ("asking the internet about artists", done, total)
            }
            ScanEvent::Genres { done, total } => ("asking the internet about genres", done, total),
            ScanEvent::Albums { done, total } => (
                "asking MusicBrainz about albums' kinds and genres",
                done,
                total,
            ),
            ScanEvent::Finished(_) | ScanEvent::Failed(_) => {
                self.step = None;
                return;
            }
        };
        if self.step != Some(step) {
            self.step = Some(step);
            self.told = Instant::now();
            log::info!("scan: {step}: {total}");
        } else if done >= total {
            log::info!("scan: {step}: all {total} done");
            self.step = None;
        } else if self.told.elapsed() >= EVERY {
            self.told = Instant::now();
            log::info!("scan: {step}: {done} of {total}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_is_told_as_it_starts_and_ends() {
        let mut log = ScanLog::new();
        log.note(&ScanEvent::Progress { done: 0, total: 3 });
        assert_eq!(log.step, Some("reading new and changed files"));
        // Not again until a while has passed.
        let told = log.told;
        log.note(&ScanEvent::Progress { done: 1, total: 3 });
        assert_eq!(log.told, told);
        log.note(&ScanEvent::Progress { done: 3, total: 3 });
        assert_eq!(log.step, None);
        log.note(&ScanEvent::Covers { done: 0, total: 2 });
        assert_eq!(log.step, Some("making albums' covers"));
    }
}
