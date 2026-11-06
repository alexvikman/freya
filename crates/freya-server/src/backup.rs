//! Backups of everything the server keeps, and restoring one.
//!
//! A backup is one zip of the server's folder: its database as one consistent copy (SQLite's
//! `VACUUM INTO`, while it runs), the pictures and covers, playlists, stations, settings,
//! accounts, certificate and key, and Last.fm's session and plays waiting. What is made again,
//! or belongs to this run alone, is left out: the log, Radio Browser's directory, the adoption
//! code, the database's journal. Under a password, the zip is sealed: AES-256-GCM in chunks of a
//! megabyte, each with its own nonce and a flag on the last, so none can be dropped, swapped or
//! cut off unseen, under a key from the password by PBKDF2-HMAC-SHA256.
//!
//! Restoring checks the backup, unpacks it beside the server's files, and has the server start
//! again: then, before anything is opened, what it had moves to a folder of its own and the
//! backup's files take its place.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::num::NonZeroU32;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use freya_library::Library;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};

/// The database, in the server's folder and in a backup.
pub const DATABASE: &str = "library.sqlite3";
/// A backup's name ends so, as a zip, or sealed under a password.
pub const PLAIN: &str = "zip";
pub const SEALED: &str = "freyabackup";
/// A backup unpacked, waiting for the server's next start.
const PENDING: &str = "restore-pending";
/// What the server had before a backup was restored, by when.
const BEFORE: &str = "before-restore-";
/// What a backup must hold to be the server's.
const NEEDED: [&str; 2] = [DATABASE, "accounts.json"];

const MAGIC: &[u8; 8] = b"FREYABK1";
const CHUNK: usize = 1024 * 1024;
const ITERATIONS: u32 = 600_000;
const SALT: usize = 16;
const PREFIX: usize = 7;

/// Whether `path` is named as a backup is: one this server made, or another's.
pub fn is_backup_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.starts_with("Freya Server backup")
        && (name.ends_with(&format!(".{PLAIN}")) || name.ends_with(&format!(".{SEALED}")))
}

/// Whether `relative`, a file in the server's folder, is left out of a backup.
fn left_out(relative: &Path) -> bool {
    let name = relative.to_string_lossy();
    let first = relative
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_default();
    matches!(
        first.as_str(),
        "server.log" | "radio-directory.json" | "adoption-code" | "pairing.key" | PENDING
    ) || first.starts_with(BEFORE)
        || first.starts_with(DATABASE)
        || name.ends_with(".partial")
}

/// The files of the server's folder a backup holds, with their names in it ("pictures/ab.jpg"),
/// the database apart.
fn files(data: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut folders = vec![data.to_owned()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(data) else {
                continue;
            };
            if left_out(relative) {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                folders.push(path);
            } else if kind.is_file() {
                let name = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                found.push((path, name));
            }
        }
    }
    found.sort_by(|a, b| a.1.cmp(&b.1));
    found
}

/// About how large a backup of `data` is, before it is compressed.
pub fn size(data: &Path) -> u64 {
    let database = std::fs::metadata(data.join(DATABASE)).map_or(0, |m| m.len());
    database
        + files(data)
            .iter()
            .map(|(path, _)| std::fs::metadata(path).map_or(0, |m| m.len()))
            .sum::<u64>()
}

