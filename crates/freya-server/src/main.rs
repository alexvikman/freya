// A window app in release builds on Windows, so starting at login opens no console; it logs
// to its folder.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! `freya-server`: the library as a service for Freya's clients, on this computer.
//!
//! `freya-server [--address HOST:PORT] [--data FOLDER] [--forget-password]`, or `--version`.
//! `--forget-password` forgets the owner's password, with the server stopped, for one who
//! forgot it to set anew. For Windows' installer, `--stop` stops the user's running server, and
//! `--forget-login` takes away its starting at login, as it is uninstalled.
//!
//! It keeps its library, pictures, users, settings and log in a folder of its own, apart from
//! Freya's. It runs until stopped: Ctrl-C, SIGTERM, its menu, or Windows closing its window. It
//! serves this computer over TLS too, with the certificate Freya here pins from its folder. One
//! that finds this user's server on its address already ends at once, and without failing, so
//! what starts it at login does not start it again and again; one that finds another program
//! there says so, and fails. On macOS and Windows it shows an icon in the menu bar or the
//! notification area, to open Freya, show a connection code or the log, or stop it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::channel;

use freya_protocol::{ADOPTION_CODE, NETWORK_PORT, ServerControl};
use freya_server::accounts::Accounts;
use freya_server::discovery::Discovery;
use freya_server::http::Server;
use freya_server::tls::Identity;
use freya_server::{LOCAL_ADDRESS, Served, Service, ServiceConfig, admit, handler, settings};

fn main() {
    more_open_files();
    let mut address = LOCAL_ADDRESS.to_owned();
    let mut data = None;
    let mut forget_password = false;
    let mut show_code = false;
    let mut stop = false;
    let mut forget_login = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--version" {
            println!("freya-server {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        if arg == "--forget-password" {
            forget_password = true;
            continue;
        }
        if arg == "--code" {
            show_code = true;
            continue;
        }
        if arg == "--stop" {
            stop = true;
            continue;
        }
        if arg == "--forget-login" {
            forget_login = true;
            continue;
        }
        match (arg.as_str(), args.next()) {
            ("--address", Some(value)) => address = value,
            ("--data", Some(value)) => data = Some(PathBuf::from(value)),
            _ => {
                eprintln!(
                    "usage: freya-server [--address HOST:PORT] [--data FOLDER] [--forget-password | --code] | --stop [--forget-login] | --version"
                );
                std::process::exit(2);
            }
        }
    }
    // For the installer, and nothing else: no server starts.
    if stop || forget_login {
        std::process::exit(installing(stop, forget_login));
    }
    // A folder of its own, apart from Freya's: Freya's taken away keeps the library.
    let own = directories::ProjectDirs::from("", "", freya_protocol::SERVER_APP);
    let Some(data) = data.or_else(|| own.map(|dirs| dirs.data_dir().to_owned())) else {
        eprintln!("freya-server: no folder to keep the library in");
        std::process::exit(1);
    };
    if let Err(e) = std::fs::create_dir_all(&data) {
        eprintln!("freya-server: could not make {}: {e}", data.display());
        std::process::exit(1);
    }
    if forget_password {
        std::process::exit(forget_owner_password(&address, &data));
    }
    if show_code {
        std::process::exit(print_adoption_code(&data));
    }
    start_log(&data.join("server.log"));
    if let Err(e) = run(&address, &data) {
        log::error!("{e}");
        eprintln!("freya-server: {e}");
        std::process::exit(1);
    }
    std::process::exit(exit_code());
}

/// What Windows' installer asks before replacing or removing the server's files: `stop` the
/// user's running server; `forget_login`, as it is uninstalled, its starting at login.
fn installing(stop: bool, forget_login: bool) -> i32 {
    #[cfg(windows)]
    if stop {
        freya_server::stop::stop();
    }
    #[cfg(not(windows))]
    if stop {
        eprintln!("freya-server: --stop is for Windows' installer");
        return 2;
    }
    if forget_login && let Err(e) = freya_server::autostart::uninstall() {
        eprintln!("freya-server: {e}");
        return 1;
    }
    0
}

