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
}
