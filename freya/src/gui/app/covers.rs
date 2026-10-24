//! The cover of what plays: read from its tags or folder, or from the library, decoded on a
//! thread, and kept as a file for the system's media controls.

use super::*;

/// Covers are decoded at most this large; the built-in skin shows them at 88 points.
pub(super) const COVER_SIZE: u32 = 512;

/// A decoded cover, and the PNG file it was saved to for the system's Now Playing.
pub(super) type CoverResult = (PathBuf, Option<(Cover, Option<PathBuf>)>);

/// Decodes cover art on a background thread, one track at a time.
pub(super) struct Covers {
    pub(super) tx: Sender<CoverResult>,
    pub(super) rx: Receiver<CoverResult>,
    pub(super) wanted: Option<PathBuf>,
}

impl Covers {
    pub(super) fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            wanted: None,
        }
    }

    /// Reads the cover of the track at `path`: from its file, or from the library `remote`
    /// keeps it in when it is played from there, as its scan kept it when it did.
    pub(super) fn request(&mut self, path: &Path, remote: Option<Arc<dyn LibraryApi>>) {
        if self.wanted.as_deref() == Some(path) {
            return;
        }
        self.wanted = Some(path.to_owned());
        let tx = self.tx.clone();
        let path = path.to_owned();
        std::thread::spawn(move || {
            let read = match &remote {
                Some(library) => kept_cover(library.as_ref(), &path)
                    .or_else(|| library.album_art(&path, COVER_SIZE).ok().flatten())
                    .and_then(|bytes| cover_from(&bytes, COVER_SIZE)),
                None => read_cover(&path, COVER_SIZE),
            };
            let cover = read.map(|cover| {
                let file = save_cover_file(&path, &cover);
                (cover, file)
            });
            let _ = tx.send((path, cover));
        });
    }

    /// The newest cover for the wanted track, if one arrived.
    pub(super) fn poll(&self) -> Option<Option<(Cover, Option<PathBuf>)>> {
        let mut latest = None;
        for (path, cover) in self.rx.try_iter() {
            if self.wanted.as_ref() == Some(&path) {
                latest = Some(cover);
            }
        }
        if let Some(Some((_, Some(file)))) = &latest {
            remove_other_cover_files(file);
        }
        latest
    }
}

/// The cover a scan kept of the album of the track at `path` in `library`, as large as the
/// scan makes them: the server reads nothing, and a client that had it before has it here.
pub(super) fn kept_cover(library: &dyn LibraryApi, path: &Path) -> Option<Vec<u8>> {
    let track = library.track_at(path, Duration::ZERO).ok()??;
    let key = freya_media::thumbnails::Thumbnails::key(
        track.album_artist.as_deref(),
        track.album.as_deref(),
    );
    let checksum = library.album_covers().ok()?.remove(&key)?.cover?;
    library.picture(&checksum).ok()?
}

/// Where covers are saved for the system's Now Playing display.
pub(super) fn cover_files_dir() -> Option<PathBuf> {
    Some(settings::cache_dir()?.join("now-playing"))
}

/// Saves `cover` for the system's Now Playing display. Named after the track, so the system
/// does not show an old image it cached. Earlier ones are removed once this one is shown, from
/// the UI thread: a slower cover saved after it would otherwise remove it.
pub(super) fn save_cover_file(track: &Path, cover: &Cover) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let dir = cover_files_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    track.hash(&mut hasher);
    let file = dir.join(format!("cover-{:016x}.png", hasher.finish()));
    match cover.save_png(&file) {
        Ok(()) => Some(file),
        Err(e) => {
            log::debug!("could not save the cover for Now Playing: {e}");
            None
        }
    }
}

/// Removes the saved covers but `keep`.
pub(super) fn remove_other_cover_files(keep: &Path) {
    let Some(entries) = cover_files_dir().and_then(|dir| std::fs::read_dir(dir).ok()) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.path() != keep {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub(super) fn show_cover(ui: &SkinWindow, image: Option<slint::Image>) {
    let has_cover = image.is_some();
    let _ = ui.set_global_property("Player", "cover", Value::Image(image.unwrap_or_default()));
    let _ = ui.set_global_property("Player", "has-cover", Value::Bool(has_cover));
}
