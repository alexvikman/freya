//! Equalizer: its bands, presets and files.

use super::*;

pub(super) fn bind(b: &Binder) {
    b.bind(
        "Equalizer",
        "toggle",
        Box::new(|app, _| app.edit_eq(|eq| eq.enabled = !eq.enabled)),
    );
    b.bind(
        "Equalizer",
        "set-preamp",
        Box::new(|app, args| {
            let db = number(args, 0) as f32;
            app.edit_eq(|eq| eq.set_preamp(db));
        }),
    );
    b.bind(
        "Equalizer",
        "set-band",
        Box::new(|app, args| {
            let (band, db) = (index(args, 0), number(args, 1) as f32);
            app.edit_eq(|eq| eq.set_band(band, db));
        }),
    );
    b.bind(
        "Equalizer",
        "load-preset",
        Box::new(|app, args| {
            if let Some(preset) = builtin_presets().get(index(args, 0)) {
                app.edit_eq(|eq| eq.apply_preset(preset));
            }
        }),
    );
    b.bind(
        "Equalizer",
        "reset",
        Box::new(|app, _| app.edit_eq(EqualizerSettings::reset)),
    );
    // Pure: computes the curve from its arguments only.
    let _ = b.ui.set_global_callback("Equalizer", "curve", |args| {
        skin_state::string(equalizer::curve_path(
            number(args, 0) as f32,
            number(args, 1) as f32,
            &numbers(args, 2),
        ))
    });
    b.dialog("Equalizer", "load-file", pick_eqf, |app, paths| {
        let Some(path) = paths.first() else { return };
        let result = std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| read_eqf(&bytes));
        match result {
            // A library file can hold several presets; like Winamp, load the first.
            Ok(presets) => app.edit_eq(|eq| eq.apply_preset(&presets[0])),
            Err(e) => log::warn!("could not load {}: {e}", path.display()),
        }
    });
    b.dialog("Equalizer", "save-file", save_eqf_target, |app, paths| {
        let Some(path) = paths.first() else { return };
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let preset = app.settings.equalizer.as_preset(&name);
        if let Err(e) = std::fs::write(path, write_eqf(&[preset])) {
            log::warn!("could not save {}: {e}", path.display());
        }
    });
}
