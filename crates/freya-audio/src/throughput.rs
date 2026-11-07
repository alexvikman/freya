//! How fast files come from servers, and whether one can start or go on playing without
//! running dry. Each second a file is being fetched, the bytes that came are kept as one of
//! [`LEVELS`] levels, for [`KEPT`]. The seconds that follow one another make a Markov chain:
//! from each level, how often each level came next. Before playing, the playing of the rest
//! of the file is simulated [`RUNS`] times with it, from what is fetched already and how fast
//! it comes now; when more than [`FAILURES`] of them run dry, playing waits.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long what was seen is kept.
const KEPT: Duration = Duration::from_secs(15 * 60);
/// The levels a second's bytes are kept as: none, then from [`SLOWEST`] to [`FASTEST`] a
/// second, each a fixed ratio above the one before, finer where it is slow.
pub(crate) const LEVELS: usize = 33;
const SLOWEST: f64 = 8.0 * 1024.0;
const FASTEST: f64 = 64.0 * 1024.0 * 1024.0;
/// How often playing is simulated, and how many of them may run dry.
const RUNS: usize = 100;
const FAILURES: usize = 1;
/// The longest a simulation looks ahead: a file that takes longer than this to come is
/// played as if the rest came at the rate it ends at.
const LONGEST: usize = 4 * 60 * 60;

/// The bytes a second, kept as a level.
fn level(bytes: f64) -> u8 {
    if bytes < SLOWEST / 2.0 {
        return 0;
    }
    let step = (FASTEST / SLOWEST).ln() / (LEVELS - 2) as f64;
    let above = ((bytes / SLOWEST).ln() / step).round();
    (1.0 + above.clamp(0.0, (LEVELS - 2) as f64)) as u8
}

/// The bytes a second a level stands for.
fn bytes_of(level: u8) -> f64 {
    if level == 0 {
        return 0.0;
    }
    let step = (FASTEST / SLOWEST).ln() / (LEVELS - 2) as f64;
    SLOWEST * (step * f64::from(level - 1)).exp()
}

/// One second of fetching: when it started, its level, and whether it followed the second
/// before it without a pause in fetching.
#[derive(Clone, Copy, Debug)]
struct Second {
    at: Instant,
    level: u8,
    follows: bool,
}

/// What was seen of the network, and the second being counted.
#[derive(Debug, Default)]
pub(crate) struct Throughput {
    seconds: VecDeque<Second>,
    /// The second being counted: when it started, and the bytes so far.
    counting: Option<(Instant, u64)>,
    /// How many files are being fetched; the seconds only count while one is.
    fetching: usize,
}

static SEEN: Mutex<Option<Throughput>> = Mutex::new(None);

/// What was seen, shared by every file fetched.
pub(crate) fn seen() -> MutexGuard<'static, Option<Throughput>> {
    SEEN.lock().unwrap_or_else(|e| e.into_inner())
}

/// Counts fetching while it lives: a file being fetched holds one.
pub(crate) struct Fetching;

impl Fetching {
    pub fn start() -> Self {
        let mut seen = seen();
        let seen = seen.get_or_insert_with(Throughput::default);
        seen.fetching += 1;
        if seen.counting.is_none() {
            seen.counting = Some((Instant::now(), 0));
        }
        Fetching
    }
}

impl Drop for Fetching {
    fn drop(&mut self) {
        if let Some(seen) = seen().as_mut() {
            seen.fetching = seen.fetching.saturating_sub(1);
            if seen.fetching == 0 {
                // A second cut short says nothing of how fast a whole one is.
                seen.counting = None;
            }
        }
    }
}

/// Counts `bytes` that came now.
pub(crate) fn came(bytes: u64) {
    if let Some(seen) = seen().as_mut() {
        seen.came(Instant::now(), bytes);
    }
}

impl Throughput {
    /// Counts `bytes` that came at `now`, closing the seconds that ended before it: those
    /// without any, as none.
    pub fn came(&mut self, now: Instant, bytes: u64) {
        if self.fetching == 0 {
            return;
        }
        let (mut start, mut counted) = self.counting.unwrap_or((now, 0));
        while now.duration_since(start) >= Duration::from_secs(1) {
            self.close(start, counted);
            start += Duration::from_secs(1);
            counted = 0;
        }
        self.counting = Some((start, counted + bytes));
    }

    fn close(&mut self, at: Instant, bytes: u64) {
        let follows = self
            .seconds
            .back()
            .is_some_and(|last| at.duration_since(last.at) <= Duration::from_millis(1500));
        self.seconds.push_back(Second {
            at,
            level: level(bytes as f64),
            follows,
        });
        while self
            .seconds
            .front()
            .is_some_and(|first| at.duration_since(first.at) > KEPT)
        {
            self.seconds.pop_front();
        }
    }

    /// From each level, how often each came next.
    fn chain(&self) -> Vec<[u32; LEVELS]> {
        let mut chain = vec![[0u32; LEVELS]; LEVELS];
        for (before, after) in self.seconds.iter().zip(self.seconds.iter().skip(1)) {
            if after.follows {
                chain[usize::from(before.level)][usize::from(after.level)] += 1;
            }
        }
        chain
    }

