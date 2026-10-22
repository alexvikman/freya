//! Answers kept: the library's answers to the calls that only read it, made once for each
//! generation of the library and given again, each with the tag a client asks with to be
//! told it has the answer already.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// The most kept, all told: a few times the largest answer of a large library.
const MOST_KEPT: usize = 64 * 1024 * 1024;
/// How hard an answer kept is compressed: well, as it is compressed once.
const GZIP_LEVEL: u32 = 6;

/// An answer, as JSON, and its tag: the first half of its SHA-256, so the same answer has
/// the same tag in every generation of the library.
pub struct Answer {
    pub json: Vec<u8>,
    pub tag: String,
    gzipped: OnceLock<Option<Vec<u8>>>,
}

impl Answer {
    pub fn new(json: Vec<u8>) -> Self {
        let digest = ring::digest::digest(&ring::digest::SHA256, &json);
        let tag = digest.as_ref()[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        Self {
            json,
            tag: format!("\"{tag}\""),
            gzipped: OnceLock::new(),
        }
    }

    /// The answer gzipped, made the first time it is asked for.
    pub fn gzipped(&self) -> Option<&[u8]> {
        self.gzipped
            .get_or_init(|| {
                let mut zipped = flate2::write::GzEncoder::new(
                    Vec::with_capacity(self.json.len() / 6),
                    flate2::Compression::new(GZIP_LEVEL),
                );
                std::io::Write::write_all(&mut zipped, &self.json).ok()?;
                zipped.finish().ok()
            })
            .as_deref()
    }
}

/// The answers of one generation of the library, by call and arguments.
#[derive(Default)]
pub struct Answers(Mutex<Kept>);

#[derive(Default)]
struct Kept {
    generation: Option<u64>,
    answers: HashMap<(String, Vec<u8>), Arc<Answer>>,
    bytes: usize,
}

impl Answers {
    /// The answer to `method` with `args` kept in `generation`.
    pub fn get(&self, generation: u64, method: &str, args: &[u8]) -> Option<Arc<Answer>> {
        let kept = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if kept.generation != Some(generation) {
            return None;
        }
        kept.answers
            .get(&(method.to_owned(), args.to_vec()))
            .map(Arc::clone)
    }

    /// Keeps `json`, the answer to `method` with `args` in `generation`: the answers of any
    /// other generation go, and when there is no room, all.
    pub fn keep(&self, generation: u64, method: &str, args: &[u8], json: Vec<u8>) -> Arc<Answer> {
        let answer = Arc::new(Answer::new(json));
        let mut kept = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if kept.generation != Some(generation) {
            *kept = Kept {
                generation: Some(generation),
                ..Kept::default()
            };
        }
        let size = answer.json.len() + method.len() + args.len();
        if kept.bytes + size > MOST_KEPT {
            kept.answers.clear();
            kept.bytes = 0;
        }
        if size <= MOST_KEPT {
            kept.bytes += size;
            let old = kept
                .answers
                .insert((method.to_owned(), args.to_vec()), Arc::clone(&answer));
            if let Some(old) = old {
                kept.bytes -= old.json.len() + method.len() + args.len();
            }
        }
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_kept_for_its_generation() {
        let answers = Answers::default();
        assert!(answers.get(1, "stats", b"[]").is_none());
        let kept = answers.keep(1, "stats", b"[]", b"{\"tracks\":1}".to_vec());
        assert_eq!(answers.get(1, "stats", b"[]").unwrap().tag, kept.tag);
        assert!(answers.get(1, "stats", b"[1]").is_none());
        assert!(answers.get(2, "stats", b"[]").is_none());
        // The same answer, in another generation, has the same tag.
        let again = answers.keep(2, "stats", b"[]", b"{\"tracks\":1}".to_vec());
        assert_eq!(again.tag, kept.tag);
        assert!(answers.get(1, "stats", b"[]").is_none());
        let other = answers.keep(2, "all_tracks", b"[]", b"[]".to_vec());
        assert_ne!(other.tag, kept.tag);
    }

    #[test]
    fn a_kept_answer_unzips_to_itself() {
        let json = br#"[{"title":"a"},{"title":"a"},{"title":"a"}]"#.repeat(1000);
        let answer = Answer::new(json.clone());
        let zipped = answer.gzipped().unwrap();
        assert!(zipped.len() < json.len() / 10);
        let mut unzipped = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(zipped), &mut unzipped)
            .unwrap();
        assert_eq!(unzipped, json);
    }
}