/// What the server ends with: 0, or after an update, what has launchd or systemd start it
/// again.
fn exit_code() -> i32 {
    freya_server::updates::EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed)
}

fn run(address: &str, data: &Path) -> Result<(), String> {
    let identity = Identity::load_or_make(data)?;
    // Before the library is opened: two servers never share it. One already answering with
    // this certificate is this user's, and this one is not needed; another program holding the
    // address, of another user perhaps, is said so, and fails, so what starts the server tries
    // again later.
    // Held from here on, so no other server of the user takes it while this one opens the
    // library.
    let listener = match std::net::TcpListener::bind(address) {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            let ours = address
                .parse()
                .is_ok_and(|at| freya_server::tls::answers_as(at, &identity.certificate));
            if ours {
                log::info!("this user's server answers on {address} already");
                return Ok(());
            }
            return Err(format!(
                "another program holds {address}, not this user's server: it cannot start"
            ));
        }
        Err(e) => return Err(format!("could not listen on {address}: {e}")),
    };
    Accounts::readable(&data.join("accounts.json"))?;
    let settings_file = data.join("settings.toml");
    let scrobbles = data.join("lastfm");
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
    #[cfg(any(target_os = "macos", windows))]
    let menu_stop = stop.clone();
    let updates = freya_server::updates::Updates::start(stop.clone());
    follow_autostart(Arc::clone(&service));
    ctrlc::set_handler(move || {
        let _ = stop.send(());
    })
    .map_err(|e| format!("could not listen for being stopped: {e}"))?;
    let accounts = Arc::new(Accounts::open(Some(data.join("accounts.json"))));
    // A key Freya on this computer paired with once: nothing takes it now.
    let _ = std::fs::remove_file(data.join("pairing.key"));
    keep_adoption_code(Arc::clone(&accounts), data.join(ADOPTION_CODE));
    #[cfg(any(target_os = "macos", windows))]
    let menu_accounts = Arc::clone(&accounts);
    let served = Served {
        library: service.api(),
        server: Some(Arc::clone(&service) as _),
        accounts: Some(accounts),
        certificate: Some(identity.certificate.clone()),
        callback_port: freya_protocol::CALLBACK_PORT,
        updates: Some(Arc::clone(&updates) as _),
    };
    // TLS on this computer too, with the certificate the client here pins from this folder.
    let server = Server::serve_on(
        listener,
        handler(served.clone()),
        Some(identity.server_config()?),
        admit(served.clone()),
    )
    .map_err(|e| format!("could not listen on {address}: {e}"))?;
    #[cfg(any(target_os = "macos", windows))]
    let menu_service = Arc::clone(&service);
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
    // The menu bar's or the notification area's icon runs on the main thread until the process
    // ends, so stopping is another thread's.
    #[cfg(any(target_os = "macos", windows))]
    {
        let closing = std::thread::spawn(move || {
            close();
            std::process::exit(exit_code())
        });
        let actions = freya_server::menu::Actions {
            stop: menu_stop,
            log: data.join("server.log"),
            service: menu_service,
            accounts: menu_accounts,
            updates,
        };
        if freya_server::menu::run(actions).is_none() {
            let _ = closing.join();
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = updates;
        close();
    }
    Ok(())
}

/// Keeps the server starting at login as its settings say, pointing at this program, which
/// moves with an update; looked at every few seconds, so a change takes at once. Never from a
/// development build, which is not the program to start.
fn follow_autostart(service: Arc<Service>) {
    use freya_server::autostart;
    if cfg!(debug_assertions) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("freya-autostart".into())
        .spawn(move || {
            let mut applied = None;
            loop {
                if let Ok(settings) = service.settings() {
                    let wanted = settings.start_at_login;
                    if applied != Some(wanted) {
                        let done = if wanted {
                            autostart::Launch::own()
                                .ok_or_else(|| "the server's program is unknown".to_owned())
                                .and_then(|launch| autostart::install(&launch))
                        } else {
                            autostart::uninstall()
                        };
                        if let Err(e) = done {
                            log::warn!("could not change how the server starts: {e}");
                        }
                        // Not tried again and again: once a change asks for it.
                        applied = Some(wanted);
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        });
}

/// Raises how many files the server may hold open to as many as the system lets it: started by
/// launchd or from Freya on macOS it may hold 256, which connections, the library and the
/// music would use up.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "asks the system how many files may be open, and for more"
)]
fn more_open_files() {
    // SAFETY: a limit read into, and given, a struct of the right kind.
    unsafe {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
            return;
        }
        // macOS takes no more than OPEN_MAX, whatever the hard limit says.
        let wanted = limit.rlim_max.min(10_240);
        if limit.rlim_cur < wanted {
            limit.rlim_cur = wanted;
            libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
        }
    }
}

