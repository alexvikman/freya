//! A playlist's own page, as an album's: its picture, or a square of its tracks' covers, and a
//! wide picture across its top; its name, description and length; and each track with its
//! cover, album, when it was added and how long it is, in the order the user picks. The
//! playlist's file holds its tracks; the rest is kept in the library.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use freya_library::Entry;
use freya_library::thumbnails::{ArtistSource, Thumbnail, Thumbnails};
use freya_protocol::LibraryApi;
use freya_protocol::{PlaylistInfo, PlaylistTrack};
use slint::Image;
use slint_interpreter::{Struct, Value};

use super::{LibraryPanel, PageFiles, PagePictures, count, long_time};
use crate::gui::skin_state::{format_time, string};

/// The longest side of a playlist's own picture, and of its wide one.
const PICTURE: u32 = 1400;
const WIDE: u32 = 3840;

/// How a playlist's tracks are ordered on its page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaylistSort {
    /// As the playlist has them.
    #[default]
    Custom,
    Title,
    Artist,
    Album,
    Added,
    Duration,
}

impl PlaylistSort {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "custom" => Self::Custom,
            "title" => Self::Title,
            "artist" => Self::Artist,
            "album" => Self::Album,
            "added" => Self::Added,
            "duration" => Self::Duration,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::Title => "title",
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Added => "added",
            Self::Duration => "duration",
        }
    }
}

/// The playlist shown, as its page shows it.
pub(super) struct PlaylistPage {
    pub(super) name: String,
    pub(super) info: PlaylistInfo,
    /// Its picture's key, for `Library.cover`.
    pub(super) picture: String,
    pub(super) banner: Option<Image>,
    pub(super) color: Option<[u8; 3]>,
    /// The files of its pictures a thread was last asked to make for the page.
    pub(super) asked: Option<PageFiles>,
    /// "84 tracks · 6 h 9 min".
    pub(super) summary: String,
    /// Where in the playlist each row shown is.
    pub(super) order: Vec<usize>,
}

/// The playlist being changed, as the dialog has it.
pub(super) struct PlaylistDraft {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) picture: Option<String>,
    pub(super) backdrop: Option<String>,
    /// Small copies of them for the dialog, read once.
    pub(super) picture_shown: Image,
    pub(super) backdrop_shown: Image,
    pub(super) error: String,
}

/// Keeps the picture in `file` among the library's, at most `largest` wide or high: its
/// checksum.
pub(super) fn take_file(api: &dyn LibraryApi, file: &Path, largest: u32) -> Result<String, String> {
    let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
    api.take_picture(&bytes, largest).map_err(|e| e.to_string())
}

/// The picture `checksum` among Freya's, small enough for the dialog.
pub(super) fn preview(api: Option<&dyn LibraryApi>, checksum: Option<&String>) -> Image {
    let small = || {
        let bytes = api?.picture(checksum?).ok()??;
        let rgb = crate::gui::decode_picture(&bytes)
            .ok()?
            .thumbnail(800, 800)
            .to_rgb8();
        Some(Image::from_rgb8(
            slint::SharedPixelBuffer::clone_from_slice(rgb.as_raw(), rgb.width(), rgb.height()),
        ))
    };
    small().unwrap_or_default()
}

/// What the page knows of a track from the library.
struct Known {
    cover: String,
    album: String,
}

impl LibraryPanel {
    /// The key of the picture of the playlist `name`, for `Library.cover`.
    pub(super) fn playlist_key(name: &str) -> String {
        Thumbnails::key(Some("\u{1}playlist"), Some(name))
    }

