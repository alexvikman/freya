//! The graphical player: loads a skin and connects it to the audio engine and the playlist.

mod app;
mod app_icon;
mod bindings;
mod builtin_skin;
pub mod equalizer;
mod hologram;
mod library;
mod listening;
mod native_oscilloscope;
mod native_spectrum;
mod playlists;
mod radio;
mod skin_state;
mod skin_window;
mod skins;
mod system_media;
#[cfg(test)]
mod tests;
mod title_bar;
mod visualizer;
mod window_events;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Context;
use freya_audio::Player;
use freya_skin::SkinSource;
use slint::{Timer, TimerMode};

use crate::settings::Settings;
use app::App;
use skin_window::SkinWindow;
use window_events::WindowEvents;

/// How often the app checks the library's progress while it is read.
const TICK: Duration = Duration::from_millis(33);
/// How often the app checks playback, tags, covers, dropped files and skin changes otherwise,
/// playing or not: the position shown moves once a second, a new track shows within a fifth
/// of one, and the skin's own callbacks are handled at once. Every wake costs more than the
/// checks; at 30 a second this was most of the main thread's time while music played.
const IDLE_TICK: Duration = Duration::from_millis(200);

/// Measured in M7: FemtoVG (GPU) used 0.3 % CPU idle and 1.5 % while playing on an M-series
/// MacBook, the software renderer 0.5 % and 4.1 %. So FemtoVG is the default, and Slint's own
/// choice is the fallback if it cannot start. `SLINT_BACKEND` still overrides everything.
///
/// On macOS the software renderer is the default all the same: OpenGL hands its frames to the
/// system out of step with the window, so while the window is resized its content lags and
/// jumps behind the edge. Software frames go with the window's own. Measured again with U6:
/// both use 0.2 % idle; while playing, about 3 % (FemtoVG) against 6 % (software), mostly
/// macOS converting each frame to the display's colours.
#[cfg(target_os = "macos")]
const DEFAULT_RENDERER: &str = "software";
#[cfg(not(target_os = "macos"))]
const DEFAULT_RENDERER: &str = "femtovg";

pub struct Options {
    pub files: Vec<PathBuf>,
    /// A skin file, folder or package; overrides the one in the settings.
    pub skin: Option<PathBuf>,
    /// Slint renderer: "femtovg", "skia" or "software".
    pub renderer: Option<String>,
}

