//! One Freya at a time, on Windows and Linux: started again, as from the Start menu, the
//! server's Open Freya, or a file opened with it, Freya hands the files it was given to the one
//! already running, which opens its window and plays them, and quits. macOS brings a running app
//! forward itself.
//!
//! The first listens on this computer's loopback, at a port it writes with two secrets into a
//! file in its folder, which only its user can read, and takes away when it quits. One started
//! later sends the first secret there, and its files only once it is answered with the second:
//! so that whatever else holds that port, as another user's program might once Freya is gone,
//! learns nothing and keeps no Freya from starting.

#![cfg_attr(
    windows,
    allow(
        unsafe_code,
        reason = "lets the running Freya bring its window to the front, as the one started may"
    )
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// How long one started later waits for the running Freya, which answers at once when it is
/// there: one that does not is taken for gone.
const PATIENCE: Duration = Duration::from_secs(2);
/// What one started later sends at most: the first secret and its files' paths.
const MOST_BYTES: u64 = 1024 * 1024;
const GREETING: &str = "freya-open 1";
const TAKEN: &str = "taken";

/// The files the running Freya was asked to open since this was last asked, its window too.
static ASKED: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);
/// Where this Freya, the first, said how it is reached, and what it wrote there.
static KEPT: OnceLock<(PathBuf, String)> = OnceLock::new();

/// Before anything else: hands `files` to the Freya already running, and returns true, for this
/// one to quit; or, being the first, listens for those started later and returns false.
pub fn hand_over(files: &[PathBuf]) -> bool {
    if cfg!(target_os = "macos") {
        return false;
    }
    let Some(path) = crate::settings::one_freya_path() else {
        return false;
    };
    let files: Vec<PathBuf> = files
        .iter()
        .map(|file| std::path::absolute(file).unwrap_or_else(|_| file.clone()))
        .collect();
    if send(&path, &files).is_ok() {
        log::info!("Freya is running already: it opens its window");
        return true;
    }
    if let Err(e) = listen(&path) {
        log::warn!("Freya started again will not find this one: {e}");
    }
    false
}

/// Once Freya quits: takes away what says how it is reached, unless one started since wrote its
/// own there.
pub fn forget() {
    if let Some((path, written)) = KEPT.get()
        && std::fs::read_to_string(path).is_ok_and(|kept| kept == *written)
    {
        let _ = std::fs::remove_file(path);
    }
}

/// Whether one started later asked this Freya to open its window, and the files it gave, since
/// this was last asked.
pub fn take() -> Option<Vec<PathBuf>> {
    ASKED.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Sends `files` to the Freya whose port and secrets are in the file at `path`, once it proves
/// it is that Freya.
fn send(path: &Path, files: &[PathBuf]) -> std::io::Result<()> {
    let kept = std::fs::read_to_string(path)?;
    let mut fields = kept.split_whitespace();
    let (Some(port), Some(ask), Some(proof), Some(process)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(std::io::ErrorKind::InvalidData.into());
    };
    let port: u16 = port.parse().map_err(|_| std::io::ErrorKind::InvalidData)?;
    let stream =
        TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), PATIENCE)?;
    stream.set_read_timeout(Some(PATIENCE))?;
    stream.set_write_timeout(Some(PATIENCE))?;
    let mut answers = BufReader::new((&stream).take(256));
    (&stream).write_all(format!("{GREETING}\n{ask}\n").as_bytes())?;
    let mut proven = String::new();
    answers.read_line(&mut proven)?;
    if !same(proven.trim_end().as_bytes(), proof.as_bytes()) {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    let mut message = String::new();
    for file in files {
        // A path that is not Unicode, or that has a line break in it, cannot be sent: it stays
        // out, and the window opens all the same.
        match file.to_str() {
            Some(file) if !file.contains(['\n', '\r']) => {
                message.push_str(file);
                message.push('\n');
            }
            _ => log::warn!("not handed to the running Freya: {}", file.display()),
        }
    }
    (&stream).write_all(message.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut answer = String::new();
    answers.read_to_string(&mut answer)?;
    if answer.trim() != TAKEN {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    allow_to_front(process);
    Ok(())
}

/// Listens for Freya started later, and keeps where in the file at `path`.
fn listen(path: &Path) -> std::io::Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let (ask, proof) = (secret()?, secret()?);
    let written = format!("{port} {ask} {proof} {}\n", std::process::id());
    write_private(path, &written)?;
    let _ = KEPT.set((path.to_owned(), written));
    std::thread::Builder::new()
        .name("freya-one".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                if let Err(e) = answer(stream, &ask, &proof) {
                    log::debug!("not asked by Freya started again: {e}");
                }
            }
        })?;
    Ok(())
}