    /// Shows the playlist `name` with its `entries`, or none; and notes when the tracks not
    /// noted before were added: now.
    pub fn show_playlist(&mut self, name: Option<&str>, entries: &[Entry]) {
        let Some(name) = name else {
            self.playlist_page = None;
            self.playlist_rows.sync(Vec::new());
            return;
        };
        let tracks: Vec<PlaylistTrack> = entries.iter().map(track_of).collect();
        let (info, mut added) = match &mut self.db {
            Some(db) => {
                if let Err(e) = db.note_playlist_added(name, &tracks, SystemTime::now()) {
                    log::warn!("could not note when tracks were added to {name}: {e}");
                }
                (
                    db.playlist_info(name).unwrap_or_default(),
                    db.playlist_added(name)
                        .map(|added| added.into_iter().collect::<HashMap<_, _>>())
                        .unwrap_or_default(),
                )
            }
            None => Default::default(),
        };
        let known = self.known_tracks(entries);
        let mut order: Vec<usize> = (0..entries.len()).collect();
        let text = |e: &Entry, f: fn(&Entry) -> Option<&String>| {
            f(e).map(|t| t.to_lowercase()).unwrap_or_default()
        };
        match self.playlist_sort {
            PlaylistSort::Custom => {}
            PlaylistSort::Title => {
                order.sort_by_cached_key(|&i| title_of(&entries[i]).to_lowercase())
            }
            PlaylistSort::Artist => {
                order.sort_by_cached_key(|&i| text(&entries[i], |e| e.artist.as_ref()))
            }
            PlaylistSort::Album => {
                order.sort_by_cached_key(|&i| text(&entries[i], |e| e.album.as_ref()))
            }
            PlaylistSort::Added => order.sort_by_key(|&i| added.get(&tracks[i]).copied()),
            PlaylistSort::Duration => order.sort_by_key(|&i| entries[i].duration),
        }
        let rows = order
            .iter()
            .enumerate()
            .map(|(row, &i)| {
                let entry = &entries[i];
                let known = known.get(&entry.path);
                let added = added.remove(&tracks[i]).map(date_text).unwrap_or_default();
                Value::Struct(Struct::from_iter([
                    ("number".to_owned(), Value::Number((row + 1) as f64)),
                    ("title".to_owned(), string(title_of(entry))),
                    (
                        "artist".to_owned(),
                        string(entry.artist.clone().unwrap_or_default()),
                    ),
                    (
                        "album".to_owned(),
                        string(
                            entry
                                .album
                                .clone()
                                .or_else(|| known.map(|k| k.album.clone()))
                                .unwrap_or_default(),
                        ),
                    ),
                    (
                        "cover".to_owned(),
                        string(known.map(|k| k.cover.clone()).unwrap_or_default()),
                    ),
                    ("added".to_owned(), string(added)),
                    (
                        "duration".to_owned(),
                        string(entry.duration.map(format_time).unwrap_or_default()),
                    ),
                ]))
            })
            .collect();
        self.playlist_rows.sync(rows);

        let total: Duration = entries.iter().filter_map(|e| e.duration).sum();
        let mut summary = count(entries.len(), "track");
        if !total.is_zero() {
            summary = format!("{summary} · {}", long_time(total));
        }
        // Its own picture, else the covers of its first albums.
        let covers = self.covers_of(entries, &known);
        let key = Self::playlist_key(name);
        let source = ArtistSource {
            picture: info.picture.clone(),
            covers,
        };
        let same = self
            .playlist_sources
            .get(&key)
            .is_some_and(|s| s.picture == source.picture && s.covers == source.covers);
        if !same {
            if let Some(thumbnails) = &mut self.thumbnails {
                thumbnails.forget_artist(&key);
            }
            self.recent_covers.remove(&key);
            self.covers_ready += 1;
            self.playlist_sources.insert(key.clone(), source);
        }
        let kept = self
            .playlist_page
            .take()
            .filter(|page| page.name == name && page.info == info && same);
        self.playlist_page = Some(PlaylistPage {
            name: name.to_owned(),
            banner: kept.as_ref().and_then(|p| p.banner.clone()),
            color: kept.as_ref().and_then(|p| p.color),
            asked: kept.and_then(|p| p.asked),
            info,
            picture: key,
            summary,
            order,
        });
        self.show_playlist_picture();
    }

    /// The covers of the first albums of `entries`, at most four, among Freya's pictures.
    fn covers_of(&self, entries: &[Entry], known: &HashMap<PathBuf, Known>) -> Vec<String> {
        let mut covers: Vec<String> = Vec::new();
        for entry in entries {
            let Some(cover) = known
                .get(&entry.path)
                .and_then(|k| self.made_covers.get(&k.cover)?.cover.clone())
            else {
                continue;
            };
            if !covers.contains(&cover) {
                covers.push(cover);
            }
            if covers.len() == 4 {
                break;
            }
        }
        covers
    }

