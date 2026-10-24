//! The skin: putting a new one in place, and reloading one from a folder as it is saved.

use super::*;

impl App {
    /// Puts a newly loaded skin in place of the current one. The returned view is the old one,
    /// whose window the caller hides.
    pub fn replace_view(&mut self, ui: SkinWindow, skin: SkinSource) -> View {
        // The new view starts with the visualization tap off; it turns it on when wanted.
        self.player.set_visualization(false);
        let old = std::mem::replace(&mut self.view, View::new(ui));
        (self.window_hidden, self.window_focused) = (false, true);
        self.skin = skin;
        self.skin_error.clear();
        self.unloaded_skin = None;
        self.playlist_dirty = true;
        show_cover(&self.view.ui, self.cover.clone());
        self.watch_skin();
        self.attach_library();
        self.refresh();
        old
    }

    /// Remembers the modification times of the skin's files, to notice when they change.
    pub fn watch_skin(&mut self) {
        self.skin_files = self
            .skin
            .watched_files()
            .into_iter()
            .map(|p| {
                let modified = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
                (p, modified)
            })
            .collect();
        self.skin_checked = Instant::now();
    }

    /// Asks for the skin to be reloaded if any of its files changed since it was loaded.
    pub(super) fn check_skin_files(&mut self) {
        if self.skin_files.is_empty() || self.skin_checked.elapsed() < SKIN_WATCH_INTERVAL {
            return;
        }
        self.skin_checked = Instant::now();
        let current: Vec<PathBuf> = self.skin.watched_files();
        let changed = current.len() != self.skin_files.len()
            || self.skin_files.iter().any(|(path, modified)| {
                std::fs::metadata(path).and_then(|m| m.modified()).ok() != *modified
            });
        if changed {
            log::info!("skin files changed; reloading");
            self.pending_skin = Some(self.skin.clone());
            // Even if the reload fails, the same edit should not be retried every second.
            self.watch_skin();
        }
    }
}
