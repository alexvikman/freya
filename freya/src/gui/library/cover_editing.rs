//! Choosing an album's cover: what it shows now, each of its editions' covers and fanart.tv's,
//! the user's own picture or one from an address; kept by the server over what the music and
//! the internet give, or forgotten again, back to them.

use std::path::Path;

use slint::Image;

use super::*;
use crate::gui::cover_finder::{CoverRequest, FoundCover};

/// The album whose cover is being chosen: its key, and its artist and title as the library
/// files it.
pub(super) struct CoverDraft {
    key: String,
    artist: Option<String>,
    album: Option<String>,
    /// The cover the user had chosen when they began, if any.
    chosen_before: Option<String>,
    /// What the album shows without a choice, its music's or the internet's: known when no
    /// other was chosen.
    own: Image,
}

/// A cover to choose.
pub(super) struct CoverChoice {
    checksum: String,
    source: String,
    size: String,
    image: Image,
    color: [u8; 3],
    order: usize,
}

impl LibraryPanel {
    /// Starts choosing the cover of the album whose page is shown.
    pub fn edit_cover(&mut self) {
        let Level::AlbumTracks { artist, album } = self.level().clone() else {
            return;
        };
        let Some(db) = self.db.clone() else {
            return;
        };
        // As the page shows the album's cover.
        let Some(key) = self.album_page.as_ref().map(|page| page.cover.clone()) else {
            return;
        };
        let made = self.made_covers.get(&key);
        let shown = made.and_then(|made| made.cover.clone());
        let chosen_before = made.filter(|made| made.chosen).and(shown.clone());
        let group = match (&artist, &album) {
            (Some(artist), Some(album)) => db
                .album_match(artist, album)
                .ok()
                .flatten()
                .and_then(|matched| matched.release_group),
            _ => None,
        };
        let own = if chosen_before.is_none() && shown.is_some() {
            self.cover(&key)
        } else {
            Image::default()
        };
        self.cover_choices.clear();
        self.cover_picked = chosen_before.clone();
        self.picture_error.clear();
        self.cover_draft = Some(CoverDraft {
            key,
            artist: artist.clone(),
            album: album.clone(),
            chosen_before: chosen_before.clone(),
            own,
        });
        // The one chosen before, among the others; the album's own is the first tile.
        let kept = chosen_before
            .iter()
            .map(|checksum| (checksum.clone(), "Chosen before".to_owned()))
            .collect();
        self.cover_finder.find(CoverRequest {
            title: album.unwrap_or_default(),
            artist: artist
                .as_deref()
                .map(plain_name)
                .unwrap_or_default()
                .to_owned(),
            group,
            fanart_key: self.settings.fanart_key.clone(),
            kept,
            online: !cfg!(test),
        });
        self.show_cover_choices();
    }

    /// Chooses the `index`th cover; the first is the album's own, its music's or the
    /// internet's.
    pub fn choose_cover(&mut self, index: usize) {
        self.cover_picked = match index {
            0 => None,
            i => match self.cover_choices.get(i - 1) {
                Some(choice) => Some(choice.checksum.clone()),
                None => return,
            },
        };
        self.show_cover_choices();
    }

    /// Adds the picture in `file` as a cover to choose, and chooses it.
    pub fn add_cover_file(&mut self, file: &Path) {
        let added = std::fs::read(file)
            .map_err(|e| e.to_string())
            .and_then(|bytes| self.cover_finder.add(&bytes));
        match added {
            Ok(found) => {
                self.picture_error.clear();
                self.add_cover_choice(found);
            }
            Err(e) => self.picture_error = format!("Could not use that file: {e}"),
        }
        self.show_cover_choices();
    }

