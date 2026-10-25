//! Sample state for previews and screenshot tests.

use slint::{Model, ModelRc, VecModel};
use slint_interpreter::{ComponentInstance, SharedString, Struct, Value};

fn text(s: &str) -> Value {
    Value::String(SharedString::from(s))
}

/// Fills the skin API with a track that is playing and a short playlist.
pub fn fill(ui: &ComponentInstance) {
    let set = |global: &str, name: &str, value: Value| {
        // Skins may leave out optional globals.
        let _ = ui.set_global_property(global, name, value);
    };
    set("Player", "title", text("Avril 14th"));
    set("Player", "artist", text("Aphex Twin"));
    set(
        "Player",
        "format-info",
        text("FLAC · 912 kbps · 44.1 kHz · Stereo"),
    );
    set(
        "Player",
        "state",
        Value::EnumerationValue("PlaybackState".into(), "playing".into()),
    );
    set("Player", "position", Value::Number(83.0));
    set("Player", "length", Value::Number(125.0));
    set("Player", "position-text", text("1:23"));
    set("Player", "remaining-text", text("-0:42"));
    set("Player", "volume", Value::Number(0.7));
    // Connected to a server: without one, the skin shows only its welcome view.
    set("Server", "chosen", Value::Bool(true));
    set("Server", "state", text("connected"));
    set("Server", "name", text("Mini"));
    set("Server", "user", text("alex"));
    set("Server", "user-initial", text("A"));

    let tracks = [
        ("Kraftwerk - Computer Love", "7:15"),
        ("Daft Punk - Something About Us", "3:52"),
        ("Aphex Twin - Avril 14th", "2:05"),
        ("Röyksopp - Eple", "3:40"),
        ("Boards of Canada - Roygbiv", "2:31"),
        ("Massive Attack - Teardrop", "5:30"),
        ("Portishead - Glory Box", "5:06"),
        (
            "Lykke Li - I Follow Rivers (The Magician Remix, an unusually long title)",
            "4:47",
        ),
        ("untagged file", ""),
    ];
    let rows: Vec<Value> = tracks
        .iter()
        .enumerate()
        .map(|(i, (title, duration))| {
            Value::Struct(Struct::from_iter([
                ("number".to_owned(), Value::Number(i as f64 + 1.0)),
                ("title".to_owned(), text(title)),
                ("duration".to_owned(), text(duration)),
                ("selected".to_owned(), Value::Bool(i == 4 || i == 5)),
                ("current".to_owned(), Value::Bool(i == 2)),
            ]))
        })
        .collect();
    set(
        "Playlist",
        "rows",
        Value::Model(ModelRc::new(VecModel::from(rows))),
    );
    set("Playlist", "summary", text("9 tracks · 34:38+"));

    // Winamp's "Rock" preset.
    let bands = [4.8, 2.9, -3.8, -5.3, -2.5, 2.5, 5.5, 6.6, 6.6, 6.6];
    let numbers = |values: &[f64]| {
        Value::Model(ModelRc::new(VecModel::from(
            values.iter().map(|&v| Value::Number(v)).collect::<Vec<_>>(),
        )))
    };
    let strings = |values: &[&str]| {
        Value::Model(ModelRc::new(VecModel::from(
            values.iter().map(|v| text(v)).collect::<Vec<_>>(),
        )))
    };
    set("Equalizer", "enabled", Value::Bool(true));
    set("Equalizer", "bands", numbers(&bands));
    set(
        "Equalizer",
        "band-labels",
        strings(&[
            "60", "170", "310", "600", "1k", "3k", "6k", "12k", "14k", "16k",
        ]),
    );
    set(
        "Equalizer",
        "presets",
        strings(&["Flat", "Classical", "Rock"]),
    );
    set("Equalizer", "preset-name", text("Rock"));
    // The app draws a spline; a polyline through the band points will do for previews.
    let _ = ui.set_global_callback("Equalizer", "curve", move |args| {
        let size = |i: usize| match args.get(i) {
            Some(Value::Number(n)) => *n,
            _ => 0.0,
        };
        let (w, h) = (size(0), size(1));
        let path: String = bands
            .iter()
            .enumerate()
            .map(|(i, db)| {
                let cmd = if i == 0 { 'M' } else { 'L' };
                format!(
                    "{cmd}{:.1} {:.1} ",
                    i as f64 * w / 9.0,
                    (0.5 - db / 12.0 * 0.45) * h
                )
            })
            .collect();
        text(path.trim_end())
    });
    set("App", "equalizer-visible", Value::Bool(true));

    // A spectrum that falls off towards the treble, with peaks a little above the bars.
    let bars: Vec<f64> = (0..24)
        .map(|i| {
            let x = f64::from(i) / 23.0;
            (0.85 - 0.6 * x + 0.12 * (x * 19.0).sin()).clamp(0.05, 1.0)
        })
        .collect();
    let peaks: Vec<f64> = bars.iter().map(|b| (b + 0.08).min(1.0)).collect();
    let _ = ui.set_global_callback("Visualizer", "bars-path", |args| {
        text(&crate::paths::bars_path(
            arg_number(args, 0),
            arg_number(args, 1),
            &arg_numbers(args, 2),
        ))
    });
    let _ = ui.set_global_callback("Visualizer", "peaks-path", |args| {
        text(&crate::paths::peaks_path(
            arg_number(args, 0),
            arg_number(args, 1),
            &arg_numbers(args, 2),
            arg_number(args, 3),
        ))
    });
    set("Visualizer", "bar-count", Value::Number(24.0));
    set("Visualizer", "bars", numbers(&bars));
    set("Visualizer", "peaks", numbers(&peaks));
    set("Playlist", "cursor", Value::Number(5.0));
    set("Playlist", "repeat", Value::Bool(true));

    // The library, open at an album.
    let library_row = |kind: &str, title: &str, detail: &str, number: &str, duration: &str| {
        Value::Struct(Struct::from_iter([
            ("kind".to_owned(), text(kind)),
            ("title".to_owned(), text(title)),
            ("detail".to_owned(), text(detail)),
            ("number".to_owned(), text(number)),
            ("duration".to_owned(), text(duration)),
        ]))
    };
    let album = [
        ("1", "Wildlife Analysis", "", "1:17"),
        ("2", "An Eagle in Your Mind", "", "6:23"),
        ("3", "The Color of the Fire", "", "1:45"),
        ("4", "Telephasic Workshop", "", "6:35"),
        ("5", "Triangles & Rhombuses", "", "1:50"),
        ("6", "Sixtyten", "", "5:48"),
        ("7", "Turquoise Hexagon Sun", "", "5:07"),
        ("8", "Kaini Industries", "", "1:00"),
        ("9", "Bocuma", "", "1:35"),
        ("10", "Roygbiv", "", "2:31"),
        ("11", "Rue the Whirl", "", "6:39"),
        ("12", "Aquarius", "Boards of Canada feat. a guest", "5:58"),
    ];
    let rows: Vec<Value> = album
        .iter()
        .map(|(n, title, detail, duration)| library_row("track", title, detail, n, duration))
        .collect();
    set(
        "Library",
        "rows",
        Value::Model(ModelRc::new(VecModel::from(rows))),
    );
    set("Library", "title", text("Music Has the Right to Children"));
    set(
        "Library",
        "subtitle",
        text("Boards of Canada · 1998 · 18 tracks · 1:10:52"),
    );
    set("Library", "can-go-back", Value::Bool(true));
    set("Library", "folders", strings(&["~/Music"]));
    set("Library", "status", text("10,412 tracks · 682 albums"));

    // Radio stations, one of them playing.
    let station = |name: &str, address: &str, playing: bool| {
        Value::Struct(Struct::from_iter([
            ("name".to_owned(), text(name)),
            ("address".to_owned(), text(address)),
            ("playing".to_owned(), Value::Bool(playing)),
        ]))
    };
    let stations = vec![
        station("SomaFM: Groove Salad", "somafm.com/groovesalad.pls", true),
        station("SomaFM: Drone Zone", "somafm.com/dronezone.pls", false),
        station(
            "Radio Paradise",
            "www.radioparadise.com/m3u/mp3-128.m3u",
            false,
        ),
        station("FIP", "icecast.radiofrance.fr/fip-midfi.mp3", false),
        station("KEXP", "kexp.streamguys1.com/kexp160.aac", false),
    ];
    set(
        "Radio",
        "stations",
        Value::Model(ModelRc::new(VecModel::from(stations))),
    );

    // Playlists in the navigation; the second one is shown.
    let playlist = |name: &str, count: &str| {
        Value::Struct(Struct::from_iter([
            ("name".to_owned(), text(name)),
            ("count".to_owned(), text(count)),
        ]))
    };
    let playlists = vec![
        playlist("Calm evenings", "24"),
        playlist("Road trip", "6"),
        playlist("Workout", "18"),
    ];
    set(
        "Playlists",
        "playlists",
        Value::Model(ModelRc::new(VecModel::from(playlists))),
    );
    set("Playlists", "shown", Value::Number(1.0));
    set("Playlists", "summary", text("6 tracks · 27:04"));
    let road: Vec<Value> = [
        ("Daft Punk - Something About Us", "3:52"),
        ("Röyksopp - Eple", "3:40"),
        ("Air - La femme d'argent", "7:11"),
        ("Portishead - Glory Box", "5:06"),
        ("Kraftwerk - Computer Love", "7:15"),
        ("A stream", ""),
    ]
    .iter()
    .enumerate()
    .map(|(i, (title, duration))| {
        Value::Struct(Struct::from_iter([
            ("number".to_owned(), Value::Number(i as f64 + 1.0)),
            ("title".to_owned(), text(title)),
            ("duration".to_owned(), text(duration)),
            ("selected".to_owned(), Value::Bool(false)),
            ("current".to_owned(), Value::Bool(false)),
        ]))
    })
    .collect();
    set(
        "Playlists",
        "tracks",
        Value::Model(ModelRc::new(VecModel::from(road))),
    );
}

