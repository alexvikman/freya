//! LastFm: scrobbling.

use super::*;

pub(super) fn bind(b: &Binder) {
    b.bind("LastFm", "connect", Box::new(|app, _| app.lastfm.connect()));
    b.bind("LastFm", "cancel", Box::new(|app, _| app.lastfm.cancel()));
    b.bind(
        "LastFm",
        "disconnect",
        Box::new(|app, _| app.lastfm.disconnect()),
    );
    b.bind(
        "LastFm",
        "set-scrobbling",
        Box::new(|app, args| {
            if let Some(Value::Bool(on)) = args.first() {
                app.settings.lastfm_scrobbling = *on;
            }
        }),
    );
}
