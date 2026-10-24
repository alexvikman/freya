//! Editing an artist or an album: matching them with MusicBrainz, and choosing pictures.

use super::*;

/// A picture to make ahead of its being shown.
pub(super) enum Prepare {
    Artist(Option<String>),
    /// An album's cover, by its checksum.
    Cover(String),
}

/// Pictures handed to the threads to make ahead, each time the library is polled.
pub(super) const PREPARE_AT_ONCE: usize = 8;

/// A picture the user can choose for an artist.
pub(super) struct Choice {
    pub(super) kind: PictureKind,
    pub(super) pick: Pick,
    pub(super) source: String,
    pub(super) size: String,
    pub(super) image: Image,
    /// Where it goes among those found.
    pub(super) order: usize,
}

/// What tells an artist MusicBrainz knows from others: "Group · Norway · 1999–".
pub(super) fn match_facts(artist: &freya_protocol::ArtistMatch) -> String {
    let years = match (artist.begin.get(..4), artist.end.get(..4)) {
        (Some(begin), Some(end)) if begin == end => begin.to_owned(),
        (Some(begin), end) => format!("{begin}–{}", end.unwrap_or("")),
        (None, _) => String::new(),
    };
    [artist.kind.as_str(), artist.area.as_str(), years.as_str()]
        .into_iter()
        .filter(|fact| !fact.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A kind of picture by its name in the skin API.
pub(super) fn picture_kind(name: &str) -> Option<PictureKind> {
    match name {
        "portrait" => Some(PictureKind::Portrait),
        "backdrop" => Some(PictureKind::Backdrop),
        _ => None,
    }
}

/// A folder as the scan shows it: its parent and its own name, "Peter Gabriel / So", which is
/// mostly the artist and the album.
pub(super) fn folder_name(folder: &Path) -> String {
    let mut names: Vec<String> = folder
        .components()
        .rev()
        .filter_map(|c| match c {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .take(2)
        .collect();
    names.reverse();
    names.join(" / ")
}

impl LibraryPanel {
    /// Starts changing the artist whose page is shown: the form gets what was changed before.
    pub fn edit_artist(&mut self) {
        self.draft = self.artist_page.as_ref().map(|page| page.name.clone());
        // What the internet says, which their own starts from.
        let found = self.draft.clone().flatten().map(|name| {
            let own = self
                .edits
                .get_mut(&name)
                .and_then(|edit| edit.biography.take());
            let (_, biography, _, from) = self.biography_of(&name);
            if let Some(edit) = self.edits.get_mut(&name) {
                edit.biography = own;
            }
            (biography, from)
        });
        (self.found_biography, self.found_biography_from) = found.unwrap_or_default();
        self.choices.clear();
        self.none_shown.clear();
        self.forget_matches();
        self.picked.clear();
        self.picked_before.clear();
        self.picture_error.clear();
        self.finder.stop();
        if let Some(name) = self.artist_page.as_ref().and_then(|p| p.name.clone()) {
            let (request, picked) = self.picture_request(&name);
            self.picked_before.clone_from(&picked);
            self.picked = picked;
            self.finder.find(request);
        }
        self.show_choices();
    }

    /// What to look for to choose an artist's pictures from, and what is chosen now.
    pub(super) fn picture_request(&self, name: &str) -> (Request, HashMap<PictureKind, Pick>) {
        let mut picked = HashMap::new();
        let mut kept = Vec::new();
        for kind in [PictureKind::Portrait, PictureKind::Backdrop] {
            let row = self
                .db
                .as_ref()
                .and_then(|db| db.artist_picture(name, kind).ok().flatten());
            let pick = match &row {
                Some(PictureRow {
                    picture: Some(checksum),
                    ..
                }) => Pick::Kept(checksum.clone()),
                _ => Pick::Nothing,
            };
            if let Some(PictureRow {
                picture: Some(checksum),
                source,
                ..
            }) = row
            {
                let source = match source.as_str() {
                    "deezer" => "Deezer",
                    "fanart.tv" => "fanart.tv",
                    freya_protocol::FOLDER => "Folder",
                    _ => "Chosen before",
                };
                kept.push((kind, checksum, source.to_owned()));
            }
            picked.insert(kind, pick);
        }
        let request = Request {
            name: freya_protocol::plain_name(name).to_owned(),
            deezer: self.settings.artist_pictures_online,
            mbid: self.known_mbid(Some(name)),
            search: true,
            fanart_key: self.settings.fanart_key.clone(),
            kept,
            covers: self.artist_source(Some(name)).covers,
        };
        (request, picked)
    }

    /// Leaves the artist being changed as they were.
    pub fn cancel_artist(&mut self) {
        self.draft = None;
        self.finder.stop();
        self.choices.clear();
        self.show_choices();
        self.forget_matches();
    }

    /// Starts matching the album shown, searching MusicBrainz by its title and artist.
    pub fn start_album_match(&mut self) {
        let Level::AlbumTracks { artist, album } = self.level().clone() else {
            return;
        };
        self.album_draft = Some((artist.clone(), album.clone()));
        self.album_candidates.clear();
        self.album_covers.clear();
        self.album_match_error.clear();
        self.show_album_matches();
        if let Some(title) = album {
            let artist = artist.as_deref().map(plain_name).unwrap_or_default();
            self.album_finder.find(title, artist.to_owned());
        }
    }

    /// Searches MusicBrainz again for the album being matched, by `title` and `artist`.
    pub fn find_album_matches(&mut self, title: &str, artist: &str) {
        if self.album_draft.is_none() || title.trim().is_empty() {
            return;
        }
        self.album_match_error.clear();
        self.album_candidates.clear();
        self.show_album_matches();
        self.album_finder
            .find(title.trim().to_owned(), artist.trim().to_owned());
    }

    pub fn cancel_album_match(&mut self) {
        self.album_draft = None;
        self.album_finder.stop();
        self.album_candidates.clear();
        self.album_covers.clear();
        self.show_album_matches();
    }

    /// Matches the album being matched to the one found of the release group `group`; empty
    /// files it as its tags say again.
    pub fn save_album_match(&mut self, group: &str) {
        let group = group.trim();
        if group.is_empty() {
            self.unmatch_album();
            return;
        }
        match self
            .album_candidates
            .iter()
            .position(|album| album.release_group == group)
        {
            Some(index) => self.choose_album_match(index),
            // The match it had: nothing changes.
            None => self.cancel_album_match(),
        }
    }

    /// Matches the album being matched to the `index`th album found. Made by another artist
    /// than the one it is filed under, even one of the same name, it is filed under that
    /// artist, who is given their name, id and MusicBrainz's words for them.
    pub(super) fn choose_album_match(&mut self, index: usize) {
        let (Some((Some(filed), Some(album))), Some(chosen)) = (
            self.album_draft.clone(),
            self.album_candidates.get(index).cloned(),
        ) else {
            return;
        };
        let Some(db) = &self.db else {
            return;
        };
        // As the tags name it.
        let tagged = db
            .album_match(&filed, &album)
            .ok()
            .flatten()
            .map_or_else(|| filed.clone(), |m| m.album_artist);
        let mbid = &chosen.artist.mbid;
        let theirs = self
            .edits
            .get(&tagged)
            .and_then(|e| e.mbid.clone())
            .or_else(|| self.found_mbid(Some(&tagged)));
        // An artist without an id yet is the one matched; one with another, another artist.
        let key = theirs
            .as_deref()
            .is_some_and(|theirs| theirs != mbid)
            .then(|| split_artist(&tagged, mbid));
        let matched = AlbumMatch {
            album_artist: tagged.clone(),
            album: album.clone(),
            release_group: Some(chosen.release_group.clone()),
            artist_mbid: mbid.clone(),
            artist_key: key.clone(),
        };
        let Some(db) = &mut self.db else {
            return;
        };
        if let Err(e) = db.match_album(&matched) {
            log::warn!("could not match {album}: {e}");
            return;
        }
        let named = match &key {
            Some(key) if !self.edits.contains_key(key) => Some((
                key.clone(),
                ArtistEdit {
                    name: Some(chosen.artist.name.clone()),
                    ..ArtistEdit::default()
                },
            )),
            None if theirs.is_none() => Some((
                tagged.clone(),
                self.edits.get(&tagged).cloned().unwrap_or_default(),
            )),
            _ => None,
        };
        if let Some((artist, edit)) = named {
            let edit = ArtistEdit {
                mbid: Some(mbid.clone()),
                note: Some(chosen.artist.disambiguation.clone()),
                ..edit
            };
            if let Err(e) = db.set_artist_edit(&artist, &edit) {
                log::warn!("could not keep who made {album}: {e}");
            }
        }
        self.cancel_album_match();
        self.show_album(Some(key.unwrap_or(tagged)), Some(album));
        // Its cover from the Cover Art Archive, when its music has none.
        self.fetch(None);
    }

    /// Files the album being matched as its tags say again.
    pub(super) fn unmatch_album(&mut self) {
        let Some((Some(filed), Some(album))) = self.album_draft.clone() else {
            return;
        };
        let Some(db) = &mut self.db else {
            return;
        };
        let tagged = match db.album_match(&filed, &album) {
            Ok(Some(matched)) => matched.album_artist,
            _ => filed,
        };
        if let Err(e) = db.unmatch_album(&tagged, &album) {
            log::warn!("could not unmatch {album}: {e}");
        }
        self.cancel_album_match();
        self.show_album(Some(tagged), Some(album));
    }

    /// Opens the album `album` filed under `artist`, under their page.
    pub(super) fn show_album(&mut self, artist: Option<String>, album: Option<String>) {
        self.opened = vec![
            Level::Artists,
            Level::ArtistAlbums {
                artist: artist.clone(),
            },
            Level::AlbumTracks { artist, album },
        ];
        self.query.clear();
        self.reload();
    }

    /// The album being matched, as `album-draft` gives it.
    pub(super) fn album_draft_value(&self) -> Value {
        let (artist, title) = self.album_draft.clone().unwrap_or_default();
        let matched = match (&self.db, &artist, &title) {
            (Some(db), Some(artist), Some(title)) => db.album_match(artist, title).ok().flatten(),
            _ => None,
        };
        let folder = self
            .items
            .iter()
            .find_map(Item::track)
            .and_then(|t| t.path.parent())
            .map(|p| p.display().to_string())
            .filter(|_| self.album_draft.is_some())
            .unwrap_or_default();
        Value::Struct(Struct::from_iter([
            ("title".to_owned(), string(title.unwrap_or_default())),
            (
                "artist".to_owned(),
                string(self.shown_artist(artist.as_deref())),
            ),
            ("folder".to_owned(), string(folder)),
            (
                "release-group".to_owned(),
                string(matched.and_then(|m| m.release_group).unwrap_or_default()),
            ),
        ]))
    }

    /// Gives the skin the albums MusicBrainz found.
    pub(super) fn show_album_matches(&mut self) {
        let rows = self
            .album_candidates
            .iter()
            .map(|album| {
                let year = album.date.get(..4).unwrap_or_default();
                let facts = [album.kind.as_str(), year]
                    .into_iter()
                    .filter(|f| !f.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ");
                Value::Struct(Struct::from_iter([
                    ("release-group".to_owned(), string(&album.release_group)),
                    ("title".to_owned(), string(&album.title)),
                    ("facts".to_owned(), string(facts)),
                    ("artist".to_owned(), string(&album.artist.name)),
                    ("detail".to_owned(), string(&album.artist.disambiguation)),
                    (
                        "picture".to_owned(),
                        Value::Image(
                            self.album_covers
                                .get(&album.release_group)
                                .cloned()
                                .unwrap_or_default(),
                        ),
                    ),
                ]))
            })
            .collect();
        self.album_rows.sync(rows);
    }

    /// Starts matching the artist whose page is shown, searching MusicBrainz by their name.
    pub fn start_artist_match(&mut self) {
        self.forget_matches();
        self.artist_match = self.artist_page.as_ref().map(|page| page.name.clone());
        if let Some(Some(name)) = &self.artist_match {
            let name = plain_name(name).to_owned();
            self.match_finder
                .find(name, self.settings.fanart_key.clone());
        }
    }

    /// Matches the artist being matched to the one with the MusicBrainz id `mbid`; empty, or
    /// Freya's own match, lets Freya match them again.
    pub fn save_artist_match(&mut self, mbid: &str) {
        let Some(Some(artist)) = self.artist_match.clone() else {
            return;
        };
        let found = self.found_mbid(Some(&artist));
        let mbid = Some(mbid.trim())
            .filter(|mbid| !mbid.is_empty() && found.as_deref() != Some(*mbid))
            .map(str::to_owned);
        let before = self.edits.get(&artist).cloned().unwrap_or_default();
        // MusicBrainz's words for them: from the search, else as kept.
        let note = mbid.as_ref().and_then(|mbid| {
            let searched = self.matches.iter().find(|m| m.mbid == *mbid);
            match searched {
                Some(found) => Some(found.disambiguation.clone()),
                None => before
                    .note
                    .clone()
                    .filter(|_| before.mbid.as_ref() == Some(mbid)),
            }
        });
        self.set_artist_mbid(&artist, mbid, note);
    }

    pub fn cancel_artist_match(&mut self) {
        self.artist_match = None;
        self.forget_matches();
    }

    /// Keeps the user's match of `artist`, ending the matching, and shows what comes of it.
    pub(super) fn set_artist_mbid(
        &mut self,
        artist: &str,
        mbid: Option<String>,
        note: Option<String>,
    ) {
        self.cancel_artist_match();
        let Some(db) = &self.db else {
            return;
        };
        let before = self.edits.get(artist).cloned().unwrap_or_default();
        let edit = ArtistEdit {
            mbid,
            note,
            ..before.clone()
        };
        if let Err(e) = db.set_artist_edit(artist, &edit) {
            log::warn!("could not keep the match of {artist}: {e}");
            return;
        }
        if edit.mbid != before.mbid {
            self.forget_found(artist);
        }
        self.reload();
    }

    /// The artist being matched, as `artist-match` gives it.
    pub(super) fn artist_match_value(&self) -> Value {
        let artist = self.artist_match.clone().flatten();
        let mbid = artist
            .as_deref()
            .and_then(|a| self.edits.get(a)?.mbid.clone());
        // With no artist being matched, nothing to ask.
        let found = artist
            .as_deref()
            .and_then(|artist| self.found_mbid(Some(artist)));
        Value::Struct(Struct::from_iter([
            (
                "artist".to_owned(),
                string(artist.as_deref().map(plain_name).unwrap_or_default()),
            ),
            ("mbid".to_owned(), string(mbid.unwrap_or_default())),
            ("found-mbid".to_owned(), string(found.unwrap_or_default())),
        ]))
    }

    /// Searches MusicBrainz for the artists named `query`, to say which is the one matched.
    pub fn find_matches(&mut self, query: &str) {
        let query = query.trim();
        if self.artist_match.is_none() || query.is_empty() {
            return;
        }
        self.match_error.clear();
        self.matches.clear();
        self.show_matches();
        self.match_finder
            .find(query.to_owned(), self.settings.fanart_key.clone());
    }

    pub(super) fn forget_matches(&mut self) {
        self.match_finder.stop();
        self.matches.clear();
        self.match_pictures.clear();
        self.match_error.clear();
        self.show_matches();
    }

    /// Gives the skin the artists MusicBrainz found.
    pub(super) fn show_matches(&mut self) {
        let rows = self
            .matches
            .iter()
            .map(|m| {
                Value::Struct(Struct::from_iter([
                    ("mbid".to_owned(), string(&m.mbid)),
                    ("name".to_owned(), string(&m.name)),
                    ("detail".to_owned(), string(&m.disambiguation)),
                    ("facts".to_owned(), string(match_facts(m))),
                    (
                        "picture".to_owned(),
                        Value::Image(
                            self.match_pictures
                                .get(&m.mbid)
                                .cloned()
                                .unwrap_or_default(),
                        ),
                    ),
                ]))
            })
            .collect();
        self.match_rows.sync(rows);
    }

    /// Chooses the `index`th picture of `kind` ("portrait" or "backdrop"); the first is none.
    pub fn choose_picture(&mut self, kind: &str, index: usize) {
        let Some(kind) = picture_kind(kind) else {
            return;
        };
        let pick = match index {
            0 => Some(Pick::Nothing),
            i => self
                .choices
                .iter()
                .filter(|c| c.kind == kind)
                .nth(i - 1)
                .map(|c| c.pick.clone()),
        };
        if let Some(pick) = pick {
            self.picked.insert(kind, pick);
            self.show_choices();
        }
    }

    /// Adds the picture in `file` as one of `kind` to choose, and chooses it.
    pub fn add_picture_file(&mut self, kind: &str, file: &Path) {
        let Some(kind) = picture_kind(kind) else {
            return;
        };
        let added = std::fs::read(file)
            .map_err(|e| e.to_string())
            .and_then(|bytes| self.finder.add(kind, &bytes));
        match added {
            Ok(found) => {
                self.picture_error.clear();
                self.add_choice(found);
            }
            Err(e) => self.picture_error = format!("Could not use that file: {e}"),
        }
        self.show_choices();
    }

    /// Downloads the picture at `url` as one of `kind` to choose; it is chosen when it comes.
    pub fn picture_from_url(&mut self, kind: &str, url: &str) {
        let (Some(kind), url) = (picture_kind(kind), url.trim()) else {
            return;
        };
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            self.picture_error = "An address starts with https://".to_owned();
            return;
        }
        self.picture_error.clear();
        self.finder.download(kind, url.to_owned());
    }

    pub(super) fn add_choice(&mut self, found: Found) {
        if found.choose {
            self.picked.insert(found.kind, found.pick.clone());
        }
        if found.pick == Pick::Nothing {
            self.none_shown
                .insert(found.kind, Image::from_rgba8(found.preview));
            return;
        }
        // The same picture once, as it was found first.
        if self
            .choices
            .iter()
            .any(|c| c.kind == found.kind && c.pick == found.pick)
        {
            return;
        }
        let at = self.choices.partition_point(|c| c.order <= found.order);
        self.choices.insert(
            at,
            Choice {
                kind: found.kind,
                pick: found.pick,
                source: found.source,
                size: format!("{} × {}", found.width, found.height),
                image: Image::from_rgba8(found.preview),
                order: found.order,
            },
        );
    }

    /// Gives the skin the pictures to choose from.
    pub(super) fn show_choices(&mut self) {
        let rows = |kind: PictureKind| -> Vec<Value> {
            let picked = self.picked.get(&kind);
            let row = |image: Image, source: &str, size: &str, chosen: bool| {
                Value::Struct(Struct::from_iter([
                    ("picture".to_owned(), Value::Image(image)),
                    ("source".to_owned(), string(source)),
                    ("size".to_owned(), string(size)),
                    ("chosen".to_owned(), Value::Bool(chosen)),
                ]))
            };
            if self.draft.is_none() {
                return Vec::new();
            }
            // None, shown as what it means: their album covers, when there are any.
            let none = self.none_shown.get(&kind).cloned();
            let label = if none.is_some() {
                "Album covers"
            } else {
                "None"
            };
            std::iter::once(row(
                none.unwrap_or_default(),
                label,
                "",
                picked == Some(&Pick::Nothing),
            ))
            .chain(
                self.choices
                    .iter()
                    .filter(|c| c.kind == kind)
                    .map(|c| row(c.image.clone(), &c.source, &c.size, picked == Some(&c.pick))),
            )
            .collect()
        };
        let portraits = rows(PictureKind::Portrait);
        let backdrops = rows(PictureKind::Backdrop);
        self.portrait_rows.sync(portraits);
        self.backdrop_rows.sync(backdrops);
    }

    /// Keeps the pictures the user chose for `artist`. Returns whether any changed.
    pub(super) fn keep_picked(&mut self, artist: &str) -> bool {
        let Some(db) = &self.db else {
            return false;
        };
        let mut changed = false;
        for kind in [PictureKind::Portrait, PictureKind::Backdrop] {
            let (Some(picked), Some(before)) =
                (self.picked.get(&kind), self.picked_before.get(&kind))
            else {
                continue;
            };
            if picked == before {
                continue;
            }
            let kept = match picked {
                Pick::Nothing => {
                    db.set_artist_picture(artist, kind, CHOSEN, None, SystemTime::now())
                }
                Pick::Kept(checksum) => {
                    db.set_artist_picture(artist, kind, CHOSEN, Some(checksum), SystemTime::now())
                }
            };
            match kept {
                Ok(()) => changed = true,
                Err(e) => log::warn!("could not keep the picture chosen for {artist}: {e}"),
            }
        }
        changed
    }

    /// The form for changing an artist, as `artist-draft` gives it.
    pub(super) fn draft_value(&self) -> Value {
        let artist = self
            .artist_page
            .as_ref()
            .filter(|_| self.draft.is_some())
            .and_then(|p| p.name.clone());
        let edit = artist
            .as_deref()
            .and_then(|a| self.edits.get(a).cloned())
            .unwrap_or_default();
        // Without the user's: the tags', else what MusicBrainz's search found; with no artist
        // being changed, nothing to ask.
        let found_mbid = artist
            .as_deref()
            .and_then(|artist| self.found_mbid(Some(artist)))
            .unwrap_or_default();
        let text = |value: Option<String>| string(value.unwrap_or_default());
        // Theirs, else Last.fm's to start from.
        let found = edit.biography.is_none() && !self.found_biography.is_empty();
        let biography = edit
            .biography
            .clone()
            .unwrap_or_else(|| self.found_biography.clone());
        Value::Struct(Struct::from_iter([
            (
                "artist".to_owned(),
                text(artist.map(|a| freya_protocol::plain_name(&a).to_owned())),
            ),
            ("name".to_owned(), text(edit.name)),
            ("sort-name".to_owned(), text(edit.sort_name)),
            ("mbid".to_owned(), text(edit.mbid)),
            ("found-mbid".to_owned(), string(found_mbid)),
            ("biography".to_owned(), string(biography)),
            (
                "header".to_owned(),
                string(edit.header.map_or("auto", Header::name)),
            ),
            (
                "biography-from".to_owned(),
                string(if found {
                    self.found_biography_from.as_str()
                } else {
                    ""
                }),
            ),
        ]))
    }

    /// Keeps what the user changed about the artist being changed, and shows it. A new
    /// MusicBrainz id looks again for their wide picture.
    pub fn save_artist(&mut self, edit: &ArtistEdit) {
        let (Some(Some(artist)), Some(db)) = (self.draft.take(), &self.db) else {
            return;
        };
        let before = self.edits.get(&artist).cloned().unwrap_or_default();
        // Last.fm's, saved unchanged, stays Last.fm's: it is not the user's own.
        let mut edit = edit.clone();
        // MusicBrainz's words for the artist matched, kept with the id.
        let mbid = edit.mbid.as_deref().map(str::trim);
        edit.note = if mbid == before.mbid.as_deref() {
            before.note.clone()
        } else {
            self.matches
                .iter()
                .find(|m| Some(m.mbid.as_str()) == mbid)
                .map(|m| m.disambiguation.clone())
        };
        if edit.biography.as_deref().map(str::trim) == Some(self.found_biography.trim()) {
            edit.biography = None;
        }
        if let Err(e) = db.set_artist_edit(&artist, &edit) {
            log::warn!("could not keep what was changed about {artist}: {e}");
            return;
        }
        let kept = db.artist_edit(&artist).unwrap_or_default();
        self.finder.stop();
        self.forget_matches();
        // Shown by `reload`, which reads the pictures the artist has now.
        if self.keep_picked(&artist) {
            self.pictures_stale = true;
        }
        self.choices.clear();
        self.show_choices();
        if kept.mbid != before.mbid {
            self.forget_found(&artist);
        }
        // The top of their page shows the other picture: made again.
        if kept.header != before.header
            && let Some(page) = &mut self.artist_page
        {
            page.banner = None;
            page.portrait_large = false;
            page.color = None;
            page.asked = None;
        }
        self.reload();
    }

    /// Forgets what the internet gave for `artist`, and asks it again: after they were
    /// matched to another artist of the name. What the user chose, and the pictures in their
    /// folder, stay.
    pub(super) fn forget_found(&mut self, artist: &str) {
        if let Some(db) = &self.db {
            for kind in [PictureKind::Portrait, PictureKind::Backdrop] {
                let found = db
                    .artist_picture(artist, kind)
                    .ok()
                    .flatten()
                    .is_some_and(|row| row.from_internet());
                if found && let Err(e) = db.forget_artist_picture(artist, kind) {
                    log::warn!("could not forget the pictures of {artist}: {e}");
                }
            }
            if let Err(e) = db.forget_artist_text(artist) {
                log::warn!("could not forget what Last.fm said of {artist}: {e}");
            }
        }
        self.pictures_stale = true;
        self.fetch(Some(vec![artist.to_owned()]));
    }
}