/// Backs up the server's folder `data`, its database read through `db`, into a new file in
/// `folder`, sealed under `password` when one is given. `progress` is told the bytes done and
/// all there are. Returns the file made; when `cancel` is set, stops and leaves nothing.
pub fn make(
    data: &Path,
    db: &Library,
    folder: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<PathBuf, String> {
    if !folder.is_dir() {
        return Err(format!("{} is not a folder.", folder.display()));
    }
    // As the system has them, after links and `..`.
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    if real(folder).starts_with(real(data)) {
        return Err("A backup cannot be kept in the server's own folder.".into());
    }
    let stamp = jiff::Zoned::now().strftime("%Y-%m-%d %H%M%S").to_string();
    let extension = if password.is_some() { SEALED } else { PLAIN };
    let out = folder.join(format!("Freya Server backup {stamp}.{extension}"));
    let partials = Partials(vec![
        folder.join(format!(".freya-backup-{stamp}.database.partial")),
        folder.join(format!(".freya-backup-{stamp}.zip.partial")),
        out.with_extension(format!("{extension}.partial")),
    ]);
    let [database, zipped, sealed] = [&partials.0[0], &partials.0[1], &partials.0[2]];
    db.snapshot(database)
        .map_err(|e| format!("The database could not be copied: {e}"))?;
    let files = files(data);
    let total = std::fs::metadata(database).map_or(0, |m| m.len())
        + files
            .iter()
            .map(|(path, _)| std::fs::metadata(path).map_or(0, |m| m.len()))
            .sum::<u64>();
    let written = if password.is_some() { zipped } else { sealed };
    let mut zip = zip::ZipWriter::new(BufWriter::new(
        private_file(written).map_err(|e| io_error(written, &e))?,
    ));
    let mut done = 0;
    let entries = std::iter::once((database.clone(), DATABASE.to_owned())).chain(files);
    for (path, name) in entries {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        // Pictures are compressed already.
        let method = if name.starts_with("pictures/") {
            zip::CompressionMethod::Stored
        } else {
            zip::CompressionMethod::Deflated
        };
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(method)
            .large_file(true);
        zip.start_file(name.as_str(), options)
            .map_err(|e| e.to_string())?;
        let mut file = File::open(&path).map_err(|e| io_error(&path, &e))?;
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let n = file.read(&mut buffer).map_err(|e| io_error(&path, &e))?;
            if n == 0 {
                break;
            }
            zip.write_all(&buffer[..n])
                .map_err(|e| io_error(written, &e))?;
            done += n as u64;
            progress(done, total);
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
        }
    }
    zip.finish()
        .map_err(|e| e.to_string())?
        .flush()
        .map_err(|e| io_error(written, &e))?;
    if let Some(password) = password {
        seal(zipped, sealed, password, cancel)?;
    }
    std::fs::rename(sealed, &out).map_err(|e| io_error(&out, &e))?;
    drop(partials);
    Ok(out)
}

/// Checks the backup `file`, opened with `password` when it is sealed, and unpacks it beside
/// the server's files in `data`, for its next start to take. Fails, leaving `data` as it was,
/// when it is not a backup of a Freya Server, the password is not its, or it is damaged.
pub fn prepare_restore(file: &Path, password: Option<&str>, data: &Path) -> Result<(), String> {
    let pending = data.join(PENDING);
    let partials = Partials(vec![
        data.join(format!("{PENDING}.zip.partial")),
        data.join(format!("{PENDING}.partial")),
    ]);
    let [opened, unpacked] = [&partials.0[0], &partials.0[1]];
    let zipped = if is_sealed(file)? {
        let password = password.ok_or("This backup is locked with a password.")?;
        unseal(file, opened, password)?;
        opened.clone()
    } else {
        file.to_owned()
    };
    let mut zip = zip::ZipArchive::new(BufReader::new(
        File::open(&zipped).map_err(|e| io_error(&zipped, &e))?,
    ))
    .map_err(|_| "This is not a backup of a Freya Server.".to_owned())?;
    zip.validate_header_consistency()
        .map_err(|e| format!("This backup is damaged: {e}"))?;
    for needed in NEEDED {
        if zip.index_for_name(needed).is_none() {
            return Err("This is not a backup of a Freya Server.".into());
        }
    }
    let _ = std::fs::remove_dir_all(unpacked);
    std::fs::create_dir_all(unpacked).map_err(|e| io_error(unpacked, &e))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().map(|n| n.into_owned()).unwrap_or_default();
        let Some(relative) = safe_relative(&name) else {
            return Err(format!("This backup names a file outside it: {name:?}"));
        };
        if entry.is_dir() {
            continue;
        }
        let path = unpacked.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_error(parent, &e))?;
        }
        let mut out = BufWriter::new(private_file(&path).map_err(|e| io_error(&path, &e))?);
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("This backup is damaged: {e}"))?;
        out.flush().map_err(|e| io_error(&path, &e))?;
    }
    let _ = std::fs::remove_dir_all(&pending);
    std::fs::rename(unpacked, &pending).map_err(|e| io_error(&pending, &e))?;
    drop(partials);
    Ok(())
}