/// No server chosen yet, and none on this computer: the welcome view.
pub fn fill_welcome(ui: &ComponentInstance) {
    let _ = ui.set_global_property("Server", "chosen", Value::Bool(false));
    let _ = ui.set_global_property("Server", "here", Value::Bool(false));
}

/// Internet radio: a station with a song, no length.
pub fn fill_radio(ui: &ComponentInstance) {
    let set = |name: &str, value: Value| {
        let _ = ui.set_global_property("Player", name, value);
    };
    set("title", text("Teleseme (Hallo Halo Remix)"));
    set("artist", text("Steve Cobby"));
    set(
        "format-info",
        text("Groove Salad [SomaFM] · MP3 · 128 kbps · 44.1 kHz · Stereo"),
    );
    set("length", Value::Number(0.0));
    set("position-text", text("12:07"));
    set("live", Value::Bool(true));
}

/// The radio directory's first page: its tags and rows.
pub fn fill_directory(ui: &ComponentInstance) {
    let set = |name: &str, value: Value| {
        let _ = ui.set_global_property("Radio", name, value);
    };
    let picked = [
        (
            "Radio Paradise (FLAC)",
            "Eclectic · Rock · United States · English",
            "Lossless",
            true,
        ),
        (
            "FIP",
            "Eclectic · Jazz · Public · France · French",
            "192 kbps",
            false,
        ),
        (
            "KEXP",
            "Indie · Alternative · Eclectic · Community · United States",
            "160 kbps",
            false,
        ),
        (
            "SomaFM: Groove Salad",
            "Chillout · Ambient · Electronic · United States",
            "128 kbps",
            false,
        ),
        (
            "NTS Radio 1",
            "Eclectic · Electronic · United Kingdom · English",
            "256 kbps",
            false,
        ),
        (
            "Sveriges Radio P2 (FLAC)",
            "Classical · Jazz · Folk · Public · Sweden",
            "Hi-Res",
            false,
        ),
        (
            "Worldwide FM",
            "World · Jazz · Electronic · United Kingdom",
            "320 kbps",
            false,
        ),
    ];
    let jazz = [
        ("FIP Jazz", "Jazz · Public · France", "192 kbps"),
        ("Naim Jazz (FLAC)", "Jazz · United Kingdom", "Lossless"),
        (
            "Český rozhlas Jazz (FLAC)",
            "Jazz · Public · Czechia · Czech",
            "Lossless",
        ),
        (
            "Jazz24",
            "Jazz · Public · United States · English",
            "256 kbps",
        ),
        ("TSF Jazz", "Jazz · France · French", "128 kbps"),
        ("DR P8 Jazz", "Jazz · Public · Denmark · Danish", "128 kbps"),
        ("NRK Jazz", "Jazz · Public · Norway · Norwegian", "192 kbps"),
    ];
    let row = |(name, detail, quality): (&str, &str, &str), playing: bool, saved: bool| {
        Value::Struct(Struct::from_iter([
            ("name".to_owned(), text(name)),
            ("detail".to_owned(), text(detail)),
            ("quality".to_owned(), text(quality)),
            ("saved".to_owned(), Value::Bool(saved)),
            ("playing".to_owned(), Value::Bool(playing)),
            ("featured".to_owned(), Value::Bool(true)),
            ("logo".to_owned(), Value::Image(Default::default())),
        ]))
    };
    let mut found: Vec<Value> = picked
        .iter()
        .enumerate()
        .map(|(i, &(name, detail, quality, saved))| row((name, detail, quality), i == 3, saved))
        .collect();
    // Two stations' logos: a square one and a wide one.
    for (at, logo) in [(1, sample_logo(120, 120)), (2, sample_logo(240, 80))] {
        if let Value::Struct(mut station) = found[at].clone() {
            station.set_field("logo".into(), Value::Image(logo));
            found[at] = Value::Struct(station);
        }
    }
    found.extend(jazz.iter().map(|&station| row(station, false, false)));
    set("found", model(found));
    let shelf = |title: &str, first: usize, count: usize, tagged: bool| {
        Value::Struct(Struct::from_iter([
            ("title".to_owned(), text(title)),
            ("first".to_owned(), Value::Number(first as f64)),
            ("count".to_owned(), Value::Number(count as f64)),
            ("tagged".to_owned(), Value::Bool(tagged)),
        ]))
    };
    set(
        "shelves",
        model(vec![
            shelf("Picked by Freya", 0, picked.len(), false),
            shelf("Jazz", picked.len(), jazz.len(), true),
        ]),
    );
    set("tags", tag_groups(false));
    set("total", Value::Number(-1.0));
    set("directory", text("ready"));
    set("summary", text("45,012 stations"));
}

