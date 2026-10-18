//! What Last.fm says of artists, for their pages: asked on a thread of its own, the first
//! time a page is shown, and kept in the library for a month, so a page never waits for it.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::SystemTime;

use freya_library::Library;
use freya_library::library::ArtistText;

use crate::lastfm::{ArtistInfo, Client, Credentials};

/// What Last.fm said of an artist, by their name.
type Answer = (String, ArtistInfo);

pub struct ArtistInfos {
    known: HashMap<String, ArtistInfo>,
    /// Artists asked about while this runs, so each is asked once.
    asked: HashSet<String>,
    results: (Sender<Answer>, Receiver<Answer>),
    /// Answers not kept in the library yet, and when they came.
    unsaved: Vec<(String, ArtistInfo, SystemTime)>,
}

impl ArtistInfos {
    pub fn new() -> Self {
        Self {
            known: HashMap::new(),
            asked: HashSet::new(),
            results: channel(),
            unsaved: Vec::new(),
        }
    }

    /// What is known of `name`: kept in `db` from before, or once Last.fm answered. Asks
    /// Last.fm when it is not known or is old, if `online` and this build has Freya's Last.fm
    /// key.
    /// `mbid`, their MusicBrainz id when known, tells Last.fm which artist of the name.
    pub fn get(
        &mut self,
        name: &str,
        mbid: Option<&str>,
        online: bool,
        db: Option<&Library>,
    ) -> Option<&ArtistInfo> {
        if !self.known.contains_key(name)
            && let Some(kept) = db.and_then(|db| db.artist_text(name).ok().flatten())
        {
            if kept.recent() {
                self.asked.insert(name.to_owned());
            }
            self.known.insert(name.to_owned(), info_of(kept.answer));
        }
        if online && self.asked.insert(name.to_owned()) {
            self.ask(name, mbid);
        }
        self.known.get(name)
    }

    /// Forgets what Last.fm said of `name`, here and in `db`, so it is asked again: after
    /// another artist of the name was matched.
    pub fn forget(&mut self, name: &str, db: Option<&Library>) {
        self.known.remove(name);
        self.asked.remove(name);
        self.unsaved.retain(|(n, _, _)| n != name);
        if let Some(db) = db
            && let Err(e) = db.forget_artist_text(name)
        {
            log::warn!("could not forget what Last.fm said of {name}: {e}");
        }
    }

    /// Takes in answers that came. Returns whether any did.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((name, info)) = self.results.1.try_recv() {
            self.unsaved
                .push((name.clone(), info.clone(), SystemTime::now()));
            self.known.insert(name, info);
            any = true;
        }
        any
    }

    /// Keeps the answers that came in `db`.
    pub fn save(&mut self, db: &Library) {
        for (name, info, asked) in self.unsaved.drain(..) {
            let text = ArtistText {
                summary: info.summary,
                biography: info.biography,
                tags: info.tags,
                url: info.url,
            };
            if let Err(e) = db.set_artist_text(&name, &text, asked) {
                log::warn!("could not keep what Last.fm said of {name}: {e}");
            }
        }
    }

    fn ask(&self, name: &str, mbid: Option<&str>) {
        let Some(credentials) = Credentials::of_build() else {
            return;
        };
        let results = self.results.0.clone();
        let (name, mbid) = (name.to_owned(), mbid.map(str::to_owned));
        let started = std::thread::Builder::new()
            .name("freya-artist-info".into())
            .spawn(move || {
                match Client::new(credentials)
                    .artist_info(freya_library::library::plain_name(&name), mbid.as_deref())
                {
                    Ok(info) => {
                        let _ = results.send((name, info));
                    }
                    // Asked again next run.
                    Err(e) => log::debug!("no Last.fm information on {name}: {e:?}"),
                }
            });
        if let Err(e) = started {
            log::warn!("could not ask Last.fm: {e}");
        }
    }
}

fn info_of(text: ArtistText) -> ArtistInfo {
    ArtistInfo {
        summary: text.summary,
        biography: text.biography,
        tags: text.tags,
        url: text.url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_what_last_fm_said_in_the_library() {
        let db = Library::open_in_memory().unwrap();
        let info = ArtistInfo {
            summary: "A Norwegian singer.".into(),
            tags: vec!["folk".into()],
            ..ArtistInfo::default()
        };
        let mut infos = ArtistInfos::new();
        assert!(infos.get("Ane Brun", None, false, Some(&db)).is_none());
        infos
            .results
            .0
            .send(("Ane Brun".into(), info.clone()))
            .unwrap();
        assert!(infos.poll());
        assert_eq!(infos.get("Ane Brun", None, false, Some(&db)), Some(&info));
        assert!(db.artist_text("Ane Brun").unwrap().is_none(), "not yet");
        infos.save(&db);

        // A new run reads it back, and counts it as fresh: Last.fm is not asked again.
        let mut again = ArtistInfos::new();
        assert_eq!(again.get("Ane Brun", None, true, Some(&db)), Some(&info));
        assert!(again.asked.contains("Ane Brun"));
    }
}
