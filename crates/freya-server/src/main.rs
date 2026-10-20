//! `freya-server`: the library as a service for Freya's clients, on this computer.
//!
//! `freya-server [--address HOST:PORT] [--data FOLDER]`, or `--version`. It keeps its library, pictures,
//! settings and log in its own folder, the first time taking in what Freya kept in its folder
//! before the server was its own. It runs until stopped: Ctrl-C, SIGTERM, or `POST /quit` from
//! this computer.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::channel;

use freya_server::http::Server;
use freya_server::{LOCAL_ADDRESS, Served, Service, ServiceConfig, handler, settings};

fn main() {
    let mut address = LOCAL_ADDRESS.to_owned();
    let mut data = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--version" {
            println!("freya-server {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        match (arg.as_str(), args.next()) {
            ("--address", Some(value)) => address = value,
            ("--data", Some(value)) => data = Some(PathBuf::from(value)),
            _ => {
                eprintln!("usage: freya-server [--address HOST:PORT] [--data FOLDER] | --version");
                std::process::exit(2);
            }
        }
    }
    let freya = directories::ProjectDirs::from("", "", "Freya").map(|d| d.data_dir().to_owned());
    let Some(data) = data.or_else(|| freya.as_ref().map(|dir| dir.join("server"))) else {
        eprintln!("freya-server: no folder to keep the library in");
        std::process::exit(1);
    };
    if let Err(e) = std::fs::create_dir_all(&data) {
        eprintln!("freya-server: could not make {}: {e}", data.display());
        std::process::exit(1);
    }
    start_log(&data.join("server.log"));
    if let Err(e) = run(&address, &data, freya.as_deref()) {
        log::error!("{e}");
        eprintln!("freya-server: {e}");
        std::process::exit(1);
    }
}

fn run(address: &str, data: &Path, freya: Option<&Path>) -> Result<(), String> {
    let settings_file = data.join("settings.toml");
    if let Some(freya) = freya {
        take_in(freya, data, &settings_file);
    }
    let (settings, damaged) = settings::load(&settings_file);
    let config = ServiceConfig {
        settings,
        settings_file: Some(settings_file),
        scans_allowed: !damaged,
        lastfm: freya_server::lastfm(),
    };
    let service = Arc::new(Service::start(
        data.join("library.sqlite3"),
        Some(data.join("pictures")),
        config,
    )?);
    let (stop, stopped) = channel();
    let quit = {
        let stop = stop.clone();
        Arc::new(move || {
            let _ = stop.send(());
        })
    };
    ctrlc::set_handler(move || {
        let _ = stop.send(());
    })
    .map_err(|e| format!("could not listen for being stopped: {e}"))?;
    let served = Served {
        library: service.api(),
        server: Some(Arc::clone(&service) as _),
        quit: Some(quit),
    };
    let server = Server::start(address, handler(served))
        .map_err(|e| format!("could not listen on {address}: {e}"))?;
    log::info!(
        "Freya's server {} serves {} on {}",
        env!("CARGO_PKG_VERSION"),
        data.display(),
        server.address()
    );
    let _ = stopped.recv();
    log::info!("stopping");
    drop(server);
    drop(service);
    Ok(())
}

/// Takes in, the first time, what Freya kept in its own folder `freya` before the server kept
/// it: the library and its pictures are moved into `data`, and the library's settings are
/// read from Freya's.
fn take_in(freya: &Path, data: &Path, settings_file: &Path) {
    if !data.join("library.sqlite3").exists() {
        for name in [
            "library.sqlite3",
            "library.sqlite3-wal",
            "library.sqlite3-shm",
            "pictures",
        ] {
            let (from, to) = (freya.join(name), data.join(name));
            if from.exists() {
                match std::fs::rename(&from, &to) {
                    Ok(()) => log::info!("moved {} to {}", from.display(), to.display()),
                    Err(e) => log::warn!("could not move {}: {e}", from.display()),
                }
            }
        }
    }
    let freya_settings = freya.join("settings.toml");
    if !settings_file.exists()
        && let Ok(text) = std::fs::read_to_string(&freya_settings)
    {
        // The library's settings have the same names there; the rest is the client's.
        match toml::from_str::<freya_protocol::ServerSettings>(&text) {
            Ok(taken) => match settings::save(settings_file, &taken) {
                Ok(()) => log::info!(
                    "took the library's settings from {}",
                    freya_settings.display()
                ),
                Err(e) => log::warn!("could not keep the settings: {e}"),
            },
            Err(e) => log::warn!("could not read the library's settings from Freya's: {e}"),
        }
    }
}

/// Logs to `file`, from this run on, and to the terminal as `RUST_LOG` says.
fn start_log(file: &Path) {
    let env = env_logger::Env::default().default_filter_or("info");
    let mut builder = env_logger::Builder::from_env(env);
    match std::fs::File::create(file) {
        Ok(file) => {
            builder.target(env_logger::Target::Pipe(Box::new(file)));
        }
        Err(e) => eprintln!("freya-server: could not log to {}: {e}", file.display()),
    }
    builder.init();
}
