//! What Wikipedia says of artists, for their pages: their article found by their MusicBrainz
//! id on a thread of its own, the first time a page is shown, and kept in the library with that
//! id for a month. Matched to another artist of the name, an artist's article is asked again.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::SystemTime;

use freya_library::Library;
use freya_library::wikipedia::{self, Article};

/// An artist's name, the id their article was asked by, and the answer; none when Wikipedia
/// could not be asked.
type Answer = (String, String, Option<Option<Article>>);

pub struct Articles {
    /// By the artist's name: the id it was found by, and the article, if they have one.
    known: HashMap<String, (String, Option<Article>)>,
    /// Artists asked about, by their id, while this runs: each once.
    asked: HashSet<(String, String)>,
    /// Those not answered about yet.
    waiting: HashSet<String>,
    results: (Sender<Answer>, Receiver<Answer>),
    /// Answers not kept in the library yet.
    unsaved: Vec<(String, String, Option<Article>, SystemTime)>,
}

impl Articles {
    pub fn new() -> Self {
        Self {
            known: HashMap::new(),
            asked: HashSet::new(),
            waiting: HashSet::new(),
            results: channel(),
            unsaved: Vec::new(),
        }
    }

    /// The article on `name`, the artist with the MusicBrainz id `mbid`: kept in `db` from
    /// before, or once Wikipedia answered. Asks when it is not known for that id, or is old,
    /// if `online`.
    pub fn get(
        &mut self,
        name: &str,
        mbid: &str,
        online: bool,
        db: Option<&Library>,
    ) -> Option<&Article> {
        let known_for = |known: &HashMap<String, (String, Option<Article>)>| {
            known.get(name).is_some_and(|(id, _)| id == mbid)
        };
        if !known_for(&self.known)
            && let Some((id, kept)) = db.and_then(|db| db.artist_article(name).ok().flatten())
            && id == mbid
        {
            if kept.recent() {
                self.asked.insert((name.to_owned(), mbid.to_owned()));
            }
            self.known.insert(name.to_owned(), (id, kept.answer));
        }
        if online && self.asked.insert((name.to_owned(), mbid.to_owned())) {
            self.ask(name, mbid);
        }
        match self.known.get(name) {
            Some((id, article)) if id == mbid => article.as_ref(),
            _ => None,
        }
    }

    /// Takes in answers that came. Returns whether any did.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((name, mbid, article)) = self.results.1.try_recv() {
            self.waiting.remove(&name);
            // Asked again next run.
            let Some(article) = article else {
                continue;
            };
            self.unsaved.push((
                name.clone(),
                mbid.clone(),
                article.clone(),
                SystemTime::now(),
            ));
            self.known.insert(name, (mbid, article));
            any = true;
        }
        any
    }

    /// Keeps the answers that came in `db`.
    pub fn save(&mut self, db: &Library) {
        for (name, mbid, article, asked) in self.unsaved.drain(..) {
            if let Err(e) = db.set_artist_article(&name, &mbid, article.as_ref(), asked) {
                log::warn!("could not keep Wikipedia's article on {name}: {e}");
            }
        }
    }

    /// The artists Wikipedia is being asked about.
    pub fn waiting(&self) -> impl Iterator<Item = &str> {
        self.waiting.iter().map(String::as_str)
    }

    fn ask(&mut self, name: &str, mbid: &str) {
        self.waiting.insert(name.to_owned());
        let results = self.results.0.clone();
        let (name, mbid) = (name.to_owned(), mbid.to_owned());
        let started = std::thread::Builder::new()
            .name("freya-wikipedia".into())
            .spawn(move || {
                let article = match wikipedia::article_of(&mbid) {
                    Ok(article) => Some(article),
                    Err(e) => {
                        log::debug!("no Wikipedia article on {name}: {e}");
                        None
                    }
                };
                let _ = results.send((name, mbid, article));
            });
        if let Err(e) = started {
            log::warn!("could not ask Wikipedia: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_articles_by_the_id_they_were_found_by() {
        let db = Library::open_in_memory().unwrap();
        let article = Article {
            title: "Shining (Norwegian band)".into(),
            summary: "A Norwegian band.".into(),
            biography: "A Norwegian band.".into(),
            url: String::new(),
        };
        let mut articles = Articles::new();
        assert!(articles.get("Shining", "no", false, Some(&db)).is_none());
        articles
            .results
            .0
            .send(("Shining".into(), "no".into(), Some(Some(article.clone()))))
            .unwrap();
        assert!(articles.poll());
        assert_eq!(
            articles.get("Shining", "no", false, Some(&db)),
            Some(&article)
        );
        // Another artist of the name: not theirs.
        assert_eq!(articles.get("Shining", "se", false, Some(&db)), None);
        articles.save(&db);

        // A new run reads it back for that id, and does not ask again.
        let mut again = Articles::new();
        assert_eq!(again.get("Shining", "no", true, Some(&db)), Some(&article));
        assert!(again.waiting.is_empty());
    }
}
