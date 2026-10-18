//! What Last.fm says of artists, for their pages: asked on a thread of its own, the first
//! time a page is shown, and kept on disk for a month, so a page never waits for it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, SystemTime};

use crate::lastfm::{ArtistInfo, Client, Credentials};

/// What Last.fm said of an artist, by their name.
type Answer = (String, ArtistInfo);

/// How long what Last.fm said is kept before it is asked again.
const KEEP: Duration = Duration::from_secs(30 * 24 * 3600);

#[derive(serde::Serialize, serde::Deserialize)]
struct Kept {
    /// When Last.fm said it, in seconds since 1970.
    asked: u64,
    info: ArtistInfo,
}

pub struct ArtistInfos {
    /// Where answers are kept; none keeps them only while Freya runs.
    dir: Option<PathBuf>,
    known: HashMap<String, ArtistInfo>,
    /// Artists asked about while this runs, so each is asked once.
    asked: HashSet<String>,
    results: (Sender<Answer>, Receiver<Answer>),
}

impl ArtistInfos {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            known: HashMap::new(),
            asked: HashSet::new(),
            results: channel(),
        }
    }

    /// What is known of `name`: kept from before, or once Last.fm answered. Asks Last.fm when
    /// it is not known or is old, if `online` and this build has Freya's Last.fm key.
    pub fn get(&mut self, name: &str, online: bool) -> Option<&ArtistInfo> {
        if !self.known.contains_key(name)
            && let Some((info, fresh)) = self.read(name)
        {
            self.known.insert(name.to_owned(), info);
            if fresh {
                self.asked.insert(name.to_owned());
            }
        }
        if online && self.asked.insert(name.to_owned()) {
            self.ask(name);
        }
        self.known.get(name)
    }

    /// Takes in answers that came. Returns whether any did.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((name, info)) = self.results.1.try_recv() {
            self.write(&name, &info);
            self.known.insert(name, info);
            any = true;
        }
        any
    }

    fn ask(&self, name: &str) {
        let Some(credentials) = Credentials::of_build() else {
            return;
        };
        let results = self.results.0.clone();
        let name = name.to_owned();
        let started = std::thread::Builder::new()
            .name("freya-artist-info".into())
            .spawn(move || match Client::new(credentials).artist_info(&name) {
                Ok(info) => {
                    let _ = results.send((name, info));
                }
                // Asked again next run.
                Err(e) => log::debug!("no Last.fm information on {name}: {e:?}"),
            });
        if let Err(e) = started {
            log::warn!("could not ask Last.fm: {e}");
        }
    }

    fn file(&self, name: &str) -> Option<PathBuf> {
        let key = freya_library::thumbnails::Thumbnails::key(Some("\u{1}lastfm"), Some(name));
        Some(self.dir.as_ref()?.join(format!("{key}.json")))
    }

    /// What was kept of `name`, and whether it is recent enough not to ask again.
    fn read(&self, name: &str) -> Option<(ArtistInfo, bool)> {
        let text = std::fs::read_to_string(self.file(name)?).ok()?;
        let kept: Kept = serde_json::from_str(&text).ok()?;
        let asked = SystemTime::UNIX_EPOCH + Duration::from_secs(kept.asked);
        let fresh = asked.elapsed().is_ok_and(|age| age < KEEP);
        Some((kept.info, fresh))
    }

    fn write(&self, name: &str, info: &ArtistInfo) {
        let Some(file) = self.file(name) else {
            return;
        };
        let asked = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let kept = Kept {
            asked,
            info: info.clone(),
        };
        let written = file
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                serde_json::to_vec(&kept)
                    .map_err(std::io::Error::other)
                    .and_then(|bytes| crate::settings::write_atomically(&file, &bytes))
            });
        if let Err(e) = written {
            log::debug!("could not keep what Last.fm said of {name}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_what_last_fm_said_between_runs() {
        let dir = std::env::temp_dir().join(format!("freya-artist-info-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let info = ArtistInfo {
            summary: "A Norwegian singer.".into(),
            tags: vec!["folk".into()],
            ..ArtistInfo::default()
        };
        let mut infos = ArtistInfos::new(Some(dir.clone()));
        assert!(infos.get("Ane Brun", false).is_none());
        infos
            .results
            .0
            .send(("Ane Brun".into(), info.clone()))
            .unwrap();
        assert!(infos.poll());
        assert_eq!(infos.get("Ane Brun", false), Some(&info));

        // A new run reads it back, and counts it as fresh: Last.fm is not asked again.
        let mut again = ArtistInfos::new(Some(dir.clone()));
        assert_eq!(again.get("Ane Brun", true), Some(&info));
        assert!(again.asked.contains("Ane Brun"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