/// At the server's start, before anything in `data` is opened: a backup unpacked by
/// [`prepare_restore`] takes the place of what the server had, which moves to a folder of its
/// own beside, "before-restore-…". Returns that folder, when a backup was restored.
pub fn apply_pending(data: &Path) -> Result<Option<PathBuf>, String> {
    let pending = data.join(PENDING);
    if !pending.is_dir() {
        return Ok(None);
    }
    let stamp = jiff::Zoned::now().strftime("%Y-%m-%d %H%M%S").to_string();
    let before = data.join(format!("{BEFORE}{stamp}"));
    std::fs::create_dir_all(&before).map_err(|e| io_error(&before, &e))?;
    let entries = std::fs::read_dir(data).map_err(|e| io_error(data, &e))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == PENDING || name.starts_with(BEFORE) || name == "server.log" {
            continue;
        }
        let to = before.join(entry.file_name());
        std::fs::rename(entry.path(), &to).map_err(|e| io_error(&to, &e))?;
    }
    let restored = std::fs::read_dir(&pending).map_err(|e| io_error(&pending, &e))?;
    for entry in restored.flatten() {
        let to = data.join(entry.file_name());
        std::fs::rename(entry.path(), &to).map_err(|e| io_error(&to, &e))?;
    }
    std::fs::remove_dir_all(&pending).map_err(|e| io_error(&pending, &e))?;
    Ok(Some(before))
}

/// Files removed when dropped: what a backup or a restore that did not finish left.
struct Partials(Vec<PathBuf>);

impl Drop for Partials {
    fn drop(&mut self) {
        for path in &self.0 {
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(path);
            } else {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// A new file only its user can read where the system has that.
fn private_file(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

fn io_error(path: &Path, e: &std::io::Error) -> String {
    format!("{}: {e}", path.display())
}

/// `name`, a path in a backup, as one to put under the folder it is unpacked into: plain names
/// only.
fn safe_relative(name: &str) -> Option<PathBuf> {
    let mut path = PathBuf::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." | ".." => return None,
            _ if part.contains([':', '\0']) => return None,
            _ => path.push(part),
        }
    }
    (path.components().count() > 0 && path.components().all(|c| matches!(c, Component::Normal(_))))
        .then_some(path)
}

/// Whether `file` is sealed under a password.
fn is_sealed(file: &Path) -> Result<bool, String> {
    let mut start = [0u8; 8];
    let mut opened = File::open(file).map_err(|e| io_error(file, &e))?;
    let read = opened.read(&mut start).map_err(|e| io_error(file, &e))?;
    Ok(read == MAGIC.len() && &start == MAGIC)
}

fn key(password: &str, salt: &[u8], iterations: u32) -> Result<LessSafeKey, String> {
    let mut bytes = [0u8; 32];
    let iterations = NonZeroU32::new(iterations).ok_or("This backup is damaged.")?;
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        password.as_bytes(),
        &mut bytes,
    );
    let key = UnboundKey::new(&AES_256_GCM, &bytes).map_err(|_| "no key".to_owned())?;
    Ok(LessSafeKey::new(key))
}

/// The nonce of the `index`th chunk, the last one flagged.
fn nonce(prefix: &[u8; PREFIX], index: u32, last: bool) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[..PREFIX].copy_from_slice(prefix);
    bytes[PREFIX..PREFIX + 4].copy_from_slice(&index.to_be_bytes());
    bytes[11] = u8::from(last);
    Nonce::assume_unique_for_key(bytes)
}

