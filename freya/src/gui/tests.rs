//! Drives the built-in skin's callbacks against the app, without a window or audio device.
//!
//! Everything runs in one test: Slint's headless platform can only be set up once per process,
//! and its windows belong to the thread that created them.

use std::path::Path;

use crate::eq_presets::builtin_presets;
use freya_skin::SkinSource;
use slint::Model;
use slint_interpreter::{Struct, Value};

use super::app::App;
use super::skin_window::SkinWindow;
use super::*;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/freya-audio/tests/fixtures")
        .join(name)
}

fn rows(ui: &SkinWindow) -> Vec<Struct> {
    let Ok(Value::Model(model)) = ui.get_global_property("Playlist", "rows") else {
        panic!("Playlist.rows is a model");
    };
    model
        .iter()
        .map(|v| match v {
            Value::Struct(s) => s,
            other => panic!("row is not a struct: {other:?}"),
        })
        .collect()
}

fn field(row: &Struct, name: &str) -> Value {
    row.get_field(name).cloned().unwrap_or_default()
}

fn titles(ui: &SkinWindow) -> Vec<String> {
    rows(ui)
        .iter()
        .map(|r| match field(r, "title") {
            Value::String(s) => s.to_string(),
            _ => String::new(),
        })
        .collect()
}

fn selected(ui: &SkinWindow) -> Vec<bool> {
    rows(ui)
        .iter()
        .map(|r| field(r, "selected") == Value::Bool(true))
        .collect()
}

fn global(ui: &SkinWindow, global: &str, name: &str) -> Value {
    ui.get_global_property(global, name).unwrap()
}

fn call(ui: &SkinWindow, global: &str, name: &str, args: &[Value]) -> Value {
    ui.invoke_global(global, name, args).unwrap()
}

fn num(n: f64) -> Value {
    Value::Number(n)
}

