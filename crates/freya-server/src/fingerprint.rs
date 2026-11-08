//! Knowing music by its sound: a fingerprint of a file's first two minutes, which AcoustID knows
//! the recordings and releases of, for files whose tags say too little to find them.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use freya_media::artist_image::get_json;
use freya_transcode::samples_of;
use rusty_chromaprint::{Configuration, FingerprintCompressor, Fingerprinter};
use serde_json::Value;

const ACOUSTID_LOOKUP: &str = "https://api.acoustid.org/v2/lookup";
/// How much of a file a fingerprint is made from, as AcoustID's own tools do.
const HEARD: Duration = Duration::from_secs(120);
/// AcoustID asks for three questions a second at most.
const ASK_EVERY: Duration = Duration::from_millis(340);
/// How sure AcoustID must be that it is the recording.
const SURE: f64 = 0.7;

/// A file's fingerprint as AcoustID reads it.
pub fn fingerprint(path: &Path) -> Result<String, String> {
    let (samples, rate, channels) = samples_of(path, HEARD).map_err(|e| e.to_string())?;
    let config = Configuration::preset_test2();
    let mut printer = Fingerprinter::new(&config);
    printer
        .start(rate, channels)
        .map_err(|e| format!("{e:?}"))?;
    printer.consume(&samples);
    printer.finish();
    let compressed = FingerprintCompressor::from(&config).compress(printer.fingerprint());
    Ok(base64_url(&compressed))
}

/// `bytes` in base 64 for addresses, without padding.
fn base64_url(bytes: &[u8]) -> String {
    const LETTERS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 2);
    for chunk in bytes.chunks(3) {
        let group = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for at in 0..=chunk.len() {
            out.push(LETTERS[(group >> (18 - 6 * at)) as usize & 63] as char);
        }
    }
    out
}

/// The releases in AcoustID's answer it is sure of.
pub fn releases_in(answer: &Value) -> Vec<String> {
    let mut releases = Vec::new();
    for result in answer
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if result.get("score").and_then(Value::as_f64).unwrap_or(0.0) < SURE {
            continue;
        }
        for recording in result
            .get("recordings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for release in recording
                .get("releases")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(id) = release.get("id").and_then(Value::as_str) {
                    releases.push(id.to_owned());
                }
            }
        }
    }
    releases.sort();
    releases.dedup();
    releases
}

/// The releases most files are on first, of what was found of each file.
pub fn most_shared(found: Vec<Vec<String>>) -> Vec<String> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for releases in found {
        for release in releases {
            *counts.entry(release).or_default() += 1;
        }
    }
    let mut releases: Vec<(String, u32)> = counts.into_iter().collect();
    releases.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    releases.into_iter().map(|(release, _)| release).collect()
}

/// AcoustID, asked no faster than it wants to be, with the user's key.
#[derive(Default)]
pub struct AcoustId {
    last: Mutex<Option<Instant>>,
}

impl AcoustId {
    /// The releases the file at `path` may be on.
    pub fn releases(
        &self,
        key: &str,
        path: &Path,
        duration: Duration,
    ) -> Result<Vec<String>, String> {
        let printed = fingerprint(path)?;
        {
            let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(wait) = last.and_then(|at| ASK_EVERY.checked_sub(at.elapsed())) {
                std::thread::sleep(wait);
            }
            *last = Some(Instant::now());
        }
        let seconds = duration.as_secs().to_string();
        let answer = get_json(
            ACOUSTID_LOOKUP,
            &[
                ("client", key),
                ("duration", &seconds),
                ("fingerprint", &printed),
                ("meta", "recordings releaseids"),
            ],
        )?;
        if answer.get("status").and_then(Value::as_str) != Some("ok") {
            let why = answer
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("it said no");
            return Err(format!("AcoustID: {why}"));
        }
        Ok(releases_in(&answer))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../freya-decode/tests/fixtures")
            .join(name)
    }

    #[test]
    fn the_same_music_gives_the_same_fingerprint() {
        let first = fingerprint(&fixture("sine.flac")).unwrap();
        assert!(!first.is_empty());
        assert_eq!(first, fingerprint(&fixture("sine.flac")).unwrap());
    }

    #[test]
    fn encodes_as_addresses_want_it() {
        assert_eq!(base64_url(b"Man"), "TWFu");
        assert_eq!(base64_url(b"Ma"), "TWE");
        assert_eq!(base64_url(b"M"), "TQ");
        assert_eq!(base64_url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn takes_the_releases_it_is_sure_of() {
        let answer = json!({ "status": "ok", "results": [
            { "score": 0.95, "recordings": [
                { "releases": [{ "id": "b" }, { "id": "a" }] },
                { "releases": [{ "id": "a" }] },
            ] },
            { "score": 0.2, "recordings": [{ "releases": [{ "id": "doubtful" }] }] },
        ] });
        assert_eq!(releases_in(&answer), ["a", "b"]);
    }

    #[test]
    fn the_release_most_files_are_on_comes_first() {
        let found = vec![
            vec!["single".to_owned(), "album".to_owned()],
            vec!["album".to_owned()],
            vec!["album".to_owned(), "other".to_owned()],
        ];
        assert_eq!(most_shared(found)[0], "album");
    }
}
