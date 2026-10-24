//! Genres as artists are shown: each with a picture, the user's or a square of its albums'
//! covers, in a grid; and a page over its albums with a wide picture across its top, what the
//! internet or the user says of it, and its most played tracks. The user changes its picture,
//! wide picture and description; the rest is the tags'.

use std::collections::HashMap;
use std::time::Duration;

use freya_media::thumbnails::{ArtistSource, Thumbnails};
use freya_protocol::GenreInfo;
use slint::Image;
use slint_interpreter::{Struct, Value};

use super::playlist_page::{preview, take_file};
use super::{ArtistPage, LibraryPanel, POPULAR_MORE, count, cover_key, long_time, string};

/// The longest side of a genre's own picture, and of its wide one.
const PICTURE: u32 = 1400;
const WIDE: u32 = 3840;

/// The genre being changed, as the dialog has it.
pub(super) struct GenreDraft {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) picture: Option<String>,
    pub(super) backdrop: Option<String>,
    pub(super) picture_shown: Image,
    pub(super) backdrop_shown: Image,
    pub(super) error: String,
}

impl LibraryPanel {
    /// The key of the picture of the genre `name`, for `Library.cover`.
    pub(super) fn genre_key(name: Option<&str>) -> String {
        Thumbnails::key(Some("\u{1}genre"), name)
    }

    /// What the user gave the genre `name`.
    pub(super) fn genre_info_of(&self, name: &str) -> Option<&GenreInfo> {
        self.genre_infos.get(&name.to_lowercase())
    }

    /// The genre `name`'s picture: the user's, else its albums' covers.
    pub(super) fn genre_source(&self, name: Option<&str>) -> ArtistSource {
        let covers = self
            .db
            .as_ref()
            .and_then(|db| db.genre_albums(name).ok())
            .unwrap_or_default()
            .iter()
            .filter_map(|album| self.made_covers.get(&cover_key(album))?.cover.clone())
            .take(4)
            .collect();
        ArtistSource {
            picture: name.and_then(|n| self.genre_info_of(n)?.picture.clone()),
            covers,
        }
    }

    /// The page of the genre `name`, over its albums.
    pub(super) fn load_genre_page(&mut self, name: String, more: bool) {
        let Some(db) = &self.db else {
            return;
        };
        let albums = db.genre_albums(Some(&name)).map(|a| a.len()).unwrap_or(0);
        let tracks = db.genre_tracks(Some(&name)).unwrap_or_default();
        let duration: Duration = tracks.iter().filter_map(|t| t.duration).sum();
        let popular = db.genre_popular(&name, POPULAR_MORE).unwrap_or_default();
        let mut detail = vec![count(albums, "album"), count(tracks.len(), "track")];
        if !duration.is_zero() {
            detail.push(long_time(duration));
        }
        let picture = Self::genre_key(Some(&name));
        self.genre_keys.insert(picture.clone(), Some(name.clone()));
        self.artist_page = Some(ArtistPage {
            genre: true,
            shown_name: name.clone(),
            own_biography: false,
            biography_from: String::new(),
            name: Some(name),
            detail: detail.join(" · "),
            genres: String::new(),
            picture,
            banner: None,
            portrait: None,
            portrait_large: false,
            picture_full: None,
            asked: None,
            color: None,
            summary: String::new(),
            biography: String::new(),
            tags: String::new(),
            popular,
            more,
        });
    }

    /// What is said of the genre `name`: the user's own, else the internet's; its summary,
    /// whole text and where from.
    pub(super) fn genre_text(&self, name: &str) -> (String, String, String, bool) {
        if let Some(own) = self.genre_info_of(name).and_then(|i| i.description.clone()) {
            let summary = own.split("\n\n").next().unwrap_or(&own).trim().to_owned();
            return (summary, own, String::new(), true);
        }
        let article = self
            .db
            .as_ref()
            .and_then(|db| db.genre_article(name).ok().flatten())
            .and_then(|kept| kept.answer);
        match article {
            Some(a) => (a.summary, a.description, a.source, false),
            None => Default::default(),
        }
    }

    /// Every genre the user gave something, from the library.
    pub(super) fn load_genre_infos(&mut self) {
        let Some(db) = &self.db else {
            return;
        };
        let infos = db.genre_infos().unwrap_or_else(|e| {
            log::warn!("could not read what was given the genres: {e}");
            HashMap::new()
        });
        if infos != self.genre_infos {
            // Their pictures are made again.
            let changed: Vec<String> = infos
                .keys()
                .chain(self.genre_infos.keys())
                .filter(|k| infos.get(*k) != self.genre_infos.get(*k))
                .cloned()
                .collect();
            for (key, name) in &self.genre_keys {
                if name
                    .as_ref()
                    .is_some_and(|n| changed.contains(&n.to_lowercase()))
                    && let Some(thumbnails) = &mut self.thumbnails
                {
                    thumbnails.forget_artist(key);
                    self.recent_covers.remove(key);
                }
            }
            self.genre_infos = infos;
            self.covers_ready += 1;
        }
    }

    // ----- Changing a genre --------------------------------------------------------------------