#[cfg(not(unix))]
fn more_open_files() {}

/// Keeps a connection code while the server is not adopted, written to `file` with until when
/// it lasts, a new one as each runs out; the file goes once the server is adopted. The code
/// itself is not logged: a log may be read by more than the server's user.
fn keep_adoption_code(accounts: Arc<Accounts>, file: PathBuf) {
    let kept = std::thread::Builder::new()
        .name("freya-adoption".into())
        .spawn(move || {
            let mut written = None;
            while let Some((code, lasts)) = accounts.adoption_code() {
                if written.as_ref() != Some(&code) {
                    let until = std::time::SystemTime::now() + lasts;
                    let seconds = until
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());
                    let text = format!("{code}\n{seconds}\n");
                    match freya_server::accounts::write_private(&file, text.as_bytes()) {
                        Ok(()) => log::info!(
                            "not adopted yet: freya-server --code shows the code a device adopts it with"
                        ),
                        Err(e) => log::warn!("could not keep the adoption code: {e}"),
                    }
                    written = Some(code);
                }
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
            let _ = std::fs::remove_file(&file);
        });
    if let Err(e) = kept {
        log::warn!("could not keep an adoption code: {e}");
    }
}

/// Prints the code a server in `data` not adopted yet is adopted with: the code to end with.
fn print_adoption_code(data: &Path) -> i32 {
    let kept = std::fs::read_to_string(data.join(ADOPTION_CODE)).unwrap_or_default();
    let mut lines = kept.lines();
    let (Some(code), Some(until)) = (
        lines.next(),
        lines.next().and_then(|u| u.parse::<u64>().ok()),
    ) else {
        eprintln!(
            "freya-server: no code to adopt this server with: it is adopted already (add devices in Freya's settings), or not running"
        );
        return 1;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let Some(left) = until.checked_sub(now).filter(|left| *left > 0) else {
        eprintln!("freya-server: the code ran out: ask again in a few seconds");
        return 1;
    };
    println!(
        "{}\n\nType it in Freya on the computer adopting this server, within {} minutes.",
        freya_protocol::show_code(code),
        left.div_ceil(60)
    );
    0
}

/// Forgets the owner's password in `data`, while no server answers at `address`: the code to
/// end with.
fn forget_owner_password(address: &str, data: &Path) -> i32 {
    let running = address.parse().is_ok_and(|at| {
        std::net::TcpStream::connect_timeout(&at, std::time::Duration::from_secs(1)).is_ok()
    });
    if running {
        eprintln!(
            "freya-server: stop Freya's server first: quit Freya, and its server in the menu bar"
        );
        return 1;
    }
    match Accounts::forget_owner_password(&data.join("accounts.json")) {
        Ok(name) => {
            println!(
                "The password of {name} is forgotten. Set a new one in Freya's settings, under Account."
            );
            0
        }
        Err(e) => {
            eprintln!("freya-server: could not forget the password: {e}");
            1
        }
    }
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
    let server = match Server::serve(
        ("0.0.0.0", NETWORK_PORT),
        handler(served.clone()),
        Some(tls),
        admit(served.clone()),
    ) {
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
    // The user's alone, as the rest of the server's folder.
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(file) {
        Ok(file) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
            }
            builder.target(env_logger::Target::Pipe(Box::new(file)));
        }
        Err(e) => eprintln!("freya-server: could not log to {}: {e}", file.display()),
    }
    builder.init();
}