    /// The covers of the first albums of `entries`, looked up in the library.
    pub(super) fn first_covers(&self, entries: &[Entry]) -> Vec<String> {
        // Enough tracks for four albums, mostly.
        let first = &entries[..entries.len().min(40)];
        self.covers_of(first, &self.known_tracks(first))
    }

    /// The cover's key and album of each track the library has, by its file.
    fn known_tracks(&self, entries: &[Entry]) -> HashMap<PathBuf, Known> {
        let Some(db) = &self.db else {
            return HashMap::new();
        };
        let at = |entry: &Entry| entry.part.map_or(Duration::ZERO, |p| p.start);
        let each: Vec<_> = entries
            .iter()
            .map(|entry| ("track_at", freya_protocol::args(&(&entry.path, at(entry)))))
            .collect();
        db.prefetch(&each);
        let mut known = HashMap::new();
        for entry in entries {
            if known.contains_key(&entry.path) {
                continue;
            }
            let start = entry.part.map_or(Duration::ZERO, |p| p.start);
            if let Ok(Some(track)) = db.track_at(&entry.path, start) {
                known.insert(
                    entry.path.clone(),
                    Known {
                        cover: Thumbnails::key(
                            track.album_artist.as_deref(),
                            track.album.as_deref(),
                        ),
                        album: track.album.unwrap_or_default(),
                    },
                );
            }
        }
        known
    }

    /// Where in the playlist the `row`th track shown is.
    pub fn playlist_entry(&self, row: usize) -> usize {
        self.playlist_page
            .as_ref()
            .and_then(|page| page.order.get(row).copied())
            .unwrap_or(row)
    }

    /// The playlist's places of its tracks, in the order shown.
    pub fn playlist_order(&self) -> Option<&[usize]> {
        self.playlist_page
            .as_ref()
            .map(|page| page.order.as_slice())
    }

    pub fn set_playlist_sort(&mut self, sort: &str) {
        if let Some(sort) = PlaylistSort::from_name(sort) {
            self.playlist_sort = sort;
        }
    }

    /// Notes that the tracks of the playlist `name` not noted before were added now.
    pub fn note_playlist_added(&mut self, name: &str, entries: &[Entry]) {
        let tracks: Vec<PlaylistTrack> = entries.iter().map(track_of).collect();
        if let Some(db) = &mut self.db
            && let Err(e) = db.note_playlist_added(name, &tracks, SystemTime::now())
        {
            log::warn!("could not note when tracks were added to {name}: {e}");
        }
    }

    /// The page's wide picture, and a color from it or from its picture, once made; asked of a
    /// thread the first time.
    pub(super) fn show_playlist_picture(&mut self) {
        let Some(page) = &self.playlist_page else {
            return;
        };
        let key = page.picture.clone();
        let backdrop = page.info.backdrop.clone();
        let needs = page.banner.is_none() || page.color.is_none();
        // Made, or asked to be made, as for the grid.
        self.cover(&key);
        let Some(thumbnails) = &mut self.thumbnails else {
            return;
        };
        if !needs {
            return;
        }
        let banner = match backdrop
            .as_deref()
            .map(|b| thumbnails.get_backdrop(&key, Some(b)))
        {
            Some(Thumbnail::Ready(file)) => Some(file),
            _ => None,
        };
        // For its color when it has no wide picture.
        let picture = thumbnails.ready(&key).map(|file| (file.to_owned(), false));
        let files = (banner, picture);
        let Some(page) = &mut self.playlist_page else {
            return;
        };
        if files == (None, None) || page.asked.as_ref() == Some(&files) {
            return;
        }
        page.asked = Some(files.clone());
        let made = self.page_pictures.0.clone();
        let started = std::thread::Builder::new()
            .name("freya-playlist-page".into())
            .spawn(move || {
                let _ = made.send((key, PagePictures::make(&files)));
            });
        if let Err(e) = started {
            log::warn!("could not make the playlist page's pictures: {e}");
        }
    }

