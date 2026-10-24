//! Freya's own updates.

use super::*;

impl App {
    /// Looks for an update when it is time, and restarts into one that was installed.
    pub(super) fn follow_updates(&mut self) {
        // Not in tests, and not before Freya has settled after starting.
        let wanted = !cfg!(test)
            && self.view.updates_out.available
            && self.settings.check_for_updates
            && self.started.elapsed() >= UPDATE_CHECK_DELAY
            && matches!(self.updater.state(), update::State::Idle)
            && update::due(self.settings.last_update_check, SystemTime::now());
        if wanted {
            self.settings.last_update_check = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs());
            let skipped = self.settings.skipped_update.clone();
            self.updater.check(false, skipped.as_deref());
        }
        if self.updater.poll()
            && matches!(self.updater.state(), update::State::Installed(_))
            && self.updater.restart()
        {
            log::info!("restarting into the update");
            let _ = slint::quit_event_loop();
        }
    }
}