/// The radio directory searched by tags: jazz from Italy.
pub fn fill_directory_search(ui: &ComponentInstance) {
    let set = |name: &str, value: Value| {
        let _ = ui.set_global_property("Radio", name, value);
    };
    let found: Vec<Value> = [
        (
            "Rai Radio 3",
            "Classical · Jazz · Public · Italy · Italian",
            "320 kbps",
            true,
        ),
        (
            "Musica Jazz Radio",
            "Blues · Jazz · Italy · Italian",
            "128 kbps",
            false,
        ),
        (
            "RMC Nights Story",
            "Jazz · World · Italy · Italian",
            "128 kbps",
            false,
        ),
        (
            "RMC Marine",
            "Chillout · Jazz · Lounge · Italy · Italian",
            "",
            false,
        ),
        (
            "Radio Dimensione Relax",
            "Ambient · Chillout · Jazz · Italy · Italian",
            "256 kbps",
            false,
        ),
        (
            "Easy Network Jazz & Soul",
            "Jazz · Soul · Italy · Italian",
            "128 kbps",
            false,
        ),
    ]
    .iter()
    .enumerate()
    .map(|(i, &(name, detail, quality, saved))| {
        Value::Struct(Struct::from_iter([
            ("name".to_owned(), text(name)),
            ("detail".to_owned(), text(detail)),
            ("quality".to_owned(), text(quality)),
            ("saved".to_owned(), Value::Bool(saved)),
            ("playing".to_owned(), Value::Bool(i == 1)),
            ("featured".to_owned(), Value::Bool(i == 0)),
            ("logo".to_owned(), Value::Image(Default::default())),
        ]))
    })
    .collect();
    set("found", model(found));
    set("shelves", model(Vec::new()));
    set("tags", tag_groups(true));
    set(
        "chosen",
        model(vec![
            tag("genre:jazz", "Jazz", "genre", 0, true),
            tag("country:it", "Italy", "country", 0, true),
        ]),
    );
    set("total", Value::Number(31.0));
    // An admin, who saves stations.
    let _ = ui.set_global_property("Server", "admin", Value::Bool(true));
    set("query", text(""));
    set("directory", text("ready"));
    set("summary", text("45,012 stations"));
}