    /// Shows pictures a thread made for the playlist's page, when they are of it still.
    /// Returns them otherwise, as they may be of an artist's page.
    pub(super) fn take_playlist_pictures(
        &mut self,
        key: &str,
        made: PagePictures,
    ) -> Option<PagePictures> {
        let Some(page) = self
            .playlist_page
            .as_mut()
            .filter(|page| page.picture == key)
        else {
            return Some(made);
        };
        if let Some((banner, color)) = made.banner {
            page.banner = Some(Image::from_rgb8(banner));
            page.color = color.or(page.color);
        }
        if let Some(portrait) = made.portrait
            && page.info.backdrop.is_none()
        {
            page.color = Some(portrait.color);
        }
        None
    }

    /// The playlist's page, for `Playlists.page`.
    pub fn playlist_page_value(&self) -> Value {
        let page = self.playlist_page.as_ref();
        let [r, g, b] = page
            .and_then(|p| p.color)
            .unwrap_or(freya_library::cover::page_color(&[]));
        let text = |f: fn(&PlaylistPage) -> String| string(page.map(f).unwrap_or_default());
        Value::Struct(Struct::from_iter([
            ("name".to_owned(), text(|p| p.name.clone())),
            (
                "description".to_owned(),
                text(|p| p.info.description.clone().unwrap_or_default()),
            ),
            ("picture".to_owned(), text(|p| p.picture.clone())),
            (
                "banner".to_owned(),
                Value::Image(page.and_then(|p| p.banner.clone()).unwrap_or_default()),
            ),
            (
                "has-banner".to_owned(),
                Value::Bool(page.is_some_and(|p| p.info.backdrop.is_some())),
            ),
            (
                "color".to_owned(),
                Value::Brush(slint::Brush::SolidColor(slint::Color::from_rgb_u8(r, g, b))),
            ),
            ("summary".to_owned(), text(|p| p.summary.clone())),
            ("sort".to_owned(), string(self.playlist_sort.name())),
            (
                "favorite".to_owned(),
                Value::Bool(page.is_some_and(|p| self.is_playlist_favorite(&p.name))),
            ),
        ]))
    }

    // ----- Changing a playlist -----------------------------------------------------------------

    /// Starts changing the playlist shown.
    pub fn edit_playlist(&mut self) {
        let thumbnails = self.db.as_deref();
        self.playlist_draft = self.playlist_page.as_ref().map(|page| PlaylistDraft {
            name: page.name.clone(),
            description: page.info.description.clone().unwrap_or_default(),
            picture: page.info.picture.clone(),
            backdrop: page.info.backdrop.clone(),
            picture_shown: preview(thumbnails, page.info.picture.as_ref()),
            backdrop_shown: preview(thumbnails, page.info.backdrop.as_ref()),
            error: String::new(),
        });
    }

