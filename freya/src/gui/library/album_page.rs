//! An album's page: its tracks, its cover in full, and the colour taken from it.

use super::*;

/// What an album's own page shows over its tracks.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct AlbumPage {
    pub(super) title: String,
    pub(super) artist: String,
    pub(super) detail: String,
    pub(super) cover: String,
    /// From the cover, once its thumbnail is made.
    pub(super) color: Option<[u8; 3]>,
    /// The cover whole and large, once read after the user asked to see it.
    pub(super) cover_full: Option<Image>,
    /// A track of the album, which its cover is read from.
    pub(super) track: PathBuf,
}

/// An album's cover whole, read on a thread: the album's cover key, and the picture.
pub(super) type FullCover = (String, slint::SharedPixelBuffer<slint::Rgb8Pixel>);

/// The longest side of an album's cover shown whole.
pub(super) const COVER_FULL: u32 = 1400;

impl LibraryPanel {
    /// The page of the album shown, and how often its tracks were played; for anything else,
    /// none.
    pub(super) fn load_album_page(&mut self) {
        self.play_counts.clear();
        let before = self.album_page.take();
        let (Some(db), Level::AlbumTracks { artist, album }) = (&self.db, self.level().clone())
        else {
            return;
        };
        if self.searching() {
            return;
        }
        self.play_counts = db
            .album_plays(artist.as_deref(), album.as_deref())
            .map(|plays| plays.into_iter().collect())
            .unwrap_or_else(|e| {
                log::warn!("library: {e}");
                HashMap::new()
            });
        let tracks: Vec<&Track> = self.items.iter().filter_map(Item::track).collect();
        let Some(first) = tracks.first() else {
            return;
        };
        let cover_album = Album {
            artist: artist.clone(),
            name: album.clone(),
            year: None,
            tracks: 0,
            track: first.path.clone(),
        };
        let cover = cover_key(&cover_album);
        let mut detail = Vec::new();
        if let Some(year) = tracks.iter().filter_map(|t| t.year).min() {
            detail.push(year.to_string());
        }
        let total: Duration = tracks.iter().filter_map(|t| t.duration).sum();
        let mut size = count(tracks.len(), "track");
        if !total.is_zero() {
            size = format!("{size}, {}", long_time(total));
        }
        detail.push(size);
        // Read already, while the same album shows.
        let cover_full = before
            .filter(|page| page.cover == cover)
            .and_then(|page| page.cover_full);
        self.album_page = Some(AlbumPage {
            title: album_name(album.as_deref()),
            artist: self.shown_artist(artist.as_deref()),
            detail: detail.join(" · "),
            track: first.path.clone(),
            cover,
            color: None,
            cover_full,
        });
        self.color_album_page();
    }

    /// Takes the album page's color from its cover, once the cover's thumbnail is made.
    pub(super) fn color_album_page(&mut self) {
        let Some(page) = &self.album_page else {
            return;
        };
        if page.color.is_some() {
            return;
        }
        // As the scan made it; no cover, or none made yet: grey, as for a cover of grey.
        let color = self
            .made_covers
            .get(&page.cover)
            .and_then(|made| made.color)
            .unwrap_or_else(|| freya_media::cover::page_color(&[]));
        if let Some(page) = &mut self.album_page {
            page.color = Some(color);
        }
    }

    pub(super) fn album_page_value(&self) -> Value {
        let page = self.album_page.clone().filter(|_| !self.searching());
        let [r, g, b] = page
            .as_ref()
            .and_then(|p| p.color)
            .unwrap_or(freya_media::cover::page_color(&[]));
        let page = page.unwrap_or(AlbumPage {
            title: String::new(),
            artist: String::new(),
            detail: String::new(),
            cover: String::new(),
            color: None,
            cover_full: None,
            track: PathBuf::new(),
        });
        Value::Struct(Struct::from_iter([
            ("shown".to_owned(), Value::Bool(!page.title.is_empty())),
            ("title".to_owned(), string(page.title)),
            ("artist".to_owned(), string(page.artist)),
            ("detail".to_owned(), string(page.detail)),
            ("cover".to_owned(), string(page.cover)),
            (
                "color".to_owned(),
                Value::Brush(slint::Brush::SolidColor(slint::Color::from_rgb_u8(r, g, b))),
            ),
            ("favorite".to_owned(), Value::Bool(self.page_favorite())),
            (
                "picture-full".to_owned(),
                Value::Image(page.cover_full.unwrap_or_default()),
            ),
        ]))
    }

    /// Reads the cover of the album whose page is shown whole, from its music, on a thread:
    /// the user asked to see it.
    pub fn show_album_cover(&mut self) {
        let Some(page) = self.album_page.as_ref().filter(|p| p.cover_full.is_none()) else {
            return;
        };
        let Some(api) = self.db.clone() else {
            return;
        };
        let (key, track) = (page.cover.clone(), page.track.clone());
        let read = self.full_covers.0.clone();
        let started = std::thread::Builder::new()
            .name("freya-album-cover".into())
            .spawn(move || {
                let picture = api
                    .album_art(&track, COVER_FULL)
                    .ok()
                    .flatten()
                    .and_then(|bytes| crate::gui::decode_picture(&bytes).ok());
                if let Some(picture) = picture {
                    let rgb = picture.to_rgb8();
                    let buffer = slint::SharedPixelBuffer::clone_from_slice(
                        rgb.as_raw(),
                        rgb.width(),
                        rgb.height(),
                    );
                    let _ = read.send((key, buffer));
                }
            });
        if let Err(e) = started {
            log::warn!("could not read the album's cover: {e}");
        }
    }

    /// Shows an album's cover whole, once read, if its page shows still.
    pub(super) fn take_full_covers(&mut self) {
        while let Ok((key, buffer)) = self.full_covers.1.try_recv() {
            if let Some(page) = self.album_page.as_mut().filter(|p| p.cover == key) {
                page.cover_full = Some(Image::from_rgb8(buffer));
            }
        }
    }
}