/// A logo to show: a two-colored picture of the given size.
fn sample_logo(width: u32, height: u32) -> slint::Image {
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
    for (i, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
        let x = i as u32 % width;
        *pixel = if x < width / 2 {
            slint::Rgba8Pixel::new(220, 40, 60, 255)
        } else {
            slint::Rgba8Pixel::new(30, 30, 30, 255)
        };
    }
    slint::Image::from_rgba8(buffer)
}

fn model(rows: Vec<Value>) -> Value {
    Value::Model(ModelRc::new(VecModel::from(rows)))
}

fn tag(id: &str, name: &str, kind: &str, stations: u32, chosen: bool) -> Value {
    Value::Struct(Struct::from_iter([
        ("id".to_owned(), text(id)),
        ("name".to_owned(), text(name)),
        ("kind".to_owned(), text(kind)),
        ("stations".to_owned(), Value::Number(f64::from(stations))),
        ("chosen".to_owned(), Value::Bool(chosen)),
    ]))
}

/// The directory's tags by kind, as on its first page or of the stations found.
fn tag_groups(found: bool) -> Value {
    let group = |kind: &str, title: &str, names: &[&str]| {
        let tags: Vec<Value> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let id = format!("{kind}:{}", name.to_lowercase());
                tag(&id, name, kind, 1000 / (i as u32 + 1), false)
            })
            .collect();
        Value::Struct(Struct::from_iter([
            ("kind".to_owned(), text(kind)),
            ("title".to_owned(), text(title)),
            ("tags".to_owned(), model(tags)),
        ]))
    };
    let groups = if found {
        vec![
            group("quality", "Quality", &["High bitrate"]),
            group(
                "genre",
                "Genres",
                &[
                    "Soul", "Lounge", "Chillout", "Blues", "Pop", "World", "Funk",
                ],
            ),
            group("language", "Languages", &["Italian", "English"]),
        ]
    } else {
        vec![
            group(
                "quality",
                "Quality",
                &["High bitrate", "Lossless", "Hi-Res"],
            ),
            group(
                "genre",
                "Genres",
                &[
                    "Pop",
                    "Rock",
                    "Oldies",
                    "Dance",
                    "Latin",
                    "Electronic",
                    "Jazz",
                    "Classical",
                    "Eclectic",
                    "Alternative",
                    "House",
                    "Chillout",
                    "Folk",
                    "Hip-hop",
                ],
            ),
            group(
                "country",
                "Countries",
                &[
                    "United States",
                    "Germany",
                    "France",
                    "Russia",
                    "Greece",
                    "United Kingdom",
                    "Mexico",
                    "Italy",
                    "Brazil",
                    "Australia",
                    "China",
                    "Spain",
                ],
            ),
            group(
                "language",
                "Languages",
                &[
                    "English", "Spanish", "German", "French", "Russian", "Chinese", "Italian",
                    "Greek", "Dutch",
                ],
            ),
            group(
                "era",
                "Decades",
                &["80s", "90s", "70s", "2000s", "60s", "2010s", "50s"],
            ),
            group(
                "format",
                "Formats",
                &[
                    "News",
                    "Talk",
                    "Religious",
                    "Community",
                    "Public",
                    "Sports",
                    "Kids",
                ],
            ),
        ]
    };
    model(groups)
}

