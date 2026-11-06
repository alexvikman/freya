//! Recents: what was played and added lately.

use super::*;

pub(super) fn bind(b: &Binder) {
    b.bind(
        "Recents",
        "set-filter",
        Box::new(|app, args| app.recents.set_filter(&text_arg(args, 0))),
    );
    b.bind(
        "Recents",
        "toggle",
        Box::new(|app, args| app.recents.toggle(index(args, 0))),
    );
    b.bind(
        "Recents",
        "open",
        Box::new(|app, args| app.open_recent(index(args, 0))),
    );
}