/// Seals the zip at `from` into `to` under `password`.
fn seal(from: &Path, to: &Path, password: &str, cancel: &AtomicBool) -> Result<(), String> {
    let random = SystemRandom::new();
    let mut salt = [0u8; SALT];
    let mut prefix = [0u8; PREFIX];
    random
        .fill(&mut salt)
        .and_then(|()| random.fill(&mut prefix))
        .map_err(|_| "no random numbers".to_owned())?;
    let key = key(password, &salt, ITERATIONS)?;
    let mut input = BufReader::new(File::open(from).map_err(|e| io_error(from, &e))?);
    let mut out = BufWriter::new(private_file(to).map_err(|e| io_error(to, &e))?);
    let header = [
        MAGIC.as_slice(),
        &ITERATIONS.to_be_bytes(),
        salt.as_slice(),
        prefix.as_slice(),
    ]
    .concat();
    out.write_all(&header).map_err(|e| io_error(to, &e))?;
    let mut chunk = read_chunk(&mut input).map_err(|e| io_error(from, &e))?;
    let mut index: u32 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let next = if chunk.len() == CHUNK {
            read_chunk(&mut input).map_err(|e| io_error(from, &e))?
        } else {
            Vec::new()
        };
        let last = next.is_empty();
        let mut sealed = chunk;
        key.seal_in_place_append_tag(nonce(&prefix, index, last), Aad::empty(), &mut sealed)
            .map_err(|_| "could not seal the backup".to_owned())?;
        out.write_all(&[u8::from(last)])
            .and_then(|()| out.write_all(&(sealed.len() as u32).to_be_bytes()))
            .and_then(|()| out.write_all(&sealed))
            .map_err(|e| io_error(to, &e))?;
        if last {
            break;
        }
        chunk = next;
        index = index.checked_add(1).ok_or("The backup is too large.")?;
    }
    out.flush().map_err(|e| io_error(to, &e))
}

/// Up to [`CHUNK`] bytes of `input`, fewer only at its end.
fn read_chunk(input: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut chunk = Vec::with_capacity(CHUNK);
    input.take(CHUNK as u64).read_to_end(&mut chunk)?;
    Ok(chunk)
}