fn text(s: &str) -> Value {
    skin_state::string(s)
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("freya-gui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn library_titles(ui: &SkinWindow) -> Vec<String> {
    let Ok(Value::Model(model)) = ui.get_global_property("Library", "rows") else {
        panic!("Library.rows is a model");
    };
    model
        .iter()
        .map(|v| match v {
            Value::Struct(s) => match field(&s, "title") {
                Value::String(s) => s.to_string(),
                _ => String::new(),
            },
            other => panic!("row is not a struct: {other:?}"),
        })
        .collect()
}

/// Ticks the app until the library scan is done: within a minute, which only a scan that hangs
/// takes, and not a busy machine running the other tests beside it.
fn wait_for_scan(app: &Rc<RefCell<App>>) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        app.borrow_mut().tick();
        if global(&current_ui(app), "Library", "scanning") == Value::Bool(false) {
            return;
        }
        assert!(Instant::now() < deadline, "library scan did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A web server on localhost: `/stream` is a radio station playing `audio` once, `/list.pls`
/// a station playlist pointing at it.
fn radio_server(audio: Vec<u8>) -> String {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let pls = format!("[playlist]\nFile1={base}/stream\nTitle1=Local - Test FM\nLength1=-1\n");
    std::thread::spawn(move || {
        for connection in listener.incoming().flatten() {
            let mut reader = BufReader::new(connection);
            let mut request = String::new();
            let _ = reader.read_line(&mut request);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let mut connection = reader.into_inner();
            if request.contains("/list.pls") {
                let _ = write!(
                    connection,
                    "HTTP/1.0 200 OK\r\nContent-Type: audio/x-scpls\r\n\r\n{pls}"
                );
            } else {
                let _ = connection.write_all(
                    b"HTTP/1.0 200 OK\r\nContent-Type: audio/mpeg\r\nicy-name: Test Radio\r\n\r\n",
                );
                let _ = connection.write_all(&audio);
            }
        }
    });
    base
}

fn current_ui(app: &Rc<RefCell<App>>) -> SkinWindow {
    app.borrow().view.ui.clone()
}

#[test]
fn skin_callbacks_drive_the_app() {
    // Freya's log, shown if the test fails.
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Info)
        .try_init();
    freya_skin::render::init_headless().unwrap();
    let ui = SkinWindow::load(&SkinSource::Builtin).unwrap();
    assert!(
        matches!(ui, SkinWindow::Builtin(_)),
        "the built-in skin is compiled in"
    );
    let app = Rc::new(RefCell::new(App::new(
        ui.clone(),
        SkinSource::Builtin,
        Player::silent(48_000),
        Settings::default(),
        WindowEvents::default(),
        app::Collections {
            library: library::LibraryPanel::new(
                Some(library::LibrarySource::InProcess {
                    database: temp_dir("library").join("library.sqlite3"),
                    pictures: Some(temp_dir("pictures")),
                }),
                Some(temp_dir("covers")),
            ),
            stations: radio::Stations::new(),
            playlists: playlists::Playlists::new(),
        },
    )));
    bindings::connect(&ui, &app);

    // Adding files shows them as rows.
    app.borrow_mut().add_paths(&[
        fixture("mono-22k.wav"),
        fixture("sine-96k-24bit.wav"),
        fixture("sine.flac"),
    ]);
    app.borrow_mut().refresh();
    assert_eq!(titles(&ui), ["mono-22k", "sine-96k-24bit", "sine"]);
    assert_eq!(global(&ui, "Playlist", "summary"), text("3 tracks · 0:00+"));

    // Click, then Shift+Down.
    call(
        &ui,
        "Playlist",
        "select",
        &[num(1.0), Value::Bool(false), Value::Bool(false)],
    );
    assert_eq!(selected(&ui), [false, true, false]);
    call(
        &ui,
        "Playlist",
        "move-cursor",
        &[num(1.0), Value::Bool(true)],
    );
    assert_eq!(selected(&ui), [false, true, true]);
    assert_eq!(global(&ui, "Playlist", "cursor"), num(2.0));

    // Drag the selection to the top, then remove it.
    call(&ui, "Playlist", "move-selected", &[num(0.0)]);
    assert_eq!(titles(&ui), ["sine-96k-24bit", "sine", "mono-22k"]);
    call(&ui, "Playlist", "remove-selected", &[]);
    assert_eq!(titles(&ui), ["mono-22k"]);

    // Shuffle and repeat.
    call(&ui, "Playlist", "toggle-repeat", &[]);
    assert_eq!(global(&ui, "Playlist", "repeat"), Value::Bool(true));

    // Equalizer: a band, a preset, on/off, and the curve.
    call(&ui, "Equalizer", "set-band", &[num(0.0), num(5.0)]);
    let Value::Model(bands) = global(&ui, "Equalizer", "bands") else {
        panic!("bands is a model");
    };
    assert_eq!(bands.row_data(0), Some(num(5.0)));
    assert_eq!(global(&ui, "Equalizer", "preset-name"), text("Custom"));
    let rock = builtin_presets()
        .iter()
        .position(|p| p.name == "Rock")
        .unwrap();
    call(&ui, "Equalizer", "load-preset", &[num(rock as f64)]);
    assert_eq!(global(&ui, "Equalizer", "preset-name"), text("Rock"));
    call(&ui, "Equalizer", "toggle", &[]);
    assert_eq!(global(&ui, "Equalizer", "enabled"), Value::Bool(true));
    assert!(
        app.borrow().player.equalizer().enabled,
        "engine gets the settings"
    );
    let Value::String(curve) = call(
        &ui,
        "Equalizer",
        "curve",
        &[num(300.0), num(100.0), Value::Model(bands)],
    ) else {
        panic!("curve is a string");
    };
    assert!(curve.starts_with("M0.00 "), "{curve}");

    // App settings.
    call(&ui, "App", "set-theme", &[text("dark")]);
    assert_eq!(global(&ui, "App", "theme"), text("dark"));
    call(&ui, "App", "set-theme", &[text("purple")]);
    assert_eq!(
        global(&ui, "App", "theme"),
        text("dark"),
        "unknown themes are ignored"
    );
    call(&ui, "App", "toggle-equalizer", &[]);
    assert_eq!(global(&ui, "App", "equalizer-visible"), Value::Bool(true));

    // Volume and playback.
    call(&ui, "Player", "set-volume", &[num(0.25)]);
    assert_eq!(global(&ui, "Player", "volume"), num(0.25));
    // Muted, it plays at 0 until unmuted, or a volume is chosen.
    call(&ui, "Player", "toggle-mute", &[]);
    assert_eq!(global(&ui, "Player", "muted"), Value::Bool(true));
    assert_eq!(global(&ui, "Player", "volume"), num(0.0));
    call(&ui, "Player", "toggle-mute", &[]);
    assert_eq!(global(&ui, "Player", "muted"), Value::Bool(false));
    assert_eq!(global(&ui, "Player", "volume"), num(0.25));
    call(&ui, "Player", "toggle-mute", &[]);
    call(&ui, "Player", "set-volume", &[num(0.5)]);
    assert_eq!(global(&ui, "Player", "muted"), Value::Bool(false));
    assert_eq!(global(&ui, "Player", "volume"), num(0.5));
    call(&ui, "Player", "set-volume", &[num(0.25)]);
    call(&ui, "Playlist", "activate", &[num(0.0)]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let playing = Value::EnumerationValue("PlaybackState".into(), "playing".into());
    while global(&ui, "Player", "state") != playing {
        assert!(Instant::now() < deadline, "playback did not start");
        std::thread::sleep(Duration::from_millis(20));
        app.borrow_mut().tick();
    }
    assert_eq!(global(&ui, "Player", "title"), text("mono-22k"));

    // Visualization: computed only while the skin shows it. The built-in skin shows it in a
    // view of its own, so act as if that view is open, with 24 bars; a 440 Hz tone lights some.
    app.borrow_mut().tick();
    assert!(app.borrow().visualization_interval().is_none());
    ui.set_global_property("Visualizer", "showing", Value::Bool(true))
        .unwrap();
    ui.set_global_property("Visualizer", "bar-count", num(24.0))
        .unwrap();
    app.borrow_mut().tick();
    let mut lit = false;
    for _ in 0..40 {
        let frame = visualizer::frame_interval(60);
        std::thread::sleep(frame);
        app.borrow_mut().tick();
        app.borrow_mut().visualize(frame);
        let Value::Model(bars) = global(&ui, "Visualizer", "bars") else {
            panic!("bars is a model");
        };
        assert_eq!(bars.row_count(), 24);
        if bars
            .iter()
            .any(|v| matches!(v, Value::Number(n) if n > 0.3))
        {
            lit = true;
            break;
        }
    }
    assert!(lit, "spectrum stayed dark while playing");
    assert!(app.borrow().visualization_interval().is_some());
    call(&ui, "Visualizer", "set-mode", &[text("off")]);
    call(&ui, "Visualizer", "set-mode", &[text("sparkles")]);
    app.borrow_mut().refresh();
    assert_eq!(global(&ui, "Visualizer", "mode"), text("off"));
    call(&ui, "Visualizer", "set-mode", &[text("spectrum")]);
    call(&ui, "Visualizer", "cycle-mode", &[]);
    assert_eq!(global(&ui, "Visualizer", "mode"), text("oscilloscope"));
    app.borrow_mut().visualize(visualizer::frame_interval(60));
    let Value::Model(wave) = global(&ui, "Visualizer", "waveform") else {
        panic!("waveform is a model");
    };
    assert_eq!(wave.row_count(), 128);

    // Switching to the Mini skin, which only has the Player global.
    let mini = Path::new(env!("CARGO_MANIFEST_DIR")).join("../skins/mini");
    skins::switch_skin(&app, SkinSource::Folder(mini.clone()));
    assert_eq!(app.borrow().skin, SkinSource::Folder(mini));
    let mini_ui = current_ui(&app);
    assert!(mini_ui.get_global_property("Playlist", "rows").is_err());
    app.borrow_mut().tick();
    assert_eq!(global(&mini_ui, "Player", "title"), text("mono-22k"));
    call(&mini_ui, "Player", "set-volume", &[num(0.5)]);
    assert_eq!(app.borrow().player.volume(), 0.5);

    // A broken skin keeps the current one and explains why.
    let dir = temp_dir("skin");
    std::fs::write(dir.join("skin.slint"), "this is not slint").unwrap();
    skins::switch_skin(&app, SkinSource::Folder(dir.clone()));
    assert!(
        app.borrow().skin_error.contains("does not compile"),
        "{}",
        app.borrow().skin_error
    );
    assert!(matches!(app.borrow().skin, SkinSource::Folder(ref p) if p.ends_with("mini")));

    // A skin in use reloads when its files change.
    let skin = r#"import { Player } from "@freya/v1.slint";
export { Player } from "@freya/v1.slint";
export component W inherits Window { Text { text: Player.title; } }
"#;
    std::fs::write(dir.join("skin.slint"), skin).unwrap();
    skins::switch_skin(&app, SkinSource::Folder(dir.clone()));
    assert!(app.borrow().skin_error.is_empty());
    let before = current_ui(&app);
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::write(
        dir.join("skin.slint"),
        skin.replace("Player.title", "Player.artist"),
    )
    .unwrap();
    app.borrow_mut().tick();
    let pending = app.borrow_mut().pending_skin.take();
    assert_eq!(
        pending,
        Some(SkinSource::Folder(dir.clone())),
        "a changed file asks for a reload"
    );
    skins::switch_skin(&app, pending.unwrap());
    assert!(matches!(current_ui(&app), SkinWindow::Loaded(_)));
    assert!(
        before.window().size() == current_ui(&app).window().size(),
        "a reload keeps the window size"
    );

    // And back to the built-in skin, which shows the playlist again.
    skins::switch_skin(&app, SkinSource::Builtin);
    app.borrow_mut().tick();
    assert_eq!(titles(&current_ui(&app)), ["mono-22k"]);
    assert_eq!(
        global(&ui, "Player", "format-info"),
        text("PCM_S16LE · 353 kbps · 22.05 kHz · Mono")
    );

    // The library: a music folder is scanned, browsed, searched and played from.
    let ui = current_ui(&app);
    assert_eq!(global(&ui, "Library", "title"), text("Artists"));
    assert!(library_titles(&ui).is_empty());
    let music = temp_dir("music");
    for name in ["sine.mp3", "sine.flac", "mono-22k.wav"] {
        std::fs::copy(fixture(name), music.join(name)).unwrap();
    }
    call(&ui, "Library", "edit-folders", &[]);
    app.borrow_mut().add_draft_folder(music.clone());
    call(&ui, "Library", "save-folders", &[]);
    wait_for_scan(&app);
    assert_eq!(library_titles(&ui), ["Freya", "Unknown artist"]);
    assert_eq!(global(&ui, "Library", "status"), text("3 tracks · 1 album"));
    let Value::Model(folders) = global(&ui, "Library", "folders") else {
        panic!("folders is a model");
    };
    assert_eq!(folders.row_count(), 1);

    call(&ui, "Library", "activate", &[num(1.0)]);
    assert_eq!(global(&ui, "Library", "title"), text("Unknown artist"));
    assert_eq!(library_titles(&ui), ["Unknown album"]);
    call(&ui, "Library", "activate", &[num(0.0)]);
    assert_eq!(library_titles(&ui), ["mono-22k", "sine"]);
    assert_eq!(global(&ui, "Library", "can-go-back"), Value::Bool(true));
    call(&ui, "Library", "back", &[]);
    call(&ui, "Library", "back", &[]);
    assert_eq!(global(&ui, "Library", "title"), text("Artists"));
    assert_eq!(global(&ui, "Library", "can-go-back"), Value::Bool(false));

    call(&ui, "Library", "search", &[text("sine te")]);
    assert_eq!(library_titles(&ui), ["Sine Test"]);
    assert_eq!(global(&ui, "Library", "query"), text("sine te"));
    call(&ui, "Library", "activate", &[num(0.0)]);
    assert_eq!(
        titles(&ui),
        ["Freya - Sine Test"],
        "the playlist becomes the results"
    );
    call(&ui, "Library", "enqueue", &[num(0.0)]);
    assert_eq!(titles(&ui).len(), 2);
    call(&ui, "Library", "back", &[]);
    assert_eq!(
        global(&ui, "Library", "query"),
        text(""),
        "back ends the search"
    );
    assert_eq!(library_titles(&ui), ["Freya", "Unknown artist"]);

    // Playing an artist plays all of their tracks.
    call(&ui, "Library", "play", &[num(1.0)]);
    assert_eq!(titles(&ui), ["mono-22k", "sine"]);

    // The other sections, and back retracing the way in.
    call(&ui, "Library", "show", &[text("songs")]);
    assert_eq!(global(&ui, "Library", "layout"), text("table"));
    // Untitled tracks (named by their files) come last.
    assert_eq!(library_titles(&ui), ["Sine Test", "mono-22k", "sine"]);
    call(&ui, "Library", "show", &[text("genres")]);
    assert_eq!(global(&ui, "Library", "section"), text("genres"));
    assert_eq!(library_titles(&ui), ["Unknown genre"]);
    call(&ui, "Library", "activate", &[num(0.0)]);
    assert_eq!(global(&ui, "Library", "layout"), text("grid"));
    assert_eq!(library_titles(&ui), ["Fixtures", "Unknown album"]);
    call(&ui, "Library", "activate", &[num(1.0)]);
    assert_eq!(global(&ui, "Library", "layout"), text("list"));
    call(&ui, "Library", "back", &[]);
    assert_eq!(global(&ui, "Library", "title"), text("Unknown genre"));
    call(&ui, "Library", "show", &[text("albums")]);
    assert_eq!(global(&ui, "Library", "title"), text("Albums"));
    assert_eq!(global(&ui, "Library", "can-go-back"), Value::Bool(false));

    // The Recents: albums by when they came, and tracks by when and how often they were
    // played.
    call(&ui, "Library", "show", &[text("recently-added")]);
    assert_eq!(global(&ui, "Library", "layout"), text("grid"));
    assert_eq!(library_titles(&ui).len(), 2);
    call(&ui, "Library", "show", &[text("recently-played")]);
    assert_eq!(global(&ui, "Library", "title"), text("Recently Played"));
    assert!(library_titles(&ui).is_empty());
    for _ in 0..2 {
        app.borrow_mut()
            .library
            .record_play(music.join("mono-22k.wav"), None);
    }
    app.borrow_mut().tick();
    assert_eq!(library_titles(&ui), ["mono-22k"]);
    let played = |ui: &SkinWindow| -> Vec<Value> {
        let Value::Model(model) = global(ui, "Library", "rows") else {
            panic!("Library.rows is a model");
        };
        model
            .iter()
            .map(|row| match row {
                Value::Struct(row) => field(&row, "played"),
                other => panic!("row is not a struct: {other:?}"),
            })
            .collect()
    };
    assert_eq!(played(&ui), [text("Just now")]);
    call(&ui, "Library", "show", &[text("most-played")]);
    assert_eq!(global(&ui, "Library", "layout"), text("table"));
    assert_eq!(played(&ui), [text("2 plays")]);
    call(&ui, "Library", "activate", &[num(0.0)]);
    assert_eq!(titles(&ui), ["mono-22k"]);
    call(&ui, "Library", "show", &[text("artists")]);

    // Radio: none saved at first; saving one, playing it twice without a second entry, and
    // removing it.
    let stations = |ui: &SkinWindow| match global(ui, "Radio", "stations") {
        Value::Model(model) => model.row_count(),
        other => panic!("stations is a model, not {other:?}"),
    };
    assert_eq!(stations(&ui), 0);
    call(&ui, "Radio", "add", &[text("Nothing"), text(" ")]);
    assert_eq!(global(&ui, "Radio", "error"), text("No address given"));
    call(&ui, "Radio", "add", &[text(""), text("127.0.0.1:9/live")]);
    assert_eq!(global(&ui, "Radio", "error"), text(""));
    assert_eq!(stations(&ui), 1);
    let queued = titles(&ui).len();
    call(&ui, "Radio", "play", &[num(0.0)]);
    call(&ui, "Radio", "play", &[num(0.0)]);
    assert_eq!(
        titles(&ui).len(),
        queued + 1,
        "a station already queued plays from there"
    );
    call(&ui, "Radio", "remove", &[num(0.0)]);
    assert_eq!(stations(&ui), 0);
    call(&ui, "Player", "stop", &[]);

    // The radio directory: its first page, a tag looked for, a station of it kept among the
    // server's and no longer, and back.
    let tick_until = |what: &str, done: &dyn Fn(&SkinWindow) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            app.borrow_mut().tick();
            if done(&ui) {
                return;
            }
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let rows = |ui: &SkinWindow, name: &str| match global(ui, "Radio", name) {
        Value::Model(model) => model.row_count(),
        other => panic!("{name} is a model, not {other:?}"),
    };
    tick_until("the directory's first page", &|ui| rows(ui, "shelves") > 0);
    assert_eq!(global(&ui, "Radio", "total"), num(-1.0));
    assert!(rows(&ui, "tags") > 0);
    call(&ui, "Radio", "toggle-tag", &[text("genre:jazz")]);
    tick_until("jazz stations", &|ui| {
        global(ui, "Radio", "searching") == Value::Bool(false) && rows(ui, "found") > 0
    });
    assert_eq!(rows(&ui, "chosen"), 1);
    assert_ne!(global(&ui, "Radio", "total"), num(-1.0));
    call(&ui, "Radio", "save-found", &[num(0.0)]);
    assert_eq!(stations(&ui), 1);
    call(&ui, "Radio", "save-found", &[num(0.0)]);
    assert_eq!(stations(&ui), 0);
    call(&ui, "Radio", "clear-find", &[]);
    assert_eq!(global(&ui, "Radio", "total"), num(-1.0));
    assert_eq!(rows(&ui, "chosen"), 0);

    // Playlists: made empty or from Up Next, filled from the library, played, edited, deleted.
    let count = |ui: &SkinWindow, name: &str| match global(ui, "Playlists", name) {
        Value::Model(model) => model.row_count(),
        other => panic!("Playlists.{name} is a model, not {other:?}"),
    };
    call(&ui, "Playlists", "create", &[text("Mix")]);
    assert_eq!(global(&ui, "Playlists", "error"), text(""));
    assert_eq!(global(&ui, "Playlists", "shown"), num(0.0));
    assert_eq!(count(&ui, "tracks"), 0);
    call(&ui, "Playlists", "create", &[text("mix")]);
    assert_eq!(
        global(&ui, "Playlists", "error"),
        text("There is already a playlist called mix")
    );
    let queued = titles(&ui).len();
    call(&ui, "Playlists", "save-queue", &[text("Evening")]);
    assert_eq!(
        global(&ui, "Playlists", "shown"),
        num(0.0),
        "Evening sorts first"
    );
    assert_eq!(count(&ui, "tracks"), queued);
    assert_eq!(count(&ui, "playlists"), 2);

    // The library shows its artists; the first one's tracks go into Mix.
    let artist_tracks = library_titles(&ui).len();
    assert!(artist_tracks > 0);
    call(&ui, "Playlists", "add-library-row", &[num(1.0), num(0.0)]);
    call(&ui, "Playlists", "show", &[num(1.0)]);
    let mix = count(&ui, "tracks");
    assert!(mix > 0);
    call(&ui, "Playlists", "play", &[num(0.0)]);
    assert_eq!(titles(&ui).len(), mix, "Up Next becomes the playlist");
    call(&ui, "Playlists", "remove", &[num(0.0)]);
    assert_eq!(count(&ui, "tracks"), mix - 1);
    call(&ui, "Playlists", "rename", &[text("Morning")]);
    assert_eq!(global(&ui, "Playlists", "shown"), num(1.0));
    call(&ui, "Playlists", "delete", &[]);
    assert_eq!(global(&ui, "Playlists", "shown"), num(-1.0));
    assert_eq!(count(&ui, "playlists"), 1);
    call(&ui, "Player", "stop", &[]);

    call(&ui, "Library", "edit-folders", &[]);
    call(&ui, "Library", "remove-draft-folder", &[num(0.0)]);
    call(&ui, "Library", "save-folders", &[]);
    wait_for_scan(&app);
    assert!(library_titles(&ui).is_empty());
    assert!(app.borrow().library.folders().is_empty());
    let _ = std::fs::remove_dir_all(&music);

    // Internet radio: a station playlist is fetched, and its station plays live.
    let base = radio_server(std::fs::read(fixture("sine.mp3")).unwrap());
    app.borrow_mut().player.set_paused(true);
    call(
        &ui,
        "Playlist",
        "add-url",
        &[text(&format!("{base}/list.pls"))],
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while global(&ui, "Player", "live") != Value::Bool(true) {
        assert!(Instant::now() < deadline, "the station did not start");
        std::thread::sleep(Duration::from_millis(10));
        app.borrow_mut().tick();
    }
    assert_eq!(
        titles(&ui).last().map(String::as_str),
        Some("Local - Test FM")
    );
    assert_eq!(global(&ui, "Player", "title"), text("Sine Test"));
    assert_eq!(global(&ui, "Player", "artist"), text("Freya"));
    let Value::String(info) = global(&ui, "Player", "format-info") else {
        panic!("format-info is a string");
    };
    assert!(info.starts_with("Test Radio · MP3"), "{info}");

    // An address that does not answer shows an error.
    call(
        &ui,
        "Playlist",
        "add-url",
        &[text("http://127.0.0.1:9/nothing")],
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while global(&ui, "Player", "error") == text("") {
        assert!(Instant::now() < deadline, "no error shown");
        std::thread::sleep(Duration::from_millis(10));
        app.borrow_mut().tick();
    }
}

/// An app on the built-in skin with a silent player, which plays in real time.
fn silent_app() -> Rc<RefCell<App>> {
    freya_skin::render::init_headless().unwrap();
    let ui = SkinWindow::load(&SkinSource::Builtin).unwrap();
    let app = Rc::new(RefCell::new(App::new(
        ui.clone(),
        SkinSource::Builtin,
        Player::silent(48_000),
        Settings::default(),
        WindowEvents::default(),
        app::Collections {
            library: library::LibraryPanel::new(None, None),
            stations: radio::Stations::new(),
            playlists: playlists::Playlists::new(),
        },
    )));
    bindings::connect(&ui, &app);
    app
}

/// Plays Up Next from its first entry until nothing plays, and returns the rows that started,
/// in order.
fn rows_played(app: &Rc<RefCell<App>>, limit: Duration) -> Vec<usize> {
    app.borrow_mut().play_index(0);
    let started = std::time::Instant::now();
    let mut played: Vec<usize> = Vec::new();
    let mut shown = None;
    let mut quiet_since = None;
    while started.elapsed() < limit {
        let mut app = app.borrow_mut();
        app.tick();
        let status = app.player.status();
        let current = status.track.as_ref().and_then(|_| app.playlist.current());
        if let Some(row) = current
            && current != shown
        {
            played.push(row);
        }
        shown = current;
        if status.state == freya_audio::PlaybackState::Stopped {
            let since = *quiet_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() > Duration::from_millis(600) {
                break;
            }
        } else {
            quiet_since = None;
        }
        drop(app);
        std::thread::sleep(Duration::from_millis(20));
    }
    played
}

#[test]
fn duplicate_entries_play_in_order() {
    // Once, the second A was taken for the first, and Up Next went A, B, A, B, ... for ever.
    let app = silent_app();
    app.borrow_mut().add_paths(&[
        fixture("sine.flac"),
        fixture("mono-22k.wav"),
        fixture("sine.flac"),
        fixture("sine.ogg"),
    ]);
    assert_eq!(rows_played(&app, Duration::from_secs(40)), [0, 1, 2, 3]);
}

#[test]
fn an_unplayable_entry_is_skipped_and_hopeless_lists_stop() {
    // The entry queued next fails to open: playback goes on with the one after it.
    let app = silent_app();
    let missing = temp_dir("unplayable").join("missing.flac");
    app.borrow_mut()
        .add_paths(&[fixture("sine.flac"), missing.clone(), fixture("sine.ogg")]);
    assert_eq!(rows_played(&app, Duration::from_secs(40)), [0, 2]);

    // Nothing can be played, repeat on: Freya tries each once, then stops.
    let app = silent_app();
    app.borrow_mut().settings.repeat = true;
    app.borrow_mut().add_paths(&[missing.clone(), missing]);
    let started = std::time::Instant::now();
    assert_eq!(
        rows_played(&app, Duration::from_secs(10)),
        Vec::<usize>::new()
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "it stopped trying"
    );
}

#[test]
fn a_station_list_is_played_again_without_adding_it_again() {
    let app = silent_app();
    let base = radio_server(std::fs::read(fixture("sine.mp3")).unwrap());
    let list = format!("{base}/list.pls");
    app.borrow_mut().stations.add("Test", &list).unwrap();
    let index = app
        .borrow()
        .stations
        .iter()
        .position(|s| s.url == list)
        .unwrap();
    let wait_until = |what: &str, done: &dyn Fn(&App) -> bool| {
        let started = std::time::Instant::now();
        loop {
            app.borrow_mut().tick();
            if done(&app.borrow()) {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "{what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    for _ in 0..2 {
        app.borrow_mut().play_station(index);
        wait_until("the station plays", &|app| {
            app.player.status().state == freya_audio::PlaybackState::Playing
        });
        // The station is the one shown playing, though Up Next has its stream's address.
        assert_eq!(app.borrow().playing_station(), Some(list.clone()));
    }
    // Time for a second download of the list to come back, were there one.
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_millis(500) {
        app.borrow_mut().tick();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(app.borrow().playlist.len(), 1);
    app.borrow_mut().stop();
}

#[test]
fn relative_paths_are_kept_from_the_root() {
    // `freya song.mp3` saved "song.mp3", which a later start from another folder could not
    // find. Tests run in the crate's folder.
    let app = silent_app();
    let relative = PathBuf::from("../crates/freya-audio/tests/fixtures/sine.wav");
    app.borrow_mut().add_paths(std::slice::from_ref(&relative));
    let app = app.borrow();
    let path = &app.playlist.entries()[0].path;
    assert!(path.is_absolute(), "{}", path.display());
    assert_eq!(
        path.canonicalize().unwrap(),
        relative.canonicalize().unwrap()
    );
}

#[test]
fn a_file_opened_from_the_system_is_added_to_up_next() {
    // MPRIS OpenUri: the address was not decoded, so a name with a space was not found, and
    // Up Next was emptied all the same.
    let app = silent_app();
    app.borrow_mut()
        .add_paths(&[fixture("sine.flac"), fixture("sine.ogg")]);
    let file = temp_dir("open-uri").join("with space.wav");
    std::fs::copy(fixture("sine.wav"), &file).unwrap();
    let path = file
        .to_string_lossy()
        .replace(' ', "%20")
        .replace('\\', "/");
    // "file:///C:/..." on Windows.
    let uri = format!(
        "file://{}{path}",
        if path.starts_with('/') { "" } else { "/" }
    );
    app.borrow_mut()
        .handle_media_request(system_media::MediaRequest::OpenUri(uri));
    let app = app.borrow();
    let paths: Vec<&Path> = app
        .playlist
        .entries()
        .iter()
        .map(|e| e.path.as_path())
        .collect();
    assert_eq!(paths.len(), 3, "{paths:?}");
    assert_eq!(paths[2], file);
    assert_eq!(app.playlist.current(), Some(2));
}

#[test]
fn a_cleared_up_next_shows_nothing_of_what_played() {
    // The album of the track played last stayed, under an empty title.
    let app = silent_app();
    let ui = current_ui(&app);
    app.borrow_mut().add_paths(&[fixture("sine.ape")]);
    app.borrow_mut().play_index(0);
    let started = Instant::now();
    while global(&ui, "Player", "album") != text("Fixtures") {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the track plays"
        );
        app.borrow_mut().tick();
        std::thread::sleep(Duration::from_millis(20));
    }
    app.borrow_mut().stop();
    call(&ui, "Playlist", "clear", &[]);
    let started = Instant::now();
    while app.borrow().player.status().state != freya_audio::PlaybackState::Stopped {
        assert!(started.elapsed() < Duration::from_secs(5), "it stops");
        std::thread::sleep(Duration::from_millis(10));
    }
    app.borrow_mut().tick();
    assert_eq!(global(&ui, "Player", "title"), text(""));
    assert_eq!(global(&ui, "Player", "album"), text(""));
}

#[test]
fn a_cue_sheet_opened_on_its_own_adds_its_tracks() {
    // The open dialog offers sheets now; one opened alone is the disc it cuts.
    let app = silent_app();
    let dir = temp_dir("open-cue");
    std::fs::copy(fixture("sine.wav"), dir.join("disc.wav")).unwrap();
    let sheet = dir.join("disc.cue");
    std::fs::write(
        &sheet,
        "FILE \"disc.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n  \
         TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:00:30\n",
    )
    .unwrap();
    app.borrow_mut().add_paths(&[sheet]);
    let app = app.borrow();
    let titles: Vec<Option<&str>> = app
        .playlist
        .entries()
        .iter()
        .map(|e| e.title.as_deref())
        .collect();
    assert_eq!(titles, [Some("One"), Some("Two")]);
}

/// A global's list of strings.
fn string_list(ui: &SkinWindow, global_name: &str, name: &str) -> Vec<String> {
    let Value::Model(model) = global(ui, global_name, name) else {
        panic!("{global_name}.{name} is a model");
    };
    model
        .iter()
        .map(|v| match v {
            Value::String(s) => s.to_string(),
            other => panic!("not a string: {other:?}"),
        })
        .collect()
}

#[test]
fn library_folders_change_only_when_saved() {
    let app = silent_app();
    let ui = current_ui(&app);
    let music = temp_dir("folders-draft-music");
    let (a, b) = (music.join("a"), music.join("b"));
    for dir in [&a, &b] {
        std::fs::create_dir_all(dir).unwrap();
    }
    app.borrow_mut()
        .library
        .set_folders(std::slice::from_ref(&a));
    app.borrow_mut().refresh();

    // Edited and cancelled: nothing changes, and the next edit starts from what is in use.
    call(&ui, "Library", "edit-folders", &[]);
    app.borrow_mut().add_draft_folder(b.clone());
    call(&ui, "Library", "remove-draft-folder", &[num(0.0)]);
    app.borrow_mut().refresh();
    assert_eq!(string_list(&ui, "Library", "folders-draft").len(), 1);
    assert_eq!(app.borrow().library.folders(), std::slice::from_ref(&a));
    call(&ui, "Library", "edit-folders", &[]);
    app.borrow_mut().refresh();
    assert_eq!(
        app.borrow().folders_draft.as_deref(),
        Some(std::slice::from_ref(&a))
    );

    // A folder inside one already there adds nothing; saved, the edit is in use.
    app.borrow_mut().add_draft_folder(a.join("inside"));
    app.borrow_mut().add_draft_folder(b.clone());
    call(&ui, "Library", "save-folders", &[]);
    app.borrow_mut().refresh();
    assert_eq!(app.borrow().library.folders(), [a, b]);
    assert_eq!(string_list(&ui, "Library", "folders").len(), 2);
    assert!(app.borrow().folders_draft.is_none());
}

#[test]
fn album_sort_and_view_are_kept_in_the_settings() {
    let app = silent_app();
    let ui = current_ui(&app);
    call(&ui, "Library", "set-album-view", &[text("list")]);
    call(&ui, "Library", "set-album-sort", &[text("name")]);
    // Values the API does not know change nothing.
    call(&ui, "Library", "set-album-view", &[text("carousel")]);
    call(&ui, "Library", "set-album-sort", &[text("year")]);
    app.borrow_mut().refresh();
    assert_eq!(app.borrow().settings.album_view, "list");
    assert_eq!(app.borrow().settings.album_sort, "name");
    assert_eq!(global(&ui, "Library", "album-view"), text("list"));
    assert_eq!(global(&ui, "Library", "album-sort"), text("name"));
}

#[test]
fn back_and_forward_retrace_the_views_and_their_scroll() {
    let app = silent_app();
    let ui = current_ui(&app);
    let go = |view: &str, scrolled_to: f64| {
        // Scrolled where the skin says, then somewhere else.
        ui.set_global_property("App", "scroll", num(scrolled_to))
            .unwrap();
        ui.set_global_property("App", "view", text(view)).unwrap();
        call(&ui, "App", "navigated", &[]);
    };
    call(&ui, "App", "navigated", &[]);
    assert_eq!(global(&ui, "App", "view"), text("queue"));
    assert_eq!(global(&ui, "App", "can-go-back"), Value::Bool(false));
    go("radio", 300.0);
    go("settings", 120.0);
    assert_eq!(global(&ui, "App", "can-go-back"), Value::Bool(true));

    ui.set_global_property("App", "scroll", num(40.0)).unwrap();
    call(&ui, "App", "go-back", &[]);
    assert_eq!(global(&ui, "App", "view"), text("radio"));
    assert_eq!(global(&ui, "App", "restore-view"), text("radio"));
    assert_eq!(global(&ui, "App", "restore-scroll"), num(120.0));
    call(&ui, "App", "go-back", &[]);
    assert_eq!(global(&ui, "App", "view"), text("queue"));
    assert_eq!(global(&ui, "App", "restore-scroll"), num(300.0));
    assert_eq!(global(&ui, "App", "can-go-back"), Value::Bool(false));
    assert_eq!(global(&ui, "App", "can-go-forward"), Value::Bool(true));

    call(&ui, "App", "go-forward", &[]);
    call(&ui, "App", "go-forward", &[]);
    assert_eq!(global(&ui, "App", "view"), text("settings"));
    assert_eq!(global(&ui, "App", "restore-scroll"), num(40.0));
    assert_eq!(global(&ui, "App", "can-go-forward"), Value::Bool(false));

    // Going somewhere new after going back leaves nothing ahead, and starts it at the top.
    call(&ui, "App", "go-back", &[]);
    go("about", 0.0);
    assert_eq!(global(&ui, "App", "can-go-forward"), Value::Bool(false));
    assert_eq!(global(&ui, "App", "restore-view"), text(""));
}

#[test]
fn back_reopens_a_playlist_that_was_shown() {
    let app = silent_app();
    let ui = current_ui(&app);
    call(&ui, "App", "navigated", &[]);
    call(&ui, "Playlists", "create", &[text("First")]);
    call(&ui, "Playlists", "create", &[text("Second")]);
    for index in [0.0, 1.0] {
        call(&ui, "Playlists", "show", &[num(index)]);
        ui.set_global_property("App", "view", text("playlist"))
            .unwrap();
        call(&ui, "App", "navigated", &[]);
    }
    assert_eq!(global(&ui, "Playlists", "shown"), num(1.0));
    call(&ui, "App", "go-back", &[]);
    assert_eq!(global(&ui, "App", "view"), text("playlist"));
    assert_eq!(global(&ui, "Playlists", "shown"), num(0.0));
}

#[test]
fn artist_order_view_and_pictures_are_kept_in_the_settings() {
    let app = silent_app();
    let ui = current_ui(&app);
    assert_eq!(global(&ui, "Library", "artist-view"), text("grid"));
    call(&ui, "Library", "set-artist-sort", &[text("most-played")]);
    call(&ui, "Library", "set-artist-view", &[text("list")]);
    call(
        &ui,
        "Library",
        "set-artist-pictures-online",
        &[Value::Bool(false)],
    );
    // Orders the API does not know change nothing.
    call(&ui, "Library", "set-artist-sort", &[text("loudest")]);
    app.borrow_mut().refresh();
    let settings = &app.borrow().settings;
    assert_eq!(settings.artist_sort, "most-played");
    assert_eq!(settings.artist_view, "list");
    assert_eq!(global(&ui, "Library", "artist-sort"), text("most-played"));
    assert_eq!(
        global(&ui, "Library", "artist-pictures-online"),
        Value::Bool(false)
    );
}
