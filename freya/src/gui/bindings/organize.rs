//! Organize: going through the music's folders and changing what their files say.

use super::*;

pub(super) fn bind(b: &Binder) {
    b.bind(
        "Organize",
        "act",
        Box::new(|app, args| {
            let action = text_arg(args, 0);
            let value = text_arg(args, 1);
            let remote = app.library.remote();
            if let Some((tracks, start)) = app.organize.act(remote, &action, &value) {
                app.play_tracks(&tracks, start);
            }
        }),
    );
    b.bind(
        "Organize",
        "edit-field",
        Box::new(|app, args| {
            let remote = app.library.remote();
            app.organize
                .edit_field(remote, &text_arg(args, 0), &text_arg(args, 1));
        }),
    );
}
