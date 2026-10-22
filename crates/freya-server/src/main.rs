// A window app in release builds on Windows, so starting at login opens no console; it logs
// to its folder.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! `freya-server`: the library as a service for Freya's clients, on this computer.
//!
//! `freya-server [--address HOST:PORT] [--data FOLDER]`, or `--version`. It keeps its library, pictures,
//! settings and log in its own folder, the first time taking in what Freya kept in its folder
//! before the server was its own. It runs until stopped: Ctrl-C, SIGTERM, or `POST /quit` from
//! this computer. One that finds another on its address ends at once, and without failing, so
//! what starts it at login does not start it again and again. On macOS it shows an icon in the
//! menu bar, to open Freya, show the log, or stop it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::channel;

use freya_protocol::{NETWORK_PORT, ServerControl};
use freya_server::accounts::Accounts;
use freya_server::discovery::Discovery;
use freya_server::http::Server;
use freya_server::take_in::{Folders, take_in};
use freya_server::tls::Identity;
use freya_server::{LOCAL_ADDRESS, PAIRING_KEY, Served, Service, ServiceConfig, handler, settings};

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
    // Before the library is opened: two servers never share it.
    match std::net::TcpListener::bind(address) {
        Ok(free) => drop(free),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            log::info!("another server answers on {address}; this one is not needed");
            return Ok(());
        }
        Err(e) => return Err(format!("could not listen on {address}: {e}")),
    }
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
    #[cfg(target_os = "macos")]
    let menu_stop = stop.clone();
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
    let identity = Identity::load_or_make(data)?;
    let served = Served {
        library: service.api(),
        server: Some(Arc::clone(&service) as _),
        quit: Some(quit),
        accounts: Some(Arc::new(Accounts::open(
            Some(data.join("accounts.json")),
            Some(data.join(PAIRING_KEY)),
        ))),
        certificate: Some(identity.certificate.clone()),
    };
    let server = Server::start(address, handler(served.clone()))
        .map_err(|e| format!("could not listen on {address}: {e}"))?;
    let network = Network::follow(served, Arc::clone(&service), identity);
    log::info!(
        "Freya's server {} serves {} on {}",
        env!("CARGO_PKG_VERSION"),
        data.display(),
        server.address()
    );
    let close = move || {
        let _ = stopped.recv();
        log::info!("stopping");
        drop(network);
        drop(server);
        drop(service);
    };
    // AppKit runs on the main thread until the process ends, so stopping is another thread's.
    #[cfg(target_os = "macos")]
    {
        let closing = std::thread::spawn(move || {
            close();
            std::process::exit(0)
        });
        let actions = freya_server::menu_bar::Actions {
            stop: menu_stop,
            log: data.join("server.log"),
        };
        if freya_server::menu_bar::run(actions).is_none() {
            let _ = closing.join();
        }
    }
    #[cfg(not(target_os = "macos"))]
    close();
    Ok(())
}

/// Clients on other computers, taken over TLS and answered when looking for servers while
/// the settings allow it; looked at every few seconds, so a change takes at once.
struct Network {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Network {
    fn follow(served: Served, service: Arc<Service>, identity: Identity) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("freya-network".into())
            .spawn(move || {
                let mut listening: Option<(Server, Option<Discovery>)> = None;
                while !stopped.load(Ordering::Relaxed) {
                    let allowed = service.settings().is_ok_and(|s| s.network);
                    if allowed && listening.is_none() {
                        listening = listen(&served, &identity);
                    } else if !allowed && listening.is_some() {
                        log::info!("no longer taking clients on other computers");
                        listening = None;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            })
            .ok();
        Self { stop, thread }
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Takes clients on other computers over TLS, and answers those looking for servers.
fn listen(served: &Served, identity: &Identity) -> Option<(Server, Option<Discovery>)> {
    let tls = match identity.server_config() {
        Ok(tls) => tls,
        Err(e) => {
            log::warn!("no TLS: {e}");
            return None;
        }
    };
    let server = match Server::start_tls(("0.0.0.0", NETWORK_PORT), handler(served.clone()), tls) {
        Ok(server) => server,
        Err(e) => {
            log::warn!("could not take clients on other computers: {e}");
            return None;
        }
    };
    let answered = served.clone();
    let discovery = Discovery::start(move || freya_server::found(&answered))
        .map_err(|e| log::warn!("could not answer clients looking for servers: {e}"))
        .ok();
    log::info!(
        "taking clients on other computers on port {NETWORK_PORT}; certificate {}",
        identity.fingerprint()
    );
    Some((server, discovery))
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