/// Takes the files of one started later, when it knows the first secret, once this one has
/// answered with the second, and says so.
fn answer(stream: TcpStream, ask: &str, proof: &str) -> std::io::Result<()> {
    stream.set_read_timeout(Some(PATIENCE))?;
    stream.set_write_timeout(Some(PATIENCE))?;
    let mut lines = BufReader::new((&stream).take(MOST_BYTES)).lines();
    let greeting = lines.next().transpose()?;
    let given = lines.next().transpose()?;
    if greeting.as_deref() != Some(GREETING)
        || !given.is_some_and(|given| same(given.as_bytes(), ask.as_bytes()))
    {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    (&stream).write_all(format!("{proof}\n").as_bytes())?;
    let files: Vec<PathBuf> = lines
        .map_while(Result::ok)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();
    ASKED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Vec::new)
        .extend(files);
    // At once, rather than at the app's next tick.
    let _ = slint::invoke_from_event_loop(|| {});
    (&stream).write_all(format!("{TAKEN}\n").as_bytes())
}

/// 32 random bytes, in hex.
fn secret() -> std::io::Result<String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| std::io::Error::other("no random numbers"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Compares the secret given with the one kept in a time that does not depend on where they
/// differ.
fn same(given: &[u8], kept: &[u8]) -> bool {
    given.len() == kept.len() && given.iter().zip(kept).fold(0, |d, (a, b)| d | (a ^ b)) == 0
}

/// Writes `text` to `path` through a new file renamed over the old, readable by its user alone.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("partial");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    // On Windows, the folder is the user's own.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&partial)?.write_all(text.as_bytes())?;
    std::fs::rename(&partial, path)
}

/// Lets the running Freya, process `process`, bring its window to the front: Windows lets only
/// the app the user just started do that, which is this one.
fn allow_to_front(process: &str) {
    #[cfg(windows)]
    if let Ok(process) = process.parse::<u32>() {
        // SAFETY: a plain call with a process id.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(process);
        }
    }
    #[cfg(not(windows))]
    let _ = process;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freya_started_again_hands_its_files_over() {
        let dir = std::env::temp_dir().join(format!("freya-one-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("one-freya");
        // None running.
        assert!(send(&path, &[]).is_err());
        listen(&path).unwrap();
        let files = [
            PathBuf::from("/music/a.flac"),
            PathBuf::from("/music/b c.mp3"),
        ];
        send(&path, &files).unwrap();
        assert_eq!(take(), Some(files.to_vec()));
        assert_eq!(take(), None);
        // Without the first secret, nothing is taken.
        let kept = std::fs::read_to_string(&path).unwrap();
        let wrong = kept.replacen(kept.split_whitespace().nth(1).unwrap(), "0", 1);
        std::fs::write(&path, wrong).unwrap();
        assert!(send(&path, &files).is_err());
        assert_eq!(take(), None);
        // Quitting takes the file away, but not one written since by another Freya.
        std::fs::write(&path, &kept).unwrap();
        forget();
        assert!(!path.exists());
        std::fs::write(&path, "another").unwrap();
        forget();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn whatever_else_holds_the_port_learns_no_files_and_stops_no_freya() {
        let dir = std::env::temp_dir().join(format!("freya-squat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A Freya that quit without taking its file away, and another program at its port that
        // answers as Freya would, without knowing the second secret.
        let squatter = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = squatter.local_addr().unwrap().port();
        let path = dir.join("one-freya");
        std::fs::write(&path, format!("{port} ask proof 1\n")).unwrap();
        let heard = std::thread::spawn(move || {
            let (stream, _) = squatter.accept().unwrap();
            stream.set_read_timeout(Some(PATIENCE)).unwrap();
            (&stream).write_all(b"guess\ntaken\n").unwrap();
            let mut heard = String::new();
            let _ = (&stream).take(4096).read_to_string(&mut heard);
            heard
        });
        assert!(send(&path, &[PathBuf::from("/music/private.flac")]).is_err());
        let heard = heard.join().unwrap();
        assert!(!heard.contains("private"), "{heard:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn secrets_are_compared_whole() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abd", b"abc"));
        assert!(!same(b"ab", b"abc"));
    }
}
