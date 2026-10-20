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
use freya_server::take_in::{Folders, take_in};
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
    let freya = directories::ProjectDirs::from("", "", "Freya");
    let Some(data) = data.or_else(|| freya.as_ref().map(|dirs| dirs.data_dir().join("server")))
    else {
        eprintln!("freya-server: no folder to keep the library in");
        std::process::exit(1);
    };
    if let Err(e) = std::fs::create_dir_all(&data) {
        eprintln!("freya-server: could not make {}: {e}", data.display());
        std::process::exit(1);
    }
    start_log(&data.join("server.log"));
    if let Err(e) = run(&address, &data, freya.as_ref()) {
        log::error!("{e}");
        eprintln!("freya-server: {e}");
        std::process::exit(1);
    }
}

fn run(address: &str, data: &Path, freya: Option<&directories::ProjectDirs>) -> Result<(), String> {
    let settings_file = data.join("settings.toml");
    let scrobbles = data.join("lastfm");
    if let Some(freya) = freya {
        take_in(&Folders {
            freya_data: freya.data_dir(),
            freya_settings: freya.config_dir(),
            data,
            settings_file: &settings_file,
            scrobbles: &scrobbles,
        });
    }
    let (settings, damaged) = settings::load(&settings_file);
    let config = ServiceConfig {
        settings,
        settings_file: Some(settings_file),
        scans_allowed: !damaged,
        lastfm: freya_server::lastfm(),
        playlists: Some(data.join("playlists")),
        scrobbles: Some(scrobbles),
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
