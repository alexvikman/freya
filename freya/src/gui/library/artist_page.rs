//! An artist's page: their picture, what is told of them, and their popular tracks.

use super::*;

/// What an artist's own page shows over their albums; or a genre's, over its.
pub(super) struct ArtistPage {
    /// A genre's page rather than an artist's.
    pub(super) genre: bool,
    pub(super) name: Option<String>,
    /// The name shown: the one the user gave them, else the tags'.
    pub(super) shown_name: String,
    /// Whether the biography is the user's own, rather than Last.fm's; and where it is from
    /// when not: "Wikipedia" or "Last.fm".
    pub(super) own_biography: bool,
    pub(super) biography_from: String,
    pub(super) detail: String,
    pub(super) genres: String,
    /// Their picture's key; a wide picture of them and their picture large, once made; and a
    /// color from the one shown.
    pub(super) picture: String,
    pub(super) banner: Option<Image>,
    pub(super) portrait: Option<Image>,
    /// Whether `portrait` is made from their picture large, rather than the grid's.
    pub(super) portrait_large: bool,
    /// Their picture large, uncut.
    pub(super) picture_full: Option<Image>,
    /// The files of their pictures a thread was last asked to make for the page.
    pub(super) asked: Option<PageFiles>,
    pub(super) color: Option<[u8; 3]>,
    pub(super) summary: String,
    pub(super) biography: String,
    pub(super) tags: String,
    /// Their most played tracks, and whether all ten show rather than five.
    pub(super) popular: Vec<Played>,
    pub(super) more: bool,
}

/// The size of an artist's round picture: sharp at 232 points on a Retina screen.
pub(super) const ROUND_PICTURE: u32 = 480;

