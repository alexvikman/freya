//! How large a track from a server on another computer is fetched: as it is, or made smaller
//! by the server for a slow network (Freya Server's `/audio?quality=`). Chosen by the user, or
//! for each track as it opens: the largest that plays through without waiting, as the
//! network has been (see [`crate::throughput`]), with what of it is kept already.

use std::sync::atomic::{AtomicU8, Ordering};

use crate::cache;
use crate::source::RemoteSource;
use crate::throughput::{self, Throughput};

/// How large a track from a server is fetched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StreamQuality {
    /// The largest that plays through, as the network has been.
    #[default]
    Automatic,
    /// As it is.
    Original,
    /// At 160 kbit/s.
    High,
    /// At 96 kbit/s.
    Normal,
}

/// The smaller versions a server makes: their names, as asked for, and the bytes a second
/// they play, as Freya Server makes them (see its `freya-transcode`).
pub const SMALLER: [(StreamQuality, &str, u32); 2] = [
    (StreamQuality::High, "high", 20_000),
    (StreamQuality::Normal, "normal", 12_000),
];

/// How much more a second a file plays than a smaller version for the server to make it:
/// as the server reckons it.
pub const SMALLER_BY: f64 = 1.5;

static CHOSEN: AtomicU8 = AtomicU8::new(0);

/// Fetches tracks from servers as `quality` says, from the next one opened.
pub fn set_stream_quality(quality: StreamQuality) {
    CHOSEN.store(quality as u8, Ordering::Relaxed);
}

fn chosen() -> StreamQuality {
    match CHOSEN.load(Ordering::Relaxed) {
        1 => StreamQuality::Original,
        2 => StreamQuality::High,
        3 => StreamQuality::Normal,
        _ => StreamQuality::Automatic,
    }
}

/// The address to fetch `remote`'s file from; `length` asks the server how long it is as it
/// is, when that is not known.
pub(crate) fn url_for(remote: &RemoteSource, length: impl FnOnce() -> Option<u64>) -> String {
    let smaller = |name: &str| format!("{}&quality={name}", remote.url);
    match chosen() {
        StreamQuality::Original => remote.url.clone(),
        StreamQuality::High => smaller("high"),
        StreamQuality::Normal => smaller("normal"),
        StreamQuality::Automatic => match automatic(remote, length) {
            Some(name) => smaller(name),
            None => remote.url.clone(),
        },
    }
}

/// The smaller version to fetch, by the network: none for the file as it is.
fn automatic(remote: &RemoteSource, length: impl FnOnce() -> Option<u64>) -> Option<&'static str> {
    let kept = cache::kept_of(&remote.url);
    // Whole here already: nothing to wait for.
    if kept.is_some_and(|(len, held)| held >= len) {
        return None;
    }
    let seconds = remote
        .duration
        .map(|d| d.as_secs_f64())
        .filter(|&s| s > 0.0)?;
    // Nothing seen of the network yet: as it is, which tells, without asking its length.
    throughput::seen().as_ref()?;
    // Asked with what was seen let go, which those fetching add to meanwhile.
    let len = match kept {
        Some((len, _)) => len,
        None => length()?,
    };
    let seen = throughput::seen();
    pick(seen.as_ref()?, &remote.url, seconds, len, cache::kept_of)
}

/// Of the file at `url`, `len` bytes playing `seconds`, the smaller version that plays through
/// as `seen` has seen the network, with what `kept_of` says is here of each: none for the file
/// as it is. The smallest when none would; as it is when too little was seen to tell, which
/// tells.
fn pick(
    seen: &Throughput,
    url: &str,
    seconds: f64,
    len: u64,
    kept_of: impl Fn(&str) -> Option<(u64, u64)>,
) -> Option<&'static str> {
    let plays = |url: &str, len: u64, rate: f64| {
        let (len, held) = kept_of(url).unwrap_or((len, 0));
        seen.plays_through(held, len.saturating_sub(held), rate)
            .unwrap_or(true)
    };
    let rate = len as f64 / seconds;
    if plays(url, len, rate) {
        return None;
    }
    let mut smallest = None;
    for (_, name, bytes) in SMALLER {
        let bytes = f64::from(bytes);
        if rate <= bytes * SMALLER_BY {
            // The server would send it as it is: no smaller.
            continue;
        }
        smallest = Some(name);
        let smaller = format!("{url}&quality={name}");
        if plays(&smaller, (bytes * seconds) as u64, bytes) {
            return Some(name);
        }
    }
    smallest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::throughput::seen_at;

    /// Four minutes of CD audio in FLAC: 30 MB, 125 kB a second.
    const SECONDS: f64 = 240.0;
    const FLAC: u64 = 30_000_000;
    const URL: &str = "https://server/audio?path=a.flac";

    fn nothing_kept(_: &str) -> Option<(u64, u64)> {
        None
    }

    #[test]
    fn as_large_as_the_network_plays_through() {
        // Two megabytes a second: as it is.
        let fast = seen_at(&[2_000_000; 60]);
        assert_eq!(pick(&fast, URL, SECONDS, FLAC, nothing_kept), None);
        // 60 kB a second: at 160 kbit/s, 20 kB.
        let slower = seen_at(&[60_000; 60]);
        assert_eq!(
            pick(&slower, URL, SECONDS, FLAC, nothing_kept),
            Some("high")
        );
        // 15 kB a second: at 96 kbit/s, 12 kB.
        let slow = seen_at(&[15_000; 60]);
        assert_eq!(
            pick(&slow, URL, SECONDS, FLAC, nothing_kept),
            Some("normal")
        );
        // 5 kB a second: none plays through, the smallest waits least.
        let slowest = seen_at(&[5_000; 60]);
        assert_eq!(
            pick(&slowest, URL, SECONDS, FLAC, nothing_kept),
            Some("normal")
        );
        // Nothing seen yet: as it is, which tells.
        assert_eq!(
            pick(&Throughput::default(), URL, SECONDS, FLAC, nothing_kept),
            None
        );
    }

    #[test]
    fn what_is_kept_counts() {
        let slower = seen_at(&[60_000; 60]);
        // Most of the file as it is here already.
        let most = |url: &str| (url == URL).then_some((FLAC, FLAC - 1_000_000));
        assert_eq!(pick(&slower, URL, SECONDS, FLAC, most), None);
    }

    #[test]
    fn a_file_small_already_is_not_asked_smaller() {
        // An MP3 of 128 kbit/s, 16 kB a second: no smaller than 1.5 times 12 kB, nor 20.
        let mp3 = (16_000.0 * SECONDS) as u64;
        let slowest = seen_at(&[5_000; 60]);
        assert_eq!(pick(&slowest, URL, SECONDS, mp3, nothing_kept), None);
        // An MP3 of 192 kbit/s, 24 kB a second, is not made 160 kbit/s, but 96.
        let mp3 = (24_000.0 * SECONDS) as u64;
        let slower = seen_at(&[20_000; 60]);
        assert_eq!(
            pick(&slower, URL, SECONDS, mp3, nothing_kept),
            Some("normal")
        );
    }
}