    /// Starts changing the genre whose page is shown.
    pub fn edit_genre(&mut self) {
        let Some(name) = self
            .artist_page
            .as_ref()
            .filter(|page| page.genre)
            .and_then(|page| page.name.clone())
        else {
            return;
        };
        let info = self.genre_info_of(&name).cloned().unwrap_or_default();
        let thumbnails = self.db.as_deref();
        self.genre_draft = Some(GenreDraft {
            description: info
                .description
                .clone()
                .unwrap_or_else(|| self.genre_text(&name).1),
            picture_shown: preview(thumbnails, info.picture.as_ref()),
            backdrop_shown: preview(thumbnails, info.backdrop.as_ref()),
            picture: info.picture,
            backdrop: info.backdrop,
            error: String::new(),
            name,
        });
    }

    /// Takes the picture in `file` for the genre being changed: "picture" or "backdrop".
    pub fn pick_genre_picture(&mut self, kind: &str, file: &std::path::Path) {
        let (Some(draft), Some(api)) = (&mut self.genre_draft, &self.db) else {
            return;
        };
        let largest = if kind == "backdrop" { WIDE } else { PICTURE };
        match take_file(api.as_ref(), file, largest) {
            Ok(checksum) => {
                draft.error.clear();
                let shown = preview(Some(api.as_ref()), Some(&checksum));
                match kind {
                    "backdrop" => (draft.backdrop, draft.backdrop_shown) = (Some(checksum), shown),
                    _ => (draft.picture, draft.picture_shown) = (Some(checksum), shown),
                }
            }
            Err(e) => draft.error = format!("Could not use that file: {e}"),
        }
    }

    pub fn clear_genre_picture(&mut self, kind: &str) {
        if let Some(draft) = &mut self.genre_draft {
            match kind {
                "backdrop" => (draft.backdrop, draft.backdrop_shown) = (None, Image::default()),
                _ => (draft.picture, draft.picture_shown) = (None, Image::default()),
            }
        }
    }

    pub fn cancel_genre(&mut self) {
        self.genre_draft = None;
    }

    /// Keeps the pictures and description of the genre being changed. A description the
    /// same as the internet's stays the internet's.
    pub fn save_genre(&mut self, description: &str) {
        let Some(draft) = self.genre_draft.take() else {
            return;
        };
        let (_, found, _, was_own) = self.genre_text(&draft.name);
        let own = was_own || description.trim() != found.trim();
        let info = GenreInfo {
            picture: draft.picture,
            backdrop: draft.backdrop,
            description: own.then(|| description.to_owned()),
        };
        if let Some(db) = &self.db
            && let Err(e) = db.set_genre_info(&draft.name, &info)
        {
            log::warn!("could not keep what was changed about {}: {e}", draft.name);
        }
        self.load_genre_infos();
        if let Some(page) = &mut self.artist_page {
            page.banner = None;
            page.portrait_large = false;
            page.color = None;
            page.asked = None;
        }
        self.reload();
    }

    /// The genre being changed, for `Library.genre-draft`.
    pub fn genre_draft_value(&self) -> Value {
        let draft = self.genre_draft.as_ref();
        let picture =
            |f: fn(&GenreDraft) -> &Image| Value::Image(draft.map(f).cloned().unwrap_or_default());
        Value::Struct(Struct::from_iter([
            ("shown".to_owned(), Value::Bool(draft.is_some())),
            (
                "name".to_owned(),
                string(draft.map(|d| d.name.clone()).unwrap_or_default()),
            ),
            (
                "description".to_owned(),
                string(draft.map(|d| d.description.clone()).unwrap_or_default()),
            ),
            ("picture".to_owned(), picture(|d| &d.picture_shown)),
            ("backdrop".to_owned(), picture(|d| &d.backdrop_shown)),
            (
                "error".to_owned(),
                string(draft.map(|d| d.error.clone()).unwrap_or_default()),
            ),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use freya_library::Library;
    use freya_protocol::GenreArticle;

    use super::*;

    #[test]
    fn a_genre_says_what_the_internet_says_until_the_user_changes_it() {
        let db = Library::open_in_memory().unwrap();
        let article = GenreArticle {
            summary: "Ambient is a genre.".into(),
            description: "Ambient is a genre.\n\nMore.".into(),
            url: String::new(),
            source: "Wikipedia".into(),
        };
        db.set_genre_article("Ambient", Some(&article), SystemTime::now())
            .unwrap();
        let mut panel = LibraryPanel::new(None, None);
        panel.open(crate::gui::library::tests::local(db, None));
        panel.load_genre_page("Ambient".into(), false);
        let page = panel.artist_page.as_ref().unwrap();
        assert!(page.genre);
        assert_eq!(page.detail, "0 albums · 0 tracks");
        assert_eq!(
            panel.genre_text("Ambient"),
            (
                "Ambient is a genre.".into(),
                article.description.clone(),
                "Wikipedia".into(),
                false
            )
        );

        // Saved unchanged, it stays the internet's.
        panel.edit_genre();
        panel.save_genre(&article.description);
        assert!(panel.genre_info_of("Ambient").is_none());

        panel.load_genre_page("Ambient".into(), false);
        panel.edit_genre();
        panel.save_genre("Music for rooms.");
        assert_eq!(
            panel.genre_text("ambient"),
            (
                "Music for rooms.".into(),
                "Music for rooms.".into(),
                String::new(),
                true
            )
        );
    }
}