    /// Downloads the picture at `url` as a cover to choose; it is chosen when it comes.
    pub fn cover_from_url(&mut self, url: &str) {
        let url = url.trim();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            self.picture_error = "An address starts with https://".to_owned();
            return;
        }
        self.picture_error.clear();
        self.cover_finder.download(url.to_owned());
    }

    /// Keeps the cover chosen, and shows it.
    pub fn save_cover(&mut self) {
        let Some(draft) = self.cover_draft.take() else {
            return;
        };
        self.cover_finder.stop();
        if self.cover_picked != draft.chosen_before
            && let Some(db) = &self.db
        {
            let color = self.cover_picked.as_ref().and_then(|checksum| {
                self.cover_choices
                    .iter()
                    .find(|c| &c.checksum == checksum)
                    .map(|c| c.color)
            });
            match db.set_chosen_album_cover(&draft.key, self.cover_picked.as_deref(), color) {
                Ok(()) => {
                    log::info!(
                        "{} the cover of {} by {}",
                        if self.cover_picked.is_some() {
                            "chose"
                        } else {
                            "forgot the chosen"
                        },
                        draft.album.as_deref().unwrap_or("an untitled album"),
                        draft.artist.as_deref().unwrap_or("no artist")
                    );
                    // Shown by `reload`, which reads the albums' covers again; the page's whole
                    // cover is read again when asked for.
                    self.pictures_stale = true;
                    if let Some(page) = self.album_page.as_mut() {
                        page.cover_full = None;
                    }
                }
                Err(e) => log::warn!("could not keep the cover chosen: {e}"),
            }
        }
        self.end_cover_edit();
        self.reload();
    }

    pub fn cancel_cover(&mut self) {
        self.cover_finder.stop();
        self.cover_draft = None;
        self.end_cover_edit();
    }

    fn end_cover_edit(&mut self) {
        self.cover_choices.clear();
        self.cover_picked = None;
        self.picture_error.clear();
        self.show_cover_choices();
    }

    /// Covers found since the last tick, while one is being chosen.
    pub(super) fn take_found_covers(&mut self) {
        if self.cover_draft.is_none() {
            return;
        }
        let (found, failed) = self.cover_finder.poll();
        if let Some(failed) = failed {
            self.picture_error = failed;
        }
        if !found.is_empty() {
            for found in found {
                self.add_cover_choice(found);
            }
            self.show_cover_choices();
        }
    }

    fn add_cover_choice(&mut self, found: FoundCover) {
        if found.choose {
            self.cover_picked = Some(found.checksum.clone());
        }
        if self
            .cover_choices
            .iter()
            .any(|c| c.checksum == found.checksum)
        {
            return;
        }
        let at = self
            .cover_choices
            .partition_point(|c| c.order <= found.order);
        self.cover_choices.insert(
            at,
            CoverChoice {
                checksum: found.checksum,
                source: found.source,
                size: format!("{} × {}", found.width, found.height),
                image: Image::from_rgba8(found.preview),
                color: found.color,
                order: found.order,
            },
        );
    }

    /// Gives the skin the covers to choose from: the album's own first.
    pub(super) fn show_cover_choices(&mut self) {
        let row = |image: Image, source: &str, size: &str, chosen: bool| {
            Value::Struct(Struct::from_iter([
                ("picture".to_owned(), Value::Image(image)),
                ("source".to_owned(), string(source)),
                ("size".to_owned(), string(size)),
                ("chosen".to_owned(), Value::Bool(chosen)),
            ]))
        };
        let rows: Vec<Value> = match &self.cover_draft {
            None => Vec::new(),
            Some(draft) => std::iter::once(row(
                draft.own.clone(),
                "The album's own",
                "",
                self.cover_picked.is_none(),
            ))
            .chain(self.cover_choices.iter().map(|c| {
                row(
                    c.image.clone(),
                    &c.source,
                    &c.size,
                    self.cover_picked.as_ref() == Some(&c.checksum),
                )
            }))
            .collect(),
        };
        self.cover_choice_rows.sync(rows);
    }

    /// Whether covers are being looked for, for `Library.finding-pictures`.
    pub(super) fn finding_covers(&self) -> bool {
        self.cover_draft.is_some() && self.cover_finder.searching()
    }
}
