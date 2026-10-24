//! Player: playing, seeking and the volume.

use super::*;

pub(super) fn bind(b: &Binder) {
    b.bind("Player", "play", Box::new(|app, _| app.play()));
    b.bind(
        "Player",
        "pause",
        Box::new(|app, _| app.player.set_paused(true)),
    );
    b.bind(
        "Player",
        "toggle-pause",
        Box::new(|app, _| app.toggle_pause()),
    );
    b.bind("Player", "stop", Box::new(|app, _| app.stop()));
    b.bind("Player", "next", Box::new(|app, _| app.step(true)));
    b.bind("Player", "previous", Box::new(|app, _| app.step(false)));
    b.bind(
        "Player",
        "seek",
        Box::new(|app, args| app.seek(number(args, 0))),
    );
    b.bind(
        "Player",
        "seek-by",
        Box::new(|app, args| app.seek_by(number(args, 0))),
    );
    b.bind(
        "Player",
        "set-volume",
        Box::new(|app, args| app.set_volume((number(args, 0) as f32).clamp(0.0, 1.0))),
    );
    b.bind(
        "Player",
        "toggle-mute",
        Box::new(|app, _| app.toggle_mute()),
    );
    b.dialog("Player", "open-files", pick_audio, |app, paths| {
        app.replace_and_play(&paths);
    });
}