/// Opens the sealed backup `from` into the zip `to` with `password`.
fn unseal(from: &Path, to: &Path, password: &str) -> Result<(), String> {
    let damaged = || "This backup is damaged.".to_owned();
    let mut input = BufReader::new(File::open(from).map_err(|e| io_error(from, &e))?);
    let mut header = [0u8; 8 + 4 + SALT + PREFIX];
    input.read_exact(&mut header).map_err(|_| damaged())?;
    if &header[..8] != MAGIC {
        return Err(damaged());
    }
    let iterations = u32::from_be_bytes(header[8..12].try_into().map_err(|_| damaged())?);
    let salt = &header[12..12 + SALT];
    let prefix: [u8; PREFIX] = header[12 + SALT..].try_into().map_err(|_| damaged())?;
    let key = key(password, salt, iterations)?;
    let mut out = BufWriter::new(private_file(to).map_err(|e| io_error(to, &e))?);
    let mut index: u32 = 0;
    loop {
        let mut head = [0u8; 5];
        input.read_exact(&mut head).map_err(|_| damaged())?;
        let last = match head[0] {
            0 => false,
            1 => true,
            _ => return Err(damaged()),
        };
        let length = u32::from_be_bytes(head[1..].try_into().map_err(|_| damaged())?) as usize;
        if length > CHUNK + AES_256_GCM.tag_len() {
            return Err(damaged());
        }
        let mut sealed = vec![0u8; length];
        input.read_exact(&mut sealed).map_err(|_| damaged())?;
        let opened = key
            .open_in_place(nonce(&prefix, index, last), Aad::empty(), &mut sealed)
            .map_err(|_| {
                if index == 0 {
                    "The password is not this backup's.".to_owned()
                } else {
                    damaged()
                }
            })?;
        out.write_all(opened).map_err(|e| io_error(to, &e))?;
        if last {
            // Nothing may follow the last chunk.
            let mut rest = [0u8; 1];
            if input.read(&mut rest).map_err(|_| damaged())? != 0 {
                return Err(damaged());
            }
            break;
        }
        index = index.checked_add(1).ok_or_else(damaged)?;
    }
    out.flush().map_err(|e| io_error(to, &e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("freya-backup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A server's folder with a library, accounts, a picture and what is left out.
    fn server(dir: &Path) -> Library {
        let db = Library::open(&dir.join(DATABASE)).unwrap();
        std::fs::write(dir.join("accounts.json"), "{\"users\":[]}").unwrap();
        std::fs::write(dir.join("settings.toml"), "network = true\n").unwrap();
        std::fs::create_dir_all(dir.join("pictures/ab")).unwrap();
        std::fs::write(dir.join("pictures/ab/cover.jpg"), vec![7u8; 3 * CHUNK + 5]).unwrap();
        std::fs::write(dir.join("server.log"), "left out").unwrap();
        std::fs::write(dir.join("radio-directory.json"), "[]").unwrap();
        db
    }

    fn backed_up_and_restored(password: Option<&str>) {
        let name = if password.is_some() {
            "sealed"
        } else {
            "plain"
        };
        let data = temp(&format!("{name}-data"));
        let kept = temp(&format!("{name}-kept"));
        let db = server(&data);
        let mut seen = 0;
        let file = make(
            &data,
            &db,
            &kept,
            password,
            &AtomicBool::new(false),
            &mut |done, total| {
                assert!(done <= total);
                seen = done;
            },
        )
        .unwrap();
        assert!(seen > 3 * CHUNK as u64);
        assert_eq!(
            std::fs::read_dir(&kept).unwrap().count(),
            1,
            "no partials left"
        );
        drop(db);

        // Elsewhere, a new server: the backup takes its place, and what it had is kept apart.
        let other = temp(&format!("{name}-other"));
        std::fs::write(other.join("accounts.json"), "{\"other\":true}").unwrap();
        if password.is_some() {
            assert_eq!(
                prepare_restore(&file, None, &other).unwrap_err(),
                "This backup is locked with a password."
            );
            assert_eq!(
                prepare_restore(&file, Some("wrong"), &other).unwrap_err(),
                "The password is not this backup's."
            );
        }
        prepare_restore(&file, password, &other).unwrap();
        let before = apply_pending(&other).unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(other.join("accounts.json")).unwrap(),
            "{\"users\":[]}"
        );
        assert_eq!(
            std::fs::read_to_string(before.join("accounts.json")).unwrap(),
            "{\"other\":true}"
        );
        assert_eq!(
            std::fs::read(other.join("pictures/ab/cover.jpg"))
                .unwrap()
                .len(),
            3 * CHUNK + 5
        );
        assert!(!other.join("server.log").exists());
        assert!(!other.join("radio-directory.json").exists());
        Library::open(&other.join(DATABASE)).unwrap();
        assert!(apply_pending(&other).unwrap().is_none(), "once");
        for dir in [data, kept, other] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn a_backup_holds_everything_and_restores_it() {
        backed_up_and_restored(None);
    }

    #[test]
    fn a_sealed_backup_opens_with_its_password_only() {
        backed_up_and_restored(Some("correct horse"));
    }

    #[test]
    fn a_sealed_backup_cut_or_changed_is_refused() {
        let dir = temp("tampered");
        let zip = dir.join("plain.zip");
        std::fs::write(&zip, vec![1u8; 2 * CHUNK + 10]).unwrap();
        let sealed = dir.join("sealed.freyabackup");
        seal(&zip, &sealed, "pw", &AtomicBool::new(false)).unwrap();
        let bytes = std::fs::read(&sealed).unwrap();
        let out = dir.join("out.zip");
        unseal(&sealed, &out, "pw").unwrap();
        assert_eq!(std::fs::read(&out).unwrap().len(), 2 * CHUNK + 10);
        // Its last chunk cut off.
        let header = 8 + 4 + SALT + PREFIX;
        let first = header + 5 + CHUNK + 16;
        std::fs::write(&sealed, &bytes[..first + 5 + CHUNK + 16]).unwrap();
        assert!(unseal(&sealed, &out, "pw").is_err());
        // A byte changed.
        let mut changed = bytes.clone();
        changed[first + 100] ^= 1;
        std::fs::write(&sealed, &changed).unwrap();
        assert!(unseal(&sealed, &out, "pw").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_backup_naming_files_outside_is_refused() {
        assert!(safe_relative("pictures/a.jpg").is_some());
        for name in ["../x", "/etc/passwd", "a/../../x", "C:x", "a\\..\\x", ""] {
            assert!(safe_relative(name).is_none(), "{name:?}");
        }
        assert!(left_out(Path::new("server.log")));
        assert!(left_out(Path::new("library.sqlite3-wal")));
        assert!(left_out(Path::new("before-restore-2026/accounts.json")));
        assert!(!left_out(Path::new("pictures/ab/c.jpg")));
    }
}
