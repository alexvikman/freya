//! The graphical player: loads a skin and connects it to the audio engine and the playlist.

mod app;
mod app_icon;
mod bindings;
mod builtin_skin;
mod cover_decoder;
pub mod equalizer;
mod history;
mod hologram;
mod library;
mod listening;
mod match_finder;
mod native_oscilloscope;
mod native_spectrum;
mod page_proxy;
mod picture_finder;
mod playlists;
mod radio;
mod server_panel;
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

/// How many ticks try to connect to the system's media controls before Freya does without:
/// on Windows the first may come before the window exists.
const MEDIA_TRIES: u8 = 5;

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
    let _ = app::STARTED.set(started);
    freya_client::window_draws_on_this_thread();
    let window_events = WindowEvents::default();
    app_icon::set_name();
    select_backend(options.renderer, &window_events)?;

    let settings = Settings::load();
    // Absolute, so the saved setting still works when Freya is started from elsewhere.
    let saved_skin = options
        .skin
        .is_none()
        .then(|| settings.skin.clone())
        .flatten();
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

    // The server on another computer the user chose, else this computer's.
    let source = settings
        .server
        .as_ref()
        .and_then(|server| {
            Some(library::LibrarySource::Remote {
                pin: server.pin()?,
                name: server.name.clone(),
            })
        })
        .or_else(local_source);
    let app = Rc::new(RefCell::new(App::new(
        ui.clone(),
        skin,
        player,
        settings,
        window_events.clone(),
        app::Collections {
            library: library::LibraryPanel::new(
                source,
                crate::settings::cache_dir().map(|dir| dir.join("covers")),
            ),
            stations: radio::Stations::load(crate::settings::stations_path()),
            playlists: playlists::Playlists::new(),
        },
    )));
    if !skin_error.is_empty() {
        app.borrow_mut().unloaded_skin = saved_skin;
    }
    app.borrow_mut().skin_error = skin_error;
    if let Some(error) = audio_error {
        app.borrow_mut().error = error;
    }
    app.borrow_mut().start(&options.files);
    bindings::connect(&ui, &app);
    skins::attach_window(&ui);
    app.borrow_mut().media_tries = MEDIA_TRIES;
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
        window_events.on_wake(move || {
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

/// Freya's server, beside this program.
pub fn server_program() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.with_file_name(format!("freya-server{}", std::env::consts::EXE_SUFFIX)))
}

/// A picture in `bytes` from a server or the internet, decoded within bounds (see
/// [`freya_library::decode_picture`]).
pub fn decode_picture(bytes: &[u8]) -> image::ImageResult<image::DynamicImage> {
    freya_library::decode_picture(bytes)
}

/// The library of Freya's server on this computer, started from beside this program.
pub fn local_source() -> Option<library::LibrarySource> {
    let address = freya_server::LOCAL_ADDRESS.parse().ok()?;
    server_program().map(|program| library::LibrarySource::Local { address, program })
}
