//! Covers and pictures of artists: kept, decoded, and made ready ahead of being shown.

use super::*;

/// The covers shown last, so scrolling back does not decode them again.
#[derive(Default)]
pub(super) struct RecentCovers {
    /// Least recently shown first.
    pub(super) order: VecDeque<String>,
    pub(super) images: HashMap<String, Image>,
}

impl RecentCovers {
    pub(super) fn get(&mut self, key: &str) -> Option<Image> {
        let image = self.images.get(key)?.clone();
        self.order.retain(|k| k != key);
        self.order.push_back(key.to_owned());
        Some(image)
    }

    pub(super) fn remove(&mut self, key: &str) {
        self.order.retain(|k| k != key);
        self.images.remove(key);
    }

    pub(super) fn put(&mut self, key: &str, image: Image) {
        if self.order.len() >= RECENT_COVERS
            && let Some(oldest) = self.order.pop_front()
        {
            self.images.remove(&oldest);
        }
        self.order.push_back(key.to_owned());
        self.images.insert(key.to_owned(), image);
    }
}

impl LibraryPanel {
    /// Takes away what an older Freya kept in the cache and keeps elsewhere now: pictures it
    /// downloaded, mixed with the thumbnails, and what Last.fm said, as files beside them.
    pub(super) fn clear_older_cache(&self) {
        let Some(thumbnails) = &self.thumbnails else {
            return;
        };
        match thumbnails.clear_older_layout() {
            Ok(0) => {}
            Ok(n) => log::info!("cleared {n} files of an older layout from the cache"),
            Err(e) => log::warn!("could not clear the older cache: {e}"),
        }
        let Some(artists) = self
            .covers_dir()
            .and_then(Path::parent)
            .map(|c| c.join("artists"))
        else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&artists) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|e| e == "json" || e == "partial")
            {
                let _ = std::fs::remove_file(path);
            }
        }
        // Gone when empty.
        let _ = std::fs::remove_dir(&artists);
    }

    pub(super) fn covers_dir(&self) -> Option<&Path> {
        self.thumbnails.as_ref().map(Thumbnails::dir)
    }

    /// The cover for a row's cover key, or an empty image while it is made or when the album
    /// has none.
    pub fn cover(&mut self, key: &str) -> Image {
        if let Some(&row) = self.cover_rows.get(key) {
            self.covers_asked = Some(match self.covers_asked {
                Some((first, last)) => (first.min(row), last.max(row)),
                None => (row, row),
            });
        }
        if let Some(image) = self.recent_covers.get(key) {
            return image;
        }
        // Shown once decoded, soon.
        if self.decoder.is_decoding(key) {
            self.covers_missed = true;
            return Image::default();
        }
        let Thumbnail::Ready(file) = self.cover_state(key) else {
            return Image::default();
        };
        // Not decoded ahead: scrolled to faster than the threads went, or the first screen.
        self.decoder.decode(key, file);
        self.covers_missed = true;
        Image::default()
    }

    /// Whether the cover for `key` is made, or being made, or none: made, or asked to be
    /// made, as soon as it is asked for.
    pub(super) fn cover_state(&mut self, key: &str) -> Thumbnail {
        // Known already, made or not: asked for again on every scroll and every new picture,
        // without the library being read for it.
        if let Some(known) = self.thumbnails.as_ref().and_then(|t| t.known(key)) {
            return known;
        }
        // A genre's picture, or its albums' covers.
        if let Some(name) = self.genre_keys.get(key).cloned() {
            let source = self.genre_source(name.as_deref());
            return match &mut self.thumbnails {
                Some(thumbnails) => thumbnails.get_artist(key, &source),
                None => Thumbnail::None,
            };
        }
        // A playlist's picture, or its covers.
        if let Some(source) = self.playlist_sources.get(key) {
            return match &mut self.thumbnails {
                Some(thumbnails) => thumbnails.get_artist(key, source),
                None => Thumbnail::None,
            };
        }
        match (&self.db, self.artist_keys.get(key)) {
            // An artist's picture.
            (Some(_), Some(name)) => {
                let source = self.artist_source(name.as_deref());
                match &mut self.thumbnails {
                    Some(thumbnails) => thumbnails.get_artist(key, &source),
                    None => Thumbnail::None,
                }
            }
            // An album's cover, as the scan made it; none made yet shows none.
            _ => {
                let (Some(thumbnails), Some(checksum)) = (
                    &mut self.thumbnails,
                    self.made_covers
                        .get(key)
                        .and_then(|made| made.cover.as_deref()),
                ) else {
                    return Thumbnail::None;
                };
                thumbnails.get_picture(checksum)
            }
        }
    }

    /// Takes in the covers decoded, and decodes those around the ones on screen ahead of
    /// their being scrolled to. Returns whether one the skin asked for came.
    pub(super) fn decode_covers(&mut self) -> bool {
        let mut came = false;
        for (key, decoded) in self.decoder.take() {
            let image = decoded.map(Image::from_rgb8).unwrap_or_default();
            self.recent_covers.put(&key, image);
            came = true;
        }
        if let Some(asked) = self.covers_asked.take() {
            self.covers_shown = asked;
        }
        let (first, last) = self.covers_shown;
        // Those on screen first, then outwards both ways.
        let around = (1..=DECODE_AHEAD)
            .flat_map(|d| [last.checked_add(d), first.checked_sub(d)])
            .flatten();
        let mut handed = 0;
        for row in (first..=last).chain(around) {
            if handed == DECODE_AT_ONCE {
                break;
            }
            let Some(key) = self.cover_order.get(row).cloned() else {
                continue;
            };
            if self.recent_covers.images.contains_key(&key) || self.decoder.is_decoding(&key) {
                continue;
            }
            if let Thumbnail::Ready(file) = self.cover_state(&key) {
                self.decoder.decode(&key, file);
                handed += 1;
            }
        }
        came && std::mem::take(&mut self.covers_missed)
    }

    /// Reads the albums' covers and the artists' pictures from the library, when a scan or the
    /// user changed them; what was shown of those that changed is made and read anew.
    ///
    /// A server is asked for them on a thread of its own, the window not waiting for it, and
    /// they are read once its answers come.
    pub(super) fn load_pictures(&mut self) {
        if !self.pictures_stale || self.asking_pictures.is_some() {
            return;
        }
        self.asking_pictures = self.ask_on_thread(|api| api.prefetch(&picture_calls()));
        if self.asking_pictures.is_none() {
            self.load_pictures_now();
        }
    }

    /// Reads the albums' covers and the artists' pictures now, from what was asked ahead.
    pub(super) fn load_pictures_now(&mut self) {
        self.asking_pictures = None;
        if !std::mem::take(&mut self.pictures_stale) {
            return;
        }
        let Some(db) = &self.db else {
            return;
        };
        db.prefetch(&picture_calls());
        let covers = db.album_covers().unwrap_or_else(|e| {
            log::warn!("could not read the albums' covers: {e}");
            HashMap::new()
        });
        self.load_genre_infos();
        let Some(db) = &self.db else {
            return;
        };
        for (key, made) in &covers {
            if self.made_covers.get(key).map(|old| &old.cover) != Some(&made.cover) {
                self.recent_covers.remove(key);
            }
        }
        self.made_covers = covers;
        self.artist_albums = artist_albums(db.all_albums().unwrap_or_default());
        let read = |kind| {
            db.artist_pictures(kind).unwrap_or_else(|e| {
                log::warn!("could not read the artists' pictures: {e}");
                HashMap::new()
            })
        };
        let (portraits, backdrops) = (read(PictureKind::Portrait), read(PictureKind::Backdrop));
        let changed: Vec<String> = portraits
            .iter()
            .filter(|(artist, picture)| self.portraits.get(*artist) != Some(*picture))
            .chain(
                backdrops
                    .iter()
                    .filter(|(artist, picture)| self.backdrops.get(*artist) != Some(*picture)),
            )
            .map(|(artist, _)| artist.clone())
            .chain(
                self.portraits
                    .keys()
                    .filter(|a| !portraits.contains_key(*a))
                    .cloned(),
            )
            .chain(
                self.backdrops
                    .keys()
                    .filter(|a| !backdrops.contains_key(*a))
                    .cloned(),
            )
            .collect();
        self.portraits = portraits;
        self.backdrops = backdrops;
        let shown = self.artist_page.as_ref().and_then(|p| p.name.clone());
        for artist in changed {
            let key = Thumbnails::artist_key(Some(&artist));
            if let Some(thumbnails) = &mut self.thumbnails {
                thumbnails.forget_artist(&key);
            }
            self.recent_covers.remove(&key);
            if shown.as_ref() == Some(&artist)
                && let Some(page) = &mut self.artist_page
            {
                page.banner = None;
                page.portrait_large = false;
                page.color = None;
                page.asked = None;
            }
        }
        self.covers_ready += 1;
        self.color_album_page();
        self.show_artist_picture();
        self.show_playlist_picture();
    }

    /// What is kept of the artist `name`'s picture: theirs, and their albums' covers.
    pub(super) fn artist_source(&self, name: Option<&str>) -> ArtistSource {
        let read;
        let albums = match self.artist_albums.get(&name.map(str::to_owned)) {
            Some(albums) => albums,
            None => {
                read = self
                    .db
                    .as_ref()
                    .and_then(|db| db.albums(name).ok())
                    .unwrap_or_default();
                &read
            }
        };
        let covers = albums
            .iter()
            .filter_map(|album| self.made_covers.get(&cover_key(album))?.cover.clone())
            .collect();
        ArtistSource {
            picture: name.and_then(|name| self.portraits.get(name)).cloned(),
            covers,
        }
    }

    /// Lines up every artist's and album's picture to be made ahead of its being shown: once
    /// they are read, when a server is asked for them.
    pub(super) fn prepare_all(&mut self) {
        self.pictures_stale = true;
        if self.remote.is_some() {
            self.prepare_after = true;
            self.load_pictures();
            return;
        }
        self.load_pictures_now();
        self.prepare_pictures();
    }

    /// Lines up the pictures read to be made.
    pub(super) fn prepare_pictures(&mut self) {
        let Some(db) = &self.db else {
            return;
        };
        self.prepare.clear();
        if let Ok(artists) = db.artists() {
            self.prepare
                .extend(artists.into_iter().map(|a| Prepare::Artist(a.name)));
        }
        let covers = self
            .made_covers
            .values()
            .filter_map(|made| made.cover.clone())
            .map(Prepare::Cover);
        self.prepare.extend(covers);
    }

    /// Hands a few of the pictures lined up to the threads: the window is not held up.
    pub(super) fn prepare_some(&mut self) {
        for _ in 0..PREPARE_AT_ONCE {
            let Some(next) = self.prepare.pop_front() else {
                return;
            };
            match next {
                Prepare::Artist(name) => {
                    let key = Thumbnails::artist_key(name.as_deref());
                    let source = self.artist_source(name.as_deref());
                    if let Some(thumbnails) = &mut self.thumbnails {
                        thumbnails.prepare_artist(&key, &source);
                    }
                }
                Prepare::Cover(checksum) => {
                    if let Some(thumbnails) = &mut self.thumbnails {
                        thumbnails.prepare_picture(&checksum);
                    }
                }
            }
        }
    }
}