    /// Whether playing goes on without running dry: `buffered` bytes are here ahead of where
    /// it plays, `missing` more are to come, and it plays `rate` bytes a second. None when
    /// too little was seen to tell.
    pub fn plays_through(&self, buffered: u64, missing: u64, rate: f64) -> Option<bool> {
        if missing == 0 {
            return Some(true);
        }
        let now = self.seconds.back()?.level;
        let chain = self.chain();
        if chain.iter().all(|next| next.iter().all(|&n| n == 0)) {
            return None;
        }
        let mut random = Random::new();
        let mut failed = 0;
        for _ in 0..RUNS {
            if !simulate(
                &chain,
                now,
                buffered as f64,
                missing as f64,
                rate,
                &mut random,
            ) {
                failed += 1;
                if failed > FAILURES {
                    return Some(false);
                }
            }
        }
        Some(true)
    }
}

/// One run: a second at a time, a level drawn from the chain after the one before, its
/// bytes come while `rate` of them play. False when it runs dry before the rest came.
fn simulate(
    chain: &[[u32; LEVELS]],
    mut level: u8,
    mut buffered: f64,
    mut missing: f64,
    rate: f64,
    random: &mut Random,
) -> bool {
    for _ in 0..LONGEST {
        level = next_level(&chain[usize::from(level)], level, random);
        let came = bytes_of(level).min(missing);
        missing -= came;
        buffered += came - rate;
        if missing <= 0.0 {
            return true;
        }
        if buffered < 0.0 {
            return false;
        }
    }
    true
}

/// A level drawn from what came after `level`, as often as each did; `level` again when
/// nothing was seen after it.
fn next_level(after: &[u32; LEVELS], level: u8, random: &mut Random) -> u8 {
    let total: u32 = after.iter().sum();
    if total == 0 {
        return level;
    }
    let mut pick = random.below(total);
    for (next, &count) in after.iter().enumerate() {
        if pick < count {
            return next as u8;
        }
        pick -= count;
    }
    level
}

/// A small generator of numbers that look random, enough to draw levels with.
struct Random(u64);

impl Random {
    fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9_7f4a_7c15, |t| t.as_nanos() as u64);
        Self(seed | 1)
    }

    fn below(&mut self, n: u32) -> u32 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let value = self.0.wrapping_mul(0x2545_f491_4f6c_dd1d);
        (((value >> 32) * u64::from(n)) >> 32) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_cover_slow_to_fast_and_stand_for_their_bytes() {
        assert_eq!(level(0.0), 0);
        assert_eq!(level(SLOWEST), 1);
        assert_eq!(level(FASTEST), (LEVELS - 1) as u8);
        assert_eq!(level(FASTEST * 100.0), (LEVELS - 1) as u8);
        for l in 1..LEVELS as u8 {
            assert_eq!(level(bytes_of(l)), l);
        }
        // Finer where it is slow: a level's width grows with it.
        assert!(bytes_of(3) - bytes_of(2) < bytes_of(20) - bytes_of(19));
    }

    /// What was seen when each second brought `bytes`, in turn, fetching all along.
    fn seen_at(rates: &[u64]) -> Throughput {
        let mut seen = Throughput {
            fetching: 1,
            ..Default::default()
        };
        let start = Instant::now();
        for (i, &bytes) in rates.iter().enumerate() {
            let at = start + Duration::from_millis(i as u64 * 1000 + 500);
            seen.came(at, bytes);
        }
        seen.came(start + Duration::from_secs(rates.len() as u64 + 1), 0);
        seen
    }

    #[test]
    fn a_fast_network_plays_at_once_and_a_slow_one_waits() {
        // A megabyte a second, steady.
        let fast = seen_at(&[1 << 20; 30]);
        // 30 seconds of a CD's FLAC to come, 110 kB a second, nothing here yet.
        assert_eq!(fast.plays_through(0, 3_300_000, 110_000.0), Some(true));
        // 20 kB a second can't keep up with it.
        let slow = seen_at(&[20_000; 30]);
        assert_eq!(slow.plays_through(0, 3_300_000, 110_000.0), Some(false));
        // Unless most of it is here.
        assert_eq!(
            slow.plays_through(3_000_000, 300_000, 110_000.0),
            Some(true)
        );
    }

    #[test]
    fn a_network_that_stops_now_and_then_needs_more_ahead() {
        // Fast, but every fourth second nothing comes; the last one fast.
        let rates: Vec<u64> = (0..121)
            .map(|i| if i % 4 == 3 { 0 } else { 200_000 })
            .collect();
        let seen = seen_at(&rates);
        // On average 150 kB a second, against 100 kB played: with nothing ahead, a pause
        // runs it dry; with ten seconds ahead, it plays through.
        assert_eq!(seen.plays_through(0, 20_000_000, 100_000.0), Some(false));
        assert_eq!(
            seen.plays_through(1_000_000, 20_000_000, 100_000.0),
            Some(true)
        );
    }

    #[test]
    fn nothing_seen_tells_nothing_and_nothing_missing_plays() {
        let seen = Throughput::default();
        assert_eq!(seen.plays_through(0, 1000, 1000.0), None);
        assert_eq!(seen.plays_through(0, 0, 1000.0), Some(true));
    }

    #[test]
    fn seconds_without_fetching_are_not_counted_as_none() {
        let mut seen = seen_at(&[100_000; 5]);
        let before = seen.seconds.len();
        // Fetching stops, and starts again a minute later.
        seen.fetching = 0;
        seen.counting = None;
        let later = Instant::now() + Duration::from_secs(60);
        seen.came(later, 100_000);
        assert_eq!(seen.seconds.len(), before);
        seen.fetching = 1;
        seen.came(later, 100_000);
        seen.came(later + Duration::from_millis(1100), 100_000);
        let last = seen.seconds.back().unwrap();
        assert!(!last.follows, "a new run, not a minute of nothing");
        assert_eq!(last.level, level(100_000.0));
    }
}