/// A track that could not be played.
pub fn fill_error(ui: &ComponentInstance) {
    let set = |name: &str, value: Value| {
        let _ = ui.set_global_property("Player", name, value);
    };
    set(
        "state",
        Value::EnumerationValue("PlaybackState".into(), "stopped".into()),
    );
    set("title", text("radio.example.com"));
    set("artist", text(""));
    set(
        "error",
        text("cannot open http://radio.example.com/live: Connection refused"),
    );
}

/// Shows the library's list of artists instead of an album, with a search typed.
pub fn fill_library_search(ui: &ComponentInstance) {
    fill(ui);
    let row = |kind: &str, title: &str, detail: &str| {
        Value::Struct(Struct::from_iter([
            ("kind".to_owned(), text(kind)),
            ("title".to_owned(), text(title)),
            ("detail".to_owned(), text(detail)),
            ("number".to_owned(), text("")),
            ("duration".to_owned(), text("")),
        ]))
    };
    let rows = vec![
        row("artist", "Aphex Twin", "6 albums · 84 tracks"),
        row("artist", "Björk", "9 albums · 112 tracks"),
        row("artist", "Boards of Canada", "4 albums · 61 tracks"),
        row("album", "Debut", "1993 · 11 tracks"),
        row("track", "Hyperballad", "Björk · Post"),
    ];
    let _ = ui.set_global_property(
        "Library",
        "rows",
        Value::Model(ModelRc::new(VecModel::from(rows))),
    );
    let _ = ui.set_global_property("Library", "title", text("Artists"));
    let _ = ui.set_global_property("Library", "subtitle", text("214 artists"));
    let _ = ui.set_global_property("Library", "can-go-back", Value::Bool(false));
    let _ = ui.set_global_property("Library", "query", text("bj"));
}

