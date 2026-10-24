//! Updates: Freya's own.

use super::*;

pub(super) fn bind(b: &Binder) {
    b.bind(
        "Updates",
        "check",
        Box::new(|app, _| app.updater.check(true, None)),
    );
    b.bind(
        "Updates",
        "install",
        Box::new(|app, _| app.updater.install()),
    );
    b.bind("Updates", "later", Box::new(|app, _| app.updater.later()));
    b.bind(
        "Updates",
        "skip",
        Box::new(|app, _| {
            if let update::State::Available(release) = app.updater.state() {
                app.settings.skipped_update = Some(release.version().to_owned());
            }
            app.updater.later();
        }),
    );
    b.bind(
        "Updates",
        "set-check-automatically",
        Box::new(|app, args| {
            if let Some(Value::Bool(on)) = args.first() {
                app.settings.check_for_updates = *on;
            }
        }),
    );
}