pub fn run(options: Options) -> anyhow::Result<()> {
    let started = Instant::now();
    let window_events = WindowEvents::default();
    app_icon::set_name();
    select_backend(options.renderer, &window_events)?;

    let settings = Settings::load();
    // Absolute, so the saved setting still works when Freya is started from elsewhere.
    let requested = options
        .skin
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .or_else(|| settings.skin.clone())
        .map_or(SkinSource::Builtin, |p| SkinSource::from_path(&p));
    // A skin that cannot be loaded must not keep Freya from starting: fall back to the
    // built-in skin and say why in the skin picker.
    let (ui, skin, skin_error) = match SkinWindow::load(&requested) {
        Ok(ui) => (ui, requested, String::new()),
        Err(e) if requested != SkinSource::Builtin => {
            log::warn!("could not load skin, using the built-in one: {e}");
            let ui = SkinWindow::load(&SkinSource::Builtin).context("could not load skin")?;
            (
                ui,
                SkinSource::Builtin,
                format!("Could not use the saved skin: {e}"),
            )
        }
        Err(e) => return Err(e).context("could not load skin"),
    };
    log::info!("skin loaded in {:?}", started.elapsed());

    // Without a sound device Freya still runs, and plays silently in real time.
    let (player, audio_error) = match Player::new() {
        Ok(player) => (player, None),
        Err(e) => {
            log::warn!("no audio output, playing silently: {e}");
            (Player::silent(48_000), Some(format!("No sound: {e}")))
        }
    };
    player.set_volume(settings.volume);
    player.set_equalizer(&settings.equalizer.engine());
    if let Some((w, h)) = settings.window_size {
        ui.window().set_size(slint::LogicalSize::new(w, h));
    }

    let app = Rc::new(RefCell::new(App::new(
        ui.clone(),
        skin,
        player,
        settings,
        window_events.clone(),
        app::Collections {
            library: library::LibraryPanel::new(
                crate::settings::library_path(),
                crate::settings::cache_dir().map(|dir| dir.join("covers")),
            ),
            stations: radio::Stations::load(crate::settings::stations_path()),
            playlists: playlists::Playlists::load(crate::settings::playlists_dir()),
        },
    )));
    app.borrow_mut().skin_error = skin_error;
    if let Some(error) = audio_error {
        app.borrow_mut().error = error;
    }
    app.borrow_mut().start(&options.files);
    bindings::connect(&ui, &app);
    skins::attach_window(&ui);
    app.borrow_mut().system_media = system_media::SystemMedia::new(ui.window());
    {
        let app = Rc::downgrade(&app);
        system_media::on_request(move || {
            if let Some(app) = app.upgrade()
                && let Ok(mut app) = app.try_borrow_mut()
            {
                app.tick();
            }
        });
    }
    {
        let app = Rc::downgrade(&app);
        window_events.on_exit(move || {
            if let Some(app) = app.upgrade()
                && let Ok(mut app) = app.try_borrow_mut()
            {
                app.save();
            }
        });
    }
    app.borrow_mut().refresh();

    // Visualization frames run on their own timer, which only runs while there is something
    // to show; otherwise the event loop sleeps.
    let vis_timer = Timer::default();
    {
        let app = Rc::downgrade(&app);
        let mut last = Instant::now();
        vis_timer.start(TimerMode::Repeated, TICK, move || {
            let now = Instant::now();
            if let Some(app) = app.upgrade()
                && let Ok(mut app) = app.try_borrow_mut()
            {
                app.visualize(now - last);
            }
            last = now;
        });
        vis_timer.stop();
    }

    let tick_app = Rc::clone(&app);
    let timer = Rc::new(Timer::default());
    let tick_timer = Rc::downgrade(&timer);
    timer.start(TimerMode::Repeated, TICK, move || {
        // Skipped while a callback is running, e.g. during a modal file dialog.
        let Ok(mut app) = tick_app.try_borrow_mut() else {
            return;
        };
        app.tick();
        let pending = app.pending_skin.take();
        let interval = app.visualization_interval();
        let tick = if app.is_busy() { TICK } else { IDLE_TICK };
        drop(app);
        if let Some(timer) = tick_timer.upgrade()
            && timer.interval() != tick
        {
            timer.set_interval(tick);
        }
        if let Some(source) = pending {
            skins::switch_skin(&tick_app, source);
        }
        match interval {
            Some(interval) if vis_timer.interval() != interval || !vis_timer.running() => {
                vis_timer.set_interval(interval);
                vis_timer.restart();
            }
            Some(_) => {}
            None => vis_timer.stop(),
        }
    });

    // Ctrl+C and termination signals quit through the event loop, so state is still saved.
    if let Err(e) = ctrlc::set_handler(|| {
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    }) {
        log::warn!("could not install signal handler: {e}");
    }

    // Logs the time to the first frame, and with debug logging how many frames are drawn.
    let mut first = true;
    let mut frames = 0u32;
    let mut counted_since = Instant::now();
    let notified = ui.window().set_rendering_notifier(move |state, _| {
        if !matches!(state, slint::RenderingState::AfterRendering) {
            return;
        }
        if std::mem::take(&mut first) {
            log::info!("first frame after {:?}", started.elapsed());
        }
        frames += 1;
        if counted_since.elapsed() >= Duration::from_secs(5) {
            let seconds = counted_since.elapsed().as_secs_f32();
            log::debug!("{:.1} frames per second", frames as f32 / seconds);
            (frames, counted_since) = (0, Instant::now());
        }
    });
    if let Err(e) = notified {
        log::debug!("frames are not counted: {e}");
    }

    ui.show().context("could not show the window")?;
    drop(ui);
    Timer::single_shot(Duration::ZERO, app_icon::show_in_dock);
    slint::run_event_loop().context("event loop failed")?;
    app.borrow_mut().save();
    Ok(())
}

fn select_backend(requested: Option<String>, events: &WindowEvents) -> anyhow::Result<()> {
    let selector =
        || slint::BackendSelector::new().with_winit_custom_application_handler(events.handler());
    if std::env::var_os("SLINT_BACKEND").is_some() {
        return selector()
            .select()
            .context("could not start the window system");
    }
    let explicit = requested.is_some();
    let name = requested.unwrap_or_else(|| DEFAULT_RENDERER.to_owned());
    match selector().renderer_name(name.clone()).select() {
        Ok(()) => Ok(()),
        Err(e) if explicit => Err(anyhow::anyhow!("could not use renderer {name}: {e}")),
        Err(e) => {
            log::warn!("renderer {name} unavailable ({e}); using Slint's default");
            selector()
                .select()
                .context("could not start the window system")
        }
    }
}