/// A `LibraryRow`.
fn library_row(
    kind: &str,
    title: &str,
    detail: &str,
    artist: &str,
    album: &str,
    duration: &str,
) -> Value {
    Value::Struct(Struct::from_iter([
        ("kind".to_owned(), text(kind)),
        ("title".to_owned(), text(title)),
        ("detail".to_owned(), text(detail)),
        ("artist".to_owned(), text(artist)),
        ("album".to_owned(), text(album)),
        ("number".to_owned(), text("")),
        ("duration".to_owned(), text(duration)),
    ]))
}

fn show_library(
    ui: &ComponentInstance,
    section: &str,
    layout: &str,
    title: &str,
    subtitle: &str,
    rows: Vec<Value>,
) {
    let set = |name: &str, value: Value| {
        let _ = ui.set_global_property("Library", name, value);
    };
    set("rows", Value::Model(ModelRc::new(VecModel::from(rows))));
    set("section", text(section));
    set("layout", text(layout));
    set("title", text(title));
    set("subtitle", text(subtitle));
    set("can-go-back", Value::Bool(false));
}

/// The library's Albums section: a grid.
pub fn fill_library_albums(ui: &ComponentInstance) {
    fill(ui);
    let albums = [
        ("Arrival", "ABBA", "1976 · 10 tracks"),
        ("Moon Safari", "Air", "1998 · 10 tracks"),
        (
            "Selected Ambient Works 85–92",
            "Aphex Twin",
            "1992 · 13 tracks",
        ),
        ("Post", "Björk", "1995 · 11 tracks"),
        (
            "Music Has the Right to Children",
            "Boards of Canada",
            "1998 · 18 tracks",
        ),
        ("Discovery", "Daft Punk", "2001 · 14 tracks"),
        ("Computer World", "Kraftwerk", "1981 · 7 tracks"),
        ("Mezzanine", "Massive Attack", "1998 · 11 tracks"),
        ("Dummy", "Portishead", "1994 · 11 tracks"),
        ("Melody A.M.", "Röyksopp", "2001 · 10 tracks"),
        ("Unknown album", "Unknown artist", "3 tracks"),
    ];
    let rows = albums
        .iter()
        .map(|(title, artist, detail)| library_row("album", title, detail, artist, "", ""))
        .collect();
    show_library(ui, "albums", "grid", "Albums", "682 albums", rows);
}