    /// Takes the picture in `file` for the playlist being changed: "picture" or "backdrop".
    pub fn pick_playlist_picture(&mut self, kind: &str, file: &Path) {
        let (Some(draft), Some(api)) = (&mut self.playlist_draft, &self.db) else {
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

    /// Takes away the playlist's picture of `kind`: its covers, or no wide picture.
    pub fn clear_playlist_picture(&mut self, kind: &str) {
        if let Some(draft) = &mut self.playlist_draft {
            match kind {
                "backdrop" => (draft.backdrop, draft.backdrop_shown) = (None, Image::default()),
                _ => (draft.picture, draft.picture_shown) = (None, Image::default()),
            }
        }
    }

    /// Why the playlist being changed could not be kept as it is.
    pub fn set_playlist_draft_error(&mut self, error: String) {
        if let Some(draft) = &mut self.playlist_draft {
            draft.error = error;
        }
    }

    pub fn cancel_playlist_edit(&mut self) {
        self.playlist_draft = None;
    }

    /// Keeps the description and pictures of the playlist being changed, by the name `name`
    /// it has now; returns its name before.
    pub fn save_playlist_edit(&mut self, name: &str, description: &str) -> Option<String> {
        let draft = self.playlist_draft.take()?;
        let info = PlaylistInfo {
            description: Some(description.to_owned()),
            picture: draft.picture,
            backdrop: draft.backdrop,
        };
        if let Some(db) = &self.db
            && let Err(e) = db.set_playlist_info(name, &info)
        {
            log::warn!("could not keep what was changed about {name}: {e}");
        }
        Some(draft.name)
    }

    /// The playlist being changed, for `Playlists.draft`.
    pub fn playlist_draft_value(&self) -> Value {
        let draft = self.playlist_draft.as_ref();
        let picture = |f: fn(&PlaylistDraft) -> &Image| {
            Value::Image(draft.map(f).cloned().unwrap_or_default())
        };
        Value::Struct(Struct::from_iter([
            (
                "name".to_owned(),
                string(draft.map(|d| d.name.clone()).unwrap_or_default()),
            ),
            (
                "description".to_owned(),
                string(draft.map(|d| d.description.clone()).unwrap_or_default()),
            ),
            ("shown".to_owned(), Value::Bool(draft.is_some())),
            ("picture".to_owned(), picture(|d| &d.picture_shown)),
            ("backdrop".to_owned(), picture(|d| &d.backdrop_shown)),
            (
                "error".to_owned(),
                string(draft.map(|d| d.error.clone()).unwrap_or_default()),
            ),
        ]))
    }
}

/// A playlist's entry as the library notes it: its file, and where in it it starts.
fn track_of(entry: &Entry) -> PlaylistTrack {
    let start = entry.part.map_or(0, |p| p.start.as_millis() as i64);
    (entry.path.clone(), start)
}

/// An entry's title: its tags', else its file's name.
fn title_of(entry: &Entry) -> String {
    entry.title.clone().unwrap_or_else(|| {
        entry
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

/// A day as the page shows it: "23 Dec 2025", in local time.
fn date_text(at: SystemTime) -> String {
    jiff::Timestamp::try_from(at)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%-d %b %Y")
                .to_string()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use freya_library::Library;

    use super::*;

    fn entry(title: &str, artist: &str, seconds: u64) -> Entry {
        Entry {
            title: Some(title.into()),
            artist: Some(artist.into()),
            duration: Some(Duration::from_secs(seconds)),
            ..Entry::new(format!("/m/{title}.flac"))
        }
    }

    #[test]
    fn shows_a_playlist_in_the_order_picked() {
        let mut panel = LibraryPanel::new(None, None);
        panel.open(crate::gui::library::tests::local(
            Library::open_in_memory().unwrap(),
            None,
        ));
        let entries = [
            entry("Swan Lake", "Tchaikovsky", 167),
            entry("Fur Elise", "Beethoven", 360),
            entry("Merry-Go-Round", "Vitamin String Quartet", 185),
        ];
        panel.show_playlist(Some("Royal"), &entries);
        let page = panel.playlist_page.as_ref().unwrap();
        assert_eq!(page.summary, "3 tracks · 11 min 52 s");
        assert_eq!(page.order, [0, 1, 2]);

        panel.set_playlist_sort("title");
        panel.show_playlist(Some("Royal"), &entries);
        assert_eq!(panel.playlist_order(), Some(&[1, 2, 0][..]));
        assert_eq!(
            panel.playlist_entry(0),
            1,
            "the first row shown is Fur Elise"
        );

        // Added when first shown, as the file does not say.
        let added = panel.db.as_ref().unwrap().playlist_added("Royal").unwrap();
        assert_eq!(added.len(), 3);

        panel.show_playlist(None, &[]);
        assert!(panel.playlist_page.is_none());
    }

    #[test]
    fn keeps_a_playlists_description() {
        let mut panel = LibraryPanel::new(None, None);
        panel.open(crate::gui::library::tests::local(
            Library::open_in_memory().unwrap(),
            None,
        ));
        panel.show_playlist(Some("Royal"), &[]);
        panel.edit_playlist();
        assert_eq!(
            panel
                .save_playlist_edit("Royal", "Moonlit encounters")
                .as_deref(),
            Some("Royal")
        );
        panel.show_playlist(Some("Royal"), &[]);
        let page = panel.playlist_page.as_ref().unwrap();
        assert_eq!(page.info.description.as_deref(), Some("Moonlit encounters"));
    }
}
