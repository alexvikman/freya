//! Radio: internet radio stations.

use super::*;

pub(super) fn bind(b: &Binder) {
    b.bind(
        "Radio",
        "play",
        Box::new(|app, args| app.play_station(index(args, 0))),
    );
    b.bind(
        "Radio",
        "add",
        Box::new(|app, args| {
            if let (Some(Value::String(name)), Some(Value::String(url))) =
                (args.first(), args.get(1))
            {
                app.radio_error = app.stations.add(name, url).err().unwrap_or_default();
            }
        }),
    );
    b.bind(
        "Radio",
        "remove",
        Box::new(|app, args| app.stations.remove(index(args, 0))),
    );
    b.bind(
        "Radio",
        "find",
        Box::new(|app, args| {
            if let Some(Value::String(text)) = args.first() {
                app.radio.find(text);
            }
        }),
    );
    b.bind(
        "Radio",
        "toggle-tag",
        Box::new(|app, args| {
            if let Some(Value::String(id)) = args.first() {
                app.radio.toggle_tag(id);
            }
        }),
    );
    b.bind(
        "Radio",
        "show-shelf",
        Box::new(|app, args| app.radio.show_shelf(index(args, 0))),
    );
    b.bind("Radio", "clear-find", Box::new(|app, _| app.radio.clear()));
    b.bind("Radio", "more", Box::new(|app, _| app.radio.more()));
    b.bind(
        "Radio",
        "play-found",
        Box::new(|app, args| app.play_found(index(args, 0))),
    );
    b.bind(
        "Radio",
        "save-found",
        Box::new(|app, args| app.save_found(index(args, 0))),
    );
}