/// The library's Songs section: a table.
pub fn fill_library_songs(ui: &ComponentInstance) {
    fill(ui);
    show_library(ui, "songs", "table", "Songs", "10,412 tracks", song_rows());
}

/// The Recents' Most Played: a table with how often each track was played.
pub fn fill_library_most_played(ui: &ComponentInstance) {
    fill(ui);
    let rows = song_rows()
        .into_iter()
        .enumerate()
        .map(|(i, mut row)| {
            if let Value::Struct(row) = &mut row {
                row.set_field("played".to_owned(), text(&format!("{} plays", 48 - 3 * i)));
            }
            row
        })
        .collect();
    show_library(
        ui,
        "most-played",
        "table",
        "Most Played",
        "200 tracks",
        rows,
    );
}

fn song_rows() -> Vec<Value> {
    let songs = [
        ("Army of Me", "Björk", "Post", "3:54"),
        ("Avril 14th", "Aphex Twin", "Drukqs", "2:05"),
        ("Computer Love", "Kraftwerk", "Computer World", "7:15"),
        ("Dancing Queen", "ABBA", "Arrival", "3:51"),
        ("Eple", "Röyksopp", "Melody A.M.", "3:40"),
        ("Glory Box", "Portishead", "Dummy", "5:06"),
        ("Hyperballad", "Björk", "Post", "5:21"),
        (
            "I Follow Rivers (The Magician Remix, an unusually long title)",
            "Lykke Li",
            "Wounded Rhymes",
            "4:47",
        ),
        ("La femme d'argent", "Air", "Moon Safari", "7:11"),
        (
            "Roygbiv",
            "Boards of Canada",
            "Music Has the Right to Children",
            "2:31",
        ),
        ("Something About Us", "Daft Punk", "Discovery", "3:52"),
        ("Teardrop", "Massive Attack", "Mezzanine", "5:30"),
    ];
    songs
        .iter()
        .map(|(title, artist, album, time)| {
            library_row(
                "track",
                title,
                &format!("{artist} · {album}"),
                artist,
                album,
                time,
            )
        })
        .collect()
}

/// A number argument of a callback.
fn arg_number(args: &[Value], i: usize) -> f32 {
    match args.get(i) {
        Some(Value::Number(n)) => *n as f32,
        _ => 0.0,
    }
}

/// A list-of-numbers argument of a callback.
fn arg_numbers(args: &[Value], i: usize) -> Vec<f32> {
    match args.get(i) {
        Some(Value::Model(model)) => model
            .iter()
            .map(|v| match v {
                Value::Number(n) => n as f32,
                _ => 0.0,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Fills the skin picker with a few skins.
pub fn fill_picker(picker: &ComponentInstance) {
    let entry = |name: &str, details: &str, current: bool| {
        Value::Struct(Struct::from_iter([
            ("name".to_owned(), text(name)),
            ("details".to_owned(), text(details)),
            ("current".to_owned(), Value::Bool(current)),
        ]))
    };
    let skins = vec![
        entry("Freya", "by Freya · 0.1.0 · The built-in skin.", false),
        entry("Mini", "by Freya · 1.0 · A compact one-line player.", true),
        entry(
            "Midnight",
            "by Someone · 2.3 · A dark skin with large text.",
            false,
        ),
    ];
    let _ = picker.set_property("skins", Value::Model(ModelRc::new(VecModel::from(skins))));
    let _ = picker.set_property("folder", text("~/Library/Application Support/Freya/skins"));
    let _ = picker.set_property(
        "error",
        text("Could not use this skin: skin does not compile: skin.slint:3: 'Player' does not have a property 'titel'"),
    );
}