/// The picture `picture`, square and cut round with a soft edge, premultiplied, for a skin to
/// show as is: Slint's software renderer clips to rectangles only.
pub(super) fn round_picture(
    picture: &image::DynamicImage,
) -> slint::SharedPixelBuffer<slint::Rgba8Pixel> {
    let square = picture
        .resize_to_fill(
            ROUND_PICTURE,
            ROUND_PICTURE,
            image::imageops::FilterType::Lanczos3,
        )
        .to_rgba8();
    let mut pixels =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(ROUND_PICTURE, ROUND_PICTURE);
    let radius = ROUND_PICTURE as f32 / 2.0;
    for ((x, y, from), to) in square.enumerate_pixels().zip(pixels.make_mut_slice()) {
        let (dx, dy) = (x as f32 + 0.5 - radius, y as f32 + 0.5 - radius);
        // Partly covered at the edge, by how far the circle reaches into the pixel.
        let cover = (radius - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
        // Premultiplied, as Slint draws it.
        let [r, g, b, a] = from.0;
        let alpha = f32::from(a) / 255.0 * cover;
        let channel = |c: u8| (f32::from(c) * alpha).round() as u8;
        *to = slint::Rgba8Pixel::new(
            channel(r),
            channel(g),
            channel(b),
            (alpha * 255.0).round() as u8,
        );
    }
    pixels
}

/// The files an artist's page shows: their wide picture, and their picture with whether it
/// is the large one.
pub(super) type PageFiles = (Option<PathBuf>, Option<(PathBuf, bool)>);

/// An artist's pictures as their page shows them, made on a thread: decoding them on the
/// window's thread held up the page as it opened.
#[derive(Default)]
pub(super) struct PagePictures {
    /// Their wide picture, and its color.
    pub(super) banner: Option<(slint::SharedPixelBuffer<slint::Rgb8Pixel>, Option<[u8; 3]>)>,
    pub(super) portrait: Option<PagePortrait>,
}

/// An artist's picture as their page shows it.
pub(super) struct PagePortrait {
    /// Cut round.
    pub(super) round: slint::SharedPixelBuffer<slint::Rgba8Pixel>,
    /// Whether it is their picture large, rather than the grid's; and then uncut.
    pub(super) large: bool,
    pub(super) full: Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>,
    pub(super) color: [u8; 3],
}

/// An artist page's pictures made, by the page's picture's key.
pub(super) type MadePage = (String, PagePictures);

impl PagePictures {
    pub(super) fn make((banner, portrait): &PageFiles) -> Self {
        let color = |picture: &image::DynamicImage| {
            freya_media::cover::page_color(picture.thumbnail(64, 64).to_rgb8().as_raw())
        };
        let rgb = |picture: &image::DynamicImage| {
            let rgb = picture.to_rgb8();
            slint::SharedPixelBuffer::clone_from_slice(rgb.as_raw(), rgb.width(), rgb.height())
        };
        let open = |file: &Path| {
            image::open(file)
                .map_err(|e| log::debug!("could not read {}: {e}", file.display()))
                .ok()
        };
        Self {
            banner: banner
                .as_deref()
                .and_then(open)
                .map(|picture| (rgb(&picture), Some(color(&picture)))),
            portrait: portrait.as_ref().and_then(|(file, large)| {
                let picture = open(file)?;
                Some(PagePortrait {
                    round: round_picture(&picture),
                    large: *large,
                    full: large.then(|| rgb(&picture)),
                    color: color(&picture),
                })
            }),
        }
    }
}

impl LibraryPanel {
    /// The page of the artist whose albums are shown; for anything else, none.
    pub(super) fn load_artist_page(&mut self) {
        let more = self.artist_page.as_ref().is_some_and(|p| p.more);
        self.artist_page = None;
        if self.searching() {
            self.popular_rows.sync(Vec::new());
            return;
        }
        if let Level::GenreAlbums { genre: Some(genre) } = self.level().clone() {
            self.load_genre_page(genre, more);
            self.show_artist_picture();
            self.show_artist_info();
            self.show_popular();
            return;
        }
        let (Some(db), Level::ArtistAlbums { artist }) = (&self.db, self.level().clone()) else {
            self.popular_rows.sync(Vec::new());
            return;
        };
        let summary = db.artist_summary(artist.as_deref(), 3).ok();
        let popular = db
            .artist_popular(artist.as_deref(), POPULAR_MORE)
            .unwrap_or_default();
        let mut detail = Vec::new();
        if let Some(s) = &summary {
            detail.push(count(s.albums as usize, "album"));
            detail.push(count(s.tracks as usize, "track"));
            if !s.duration.is_zero() {
                detail.push(long_time(s.duration));
            }
        }
        let picture = Thumbnails::artist_key(artist.as_deref());
        self.artist_keys.insert(picture.clone(), artist.clone());
        self.artist_page = Some(ArtistPage {
            genre: false,
            shown_name: self.shown_artist(artist.as_deref()),
            own_biography: false,
            biography_from: String::new(),
            name: artist,
            detail: detail.join(" · "),
            genres: summary.map(|s| s.genres.join(", ")).unwrap_or_default(),
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
        self.show_artist_picture();
        self.show_artist_info();
        self.show_popular();
    }

    /// The artist page's wide picture, or else their picture large, and a color from the one
    /// shown, once made; asking for them to be made the first time.
    pub(super) fn show_artist_picture(&mut self) {
        let Some(page) = &self.artist_page else {
            return;
        };
        let key = page.picture.clone();
        let needs_banner = page.banner.is_none();
        let backdrop =
            page_backdrop(page, &self.backdrops, &self.edits, &self.genre_infos).cloned();
        // Their picture shows only without a wide one.
        let needs_portrait = !page.portrait_large && backdrop.is_none();
        // Made, or asked to be made, as for the grid.
        self.cover(&key);
        let Some(thumbnails) = &mut self.thumbnails else {
            return;
        };
        let banner = match needs_banner.then(|| thumbnails.get_backdrop(&key, backdrop.as_deref()))
        {
            Some(Thumbnail::Ready(file)) => Some(file),
            _ => None,
        };
        // Their picture large, else the grid's while it is made, or of a collage.
        let portrait = match thumbnails.artist_portrait(&key) {
            _ if !needs_portrait => None,
            Some(file) => Some((file, true)),
            None => thumbnails.ready(&key).map(|file| (file.to_owned(), false)),
        };
        let files = (banner, portrait);
        let Some(page) = &mut self.artist_page else {
            return;
        };
        if files == (None, None) || page.asked.as_ref() == Some(&files) {
            return;
        }
        page.asked = Some(files.clone());
        let made = self.page_pictures.0.clone();
        let started = std::thread::Builder::new()
            .name("freya-artist-page".into())
            .spawn(move || {
                let _ = made.send((key, PagePictures::make(&files)));
            });
        if let Err(e) = started {
            log::warn!("could not make the artist page's pictures: {e}");
        }
    }

    /// Shows the page's pictures a thread made, if they are of the artist shown still.
    pub(super) fn take_page_pictures(&mut self) {
        while let Ok((key, made)) = self.page_pictures.1.try_recv() {
            let Some(made) = self.take_playlist_pictures(&key, made) else {
                continue;
            };
            let Some(page) = self.artist_page.as_mut().filter(|page| page.picture == key) else {
                continue;
            };
            if let Some((banner, color)) = made.banner {
                page.banner = Some(Image::from_rgb8(banner));
                page.color = color.or(page.color);
            }
            if let Some(portrait) = made.portrait {
                page.portrait = Some(Image::from_rgba8_premultiplied(portrait.round));
                page.portrait_large = portrait.large;
                page.picture_full = portrait.full.map(Image::from_rgb8);
                // Without a wide picture: the color of their picture.
                let wide =
                    page_backdrop(page, &self.backdrops, &self.edits, &self.genre_infos).is_some();
                if !wide {
                    page.color = Some(portrait.color);
                }
            }
        }
    }

    /// What is said of the artist on the page, as kept.
    pub(super) fn show_artist_info(&mut self) {
        let Some(page) = &self.artist_page else {
            return;
        };
        let Some(name) = page.name.clone() else {
            return;
        };
        let ((summary, biography, tags, from), own) = if page.genre {
            let (summary, text, from, own) = self.genre_text(&name);
            ((summary, text, String::new(), from), own)
        } else {
            let own = self.edits.get(&name).is_some_and(|e| e.biography.is_some());
            (self.biography_of(&name), own)
        };
        if let Some(page) = &mut self.artist_page {
            page.own_biography = own;
            page.summary = summary;
            page.biography = biography;
            page.tags = tags;
            page.biography_from = from;
        }
    }

    /// What is said of the artist `name`: their summary, whole biography, tags, and where
    /// from. The user's own goes before Wikipedia's article, found by their id, which goes
    /// before what Last.fm says; each as a scan kept it.
    pub(super) fn biography_of(&self, name: &str) -> (String, String, String, String) {
        if let Some(own) = self.edits.get(name).and_then(|e| e.biography.clone()) {
            let summary = own.split("\n\n").next().unwrap_or(&own).trim().to_owned();
            return (summary, own, String::new(), String::new());
        }
        let Some(db) = &self.db else {
            return Default::default();
        };
        // Of the artist they are now: found by their id now.
        let mbid = self.known_mbid(Some(name));
        let article = db
            .artist_article(name)
            .ok()
            .flatten()
            .filter(|(id, _)| Some(id) == mbid.as_ref())
            .and_then(|(_, kept)| kept.answer);
        let info = db.artist_text(name).ok().flatten().map(|kept| kept.answer);
        let tags = info
            .as_ref()
            .map(|info| {
                info.tags
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        match (article, info) {
            (Some(article), _) => (article.summary, article.biography, tags, "Wikipedia".into()),
            (None, Some(info)) => (info.summary, info.biography, tags, "Last.fm".into()),
            (None, None) => Default::default(),
        }
    }

    pub(super) fn show_popular(&mut self) {
        let rows = match &self.artist_page {
            Some(page) => {
                let shown = if page.more { POPULAR_MORE } else { POPULAR };
                page.popular
                    .iter()
                    .take(shown)
                    .enumerate()
                    .map(|(i, played)| {
                        let item = Item::Track(played.track.clone());
                        let mut row = row_value(&item, None, |_| String::new(), Some(played.plays));
                        if let Value::Struct(s) = &mut row {
                            s.set_field("number".into(), string((i + 1).to_string()));
                            let album = Album {
                                artist: played.track.album_artist.clone(),
                                name: played.track.album.clone(),
                                year: None,
                                tracks: 0,
                                track: played.track.path.clone(),
                            };
                            let key = cover_key(&album);
                            s.set_field("cover".into(), string(key));
                        }
                        row
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        self.popular_rows.sync(rows);
    }

    /// Plays the artist's popular tracks shown, from the `index`th.
    pub fn popular_tracks(&self) -> Vec<Track> {
        self.artist_page.as_ref().map_or_else(Vec::new, |page| {
            let shown = if page.more { POPULAR_MORE } else { POPULAR };
            page.popular
                .iter()
                .take(shown)
                .map(|p| p.track.clone())
                .collect()
        })
    }

    /// Shows all ten popular tracks, or five again.
    pub fn toggle_more_popular(&mut self) {
        if let Some(page) = &mut self.artist_page {
            page.more = !page.more;
        }
        self.show_popular();
    }

    /// The user's personal fanart.tv key, or none: with a new one, fanart.tv is asked for
    /// the pictures artists lack.
    pub fn set_fanart_key(&mut self, key: Option<String>) {
        if self.settings.fanart_key != key {
            self.settings.fanart_key = key;
            self.tell_settings();
        }
    }

    /// Whether a scan may ask Wikipedia for articles on artists: allowed, it is asked now for
    /// those who lack one.
    pub fn set_online_wikipedia(&mut self, online: bool) {
        self.settings.artist_wikipedia_online = online;
        self.tell_settings();
    }

    /// Whether a scan may ask Last.fm what it says of artists; as for Wikipedia.
    pub fn set_online_bios(&mut self, online: bool) {
        self.settings.artist_bios_online = online;
        self.tell_settings();
    }

    pub(super) fn artist_page_value(&self) -> Value {
        let page = self.artist_page.as_ref().filter(|_| !self.searching());
        let [r, g, b] = page
            .and_then(|p| p.color)
            .unwrap_or(freya_media::cover::page_color(&[]));
        let text = |f: fn(&ArtistPage) -> String| string(page.map(f).unwrap_or_default());
        Value::Struct(Struct::from_iter([
            ("shown".to_owned(), Value::Bool(page.is_some())),
            (
                "kind".to_owned(),
                string(if page.is_some_and(|p| p.genre) {
                    "genre"
                } else {
                    "artist"
                }),
            ),
            ("name".to_owned(), text(|p| p.shown_name.clone())),
            (
                "favorite".to_owned(),
                Value::Bool(page.is_some_and(|p| {
                    !p.genre
                        && p.name
                            .as_ref()
                            .is_some_and(|n| self.artist_favorites.contains_key(n))
                })),
            ),
            (
                "own-biography".to_owned(),
                Value::Bool(page.is_some_and(|p| p.own_biography)),
            ),
            (
                "biography-from".to_owned(),
                text(|p| p.biography_from.clone()),
            ),
            (
                "note".to_owned(),
                string(
                    page.and_then(|p| self.edits.get(p.name.as_deref()?)?.note.clone())
                        .unwrap_or_default(),
                ),
            ),
            ("detail".to_owned(), text(|p| p.detail.clone())),
            ("genres".to_owned(), text(|p| p.genres.clone())),
            ("picture".to_owned(), text(|p| p.picture.clone())),
            (
                "banner".to_owned(),
                Value::Image(page.and_then(|p| p.banner.clone()).unwrap_or_default()),
            ),
            (
                "has-banner".to_owned(),
                Value::Bool(page.is_some_and(|p| {
                    p.banner.is_some()
                        || page_backdrop(p, &self.backdrops, &self.edits, &self.genre_infos)
                            .is_some()
                })),
            ),
            (
                "portrait".to_owned(),
                Value::Image(page.and_then(|p| p.portrait.clone()).unwrap_or_default()),
            ),
            (
                "picture-full".to_owned(),
                Value::Image(
                    page.and_then(|p| p.picture_full.clone())
                        .unwrap_or_default(),
                ),
            ),
            (
                "color".to_owned(),
                Value::Brush(slint::Brush::SolidColor(slint::Color::from_rgb_u8(r, g, b))),
            ),
            ("summary".to_owned(), text(|p| p.summary.clone())),
            ("biography".to_owned(), text(|p| p.biography.clone())),
            ("tags".to_owned(), text(|p| p.tags.clone())),
            ("more".to_owned(), Value::Bool(page.is_some_and(|p| p.more))),
            (
                "popular-count".to_owned(),
                Value::Number(page.map_or(0, |p| p.popular.len()) as f64),
            ),
        ]))
    }
}
