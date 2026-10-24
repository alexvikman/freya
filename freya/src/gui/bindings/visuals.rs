//! Visualizer: its modes, and the paths drawn from its data.

use super::*;

pub(super) fn bind(b: &Binder) {
    b.bind(
        "Visualizer",
        "cycle-mode",
        Box::new(|app, _| {
            let without_hologram = app.view.visualizer.without_hologram();
            app.settings.visualizer_mode =
                visualizer::next_mode(&app.settings.visualizer_mode, without_hologram).to_owned();
        }),
    );
    b.bind(
        "Visualizer",
        "set-mode",
        Box::new(|app, args| {
            if let Some(Value::String(mode)) = args.first()
                && visualizer::is_mode(mode)
            {
                app.settings.visualizer_mode = mode.to_string();
            }
        }),
    );
    b.bind(
        "Visualizer",
        "set-fps",
        Box::new(|app, args| {
            if let Some(fps) = visualizer::fps(number(args, 0)) {
                app.settings.visualizer_fps = fps;
            }
        }),
    );
    // Pure: these compute paths from their arguments only.
    let _ = b.ui.set_global_callback("Visualizer", "bars-path", |args| {
        skin_state::string(freya_skin::paths::bars_path(
            number(args, 0) as f32,
            number(args, 1) as f32,
            &numbers(args, 2),
        ))
    });
    let _ =
        b.ui.set_global_callback("Visualizer", "peaks-path", |args| {
            skin_state::string(freya_skin::paths::peaks_path(
                number(args, 0) as f32,
                number(args, 1) as f32,
                &numbers(args, 2),
                number(args, 3) as f32,
            ))
        });
    let _ = b.ui.set_global_callback("Visualizer", "wave", |args| {
        skin_state::string(freya_skin::paths::wave_path(
            number(args, 0) as f32,
            number(args, 1) as f32,
            &numbers(args, 2),
        ))
    });
}
