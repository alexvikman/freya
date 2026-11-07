//! Exporting what the users made with the library, and taking another's export in.
//!
//! An export is a zip ("….freyaexport") of `export.json` ([`Export`]) and the pictures it names
//! (`pictures/<sha256>`): every user's plays, favourites, playlists and what they say of
//! themselves, the radio stations, and what was changed of artists, albums and genres; no
//! account, password or key. Taken in, its tracks are found again in this library (by path,
//! path within a music folder, or tags), its users by their names (the owner as the owner), and
//! its playlists by their names, one already here taken in as "(imported)". It adds and never
//! removes, and says what found no track.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use freya_library::Library;
use freya_media::pictures::Pictures;
use freya_protocol::{
    EXPORT_VERSION, Export, ExportedEntry, ExportedPlaylist, ExportedUser, ImportSummary,
    PlaylistEntry, RadioStation, SavedPlaylist, TrackRef,
};

/// An export's name ends so.
pub const EXTENSION: &str = "freyaexport";
const DATA: &str = "export.json";
/// At most this large: the data, and each picture.
const MOST_DATA: u64 = 512 * 1024 * 1024;
const MOST_PICTURE: u64 = 64 * 1024 * 1024;
/// How many tracks found nowhere are named.
const MOST_MISSING: usize = 100;
/// The owner, the same on every server.
const OWNER: i64 = 1;

/// Whether `path` is named as an export is.
pub fn is_export_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.starts_with("Freya Server export") && name.ends_with(&format!(".{EXTENSION}"))
}

/// What a server keeps besides its library's database, for an export.
pub struct Kept<'a> {
    pub folders: &'a [PathBuf],
    pub users: Vec<ExportedUser>,
    pub playlists: Vec<SavedPlaylist>,
    pub stations: Vec<RadioStation>,
    pub pictures: Option<&'a Pictures>,
}

/// Exports what the users made into a new file in `folder`. Returns the file.
pub fn export(db: &Library, kept: Kept<'_>, folder: &Path) -> Result<PathBuf, String> {
    if !folder.is_dir() {
        return Err(format!("{} is not a folder.", folder.display()));
    }
    let made = db.made(kept.folders).map_err(|e| e.to_string())?;
    let mut playlists = Vec::new();
    for playlist in &kept.playlists {
        let mut tracks = Vec::new();
        for entry in &playlist.entries {
            let start = entry.part.map(|p| p.start.as_millis() as i64);
            tracks.push(ExportedEntry {
                track: db
                    .track_ref(&entry.path, start, kept.folders)
                    .map_err(|e| e.to_string())?,
                title: entry.title.clone(),
                artist: entry.artist.clone(),
                album: entry.album.clone(),
                duration_ms: entry.duration.map(|d| d.as_millis() as u64),
            });
        }
        playlists.push(ExportedPlaylist {
            name: playlist.name.clone(),
            created: playlist.created,
            tracks,
        });
    }
    let pictures = made.pictures();
    let export = Export {
        version: EXPORT_VERSION,
        exported: std::time::SystemTime::now(),
        folders: kept.folders.to_vec(),
        users: kept.users,
        made,
        playlists,
        stations: kept.stations,
    };
    let stamp = jiff::Zoned::now().strftime("%Y-%m-%d %H%M%S").to_string();
    let out = folder.join(format!("Freya Server export {stamp}.{EXTENSION}"));
    if out.exists() {
        return Err(format!("{} is there already.", out.display()));
    }
    let partial = folder.join(format!(".freya-export-{}.partial", random_name()?));
    let written = (|| -> Result<(), String> {
        let mut zip = zip::ZipWriter::new(BufWriter::new(
            new_file(&partial).map_err(|e| e.to_string())?,
        ));
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file(DATA, deflated).map_err(|e| e.to_string())?;
        serde_json::to_writer(&mut zip, &export).map_err(|e| e.to_string())?;
        if let Some(store) = kept.pictures {
            let stored = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for checksum in &pictures {
                let Some(bytes) = store.read(checksum) else {
                    continue;
                };
                zip.start_file(format!("pictures/{checksum}"), stored)
                    .map_err(|e| e.to_string())?;
                zip.write_all(&bytes).map_err(|e| e.to_string())?;
            }
        }
        zip.finish()
            .map_err(|e| e.to_string())?
            .flush()
            .map_err(|e| e.to_string())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    std::fs::rename(&partial, &out).map_err(|e| {
        let _ = std::fs::remove_file(&partial);
        e.to_string()
    })?;
    Ok(out)
}

/// What a server takes an export into, besides its library's database.
pub struct ImportInto<'a> {
    pub folders: &'a [PathBuf],
    /// Its users, by id and name.
    pub users: Vec<(i64, String)>,
    pub playlists: &'a crate::playlists::Playlists,
    pub stations: Option<&'a crate::stations::Stations>,
    pub pictures: Option<&'a Pictures>,
}

/// Takes the export `file` in. Fails, having taken nothing, when it is not an export of a Freya
/// Server, is from a later Freya, or is damaged.
pub fn import(
    db: &mut Library,
    into: ImportInto<'_>,
    file: &Path,
) -> Result<ImportSummary, String> {
    if !is_export_name(file) {
        return Err("This is not named as an export of a Freya Server is.".into());
    }
    let (export, pictures) = read(file)?;
    let mut summary = ImportSummary::default();

    // Users by name; the owner as the owner, whatever their names.
    let here: HashMap<String, i64> = into
        .users
        .iter()
        .map(|(id, name)| (name.to_lowercase(), *id))
        .collect();
    let mut users = HashMap::from([(OWNER, OWNER)]);
    for user in &export.users {
        if user.id == OWNER {
            continue;
        }
        match here.get(&user.name.to_lowercase()) {
            Some(id) => {
                users.insert(user.id, *id);
            }
            None => summary.users_left_out.push(user.name.clone()),
        }
    }

    // Playlists by their names; one here already is taken in beside it.
    let mut names: HashSet<String> = into
        .playlists
        .all()
        .into_iter()
        .map(|p| p.name.to_lowercase())
        .collect();
    let mut renamed = HashMap::new();
    for playlist in &export.playlists {
        let mut name = playlist.name.clone();
        let mut n = 1;
        while names.contains(&name.to_lowercase()) {
            name = if n == 1 {
                format!("{} (imported)", playlist.name)
            } else {
                format!("{} (imported {n})", playlist.name)
            };
            n += 1;
        }
        names.insert(name.to_lowercase());
        renamed.insert(playlist.name.clone(), name);
    }

    // Every track the export names, found here once.
    let tracks: Vec<&TrackRef> = export
        .made
        .plays
        .iter()
        .map(|play| &play.track)
        .chain(export.made.playlist_added.iter().map(|added| &added.track))
        .chain(
            export
                .playlists
                .iter()
                .flat_map(|p| p.tracks.iter().map(|e| &e.track)),
        )
        .collect();
    let found = db
        .find_tracks(tracks, into.folders)
        .map_err(|e| e.to_string())?;
    let mut missing: Vec<&TrackRef> = found
        .iter()
        .filter(|(_, here)| here.is_none())
        .map(|(track, _)| track)
        .collect();
    missing.sort_by_key(|track| (track.album_artist.clone(), track.title.clone()));
    summary.missing_count = missing.len();
    summary.missing = missing
        .iter()
        .take(MOST_MISSING)
        .map(|track| {
            let title = track
                .title
                .clone()
                .unwrap_or_else(|| track.path.display().to_string());
            match &track.album_artist {
                Some(artist) => format!("{artist} – {title}"),
                None => title,
            }
        })
        .collect();

    // The pictures the export names, each checked to be what its name says.
    if let Some(store) = into.pictures {
        for (checksum, bytes) in &pictures {
            if Pictures::checksum(bytes) == *checksum {
                store.put(bytes).map_err(|e| e.to_string())?;
            }
        }
    }

    for playlist in &export.playlists {
        let entries: Vec<PlaylistEntry> = playlist
            .tracks
            .iter()
            .filter_map(|entry| {
                let (path, start) = found.get(&entry.track).cloned().flatten()?;
                let part = start.and_then(|start| {
                    let start = Duration::from_millis(start.max(0) as u64);
                    db.track_at(Path::new(&path), start).ok().flatten()?.part
                });
                Some(PlaylistEntry {
                    path: PathBuf::from(path),
                    part,
                    title: entry.title.clone(),
                    artist: entry.artist.clone(),
                    album: entry.album.clone(),
                    duration: entry.duration_ms.map(Duration::from_millis),
                })
            })
            .collect();
        let name = &renamed[&playlist.name];
        into.playlists.save(name, &entries)?;
        summary.playlists += 1;
    }

    if let Some(stations) = into.stations {
        let mut all = stations.all();
        let before = all.len();
        for station in &export.stations {
            if !all.iter().any(|s| s.url == station.url) {
                all.push(station.clone());
            }
        }
        summary.stations = all.len() - before;
        if summary.stations > 0 {
            stations.save(&all)?;
        }
    }

    let taken = db
        .take_made(&export.made, &users, &renamed, &mut |track| {
            found.get(track).cloned().flatten()
        })
        .map_err(|e| e.to_string())?;
    summary.plays = taken.plays;
    summary.favorites = taken.favorites;
    summary.edits = taken.edits;
    Ok(summary)
}

/// The export in `file`, and its pictures by their names.
fn read(file: &Path) -> Result<(Export, HashMap<String, Vec<u8>>), String> {
    let not_one = || "This is not an export of a Freya Server.".to_owned();
    let mut zip = zip::ZipArchive::new(BufReader::new(
        File::open(file).map_err(|e| format!("{}: {e}", file.display()))?,
    ))
    .map_err(|_| not_one())?;
    zip.validate_header_consistency()
        .map_err(|e| format!("This export is damaged: {e}"))?;
    let mut data = Vec::new();
    zip.by_name(DATA)
        .map_err(|_| not_one())?
        .take(MOST_DATA + 1)
        .read_to_end(&mut data)
        .map_err(|e| format!("This export is damaged: {e}"))?;
    if data.len() as u64 > MOST_DATA {
        return Err("This export is larger than Freya takes.".into());
    }
    let export: Export = serde_json::from_slice(&data).map_err(|_| not_one())?;
    if export.version > EXPORT_VERSION {
        return Err("This export is from a later Freya: update this server first.".into());
    }
    let wanted: HashSet<String> = export.made.pictures().into_iter().collect();
    let mut pictures = HashMap::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().map(|n| n.into_owned()).unwrap_or_default();
        let Some(checksum) = name.strip_prefix("pictures/") else {
            continue;
        };
        if !wanted.contains(checksum) {
            continue;
        }
        let mut bytes = Vec::new();
        (&mut entry)
            .take(MOST_PICTURE + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("This export is damaged: {e}"))?;
        if bytes.len() as u64 <= MOST_PICTURE {
            pictures.insert(checksum.to_owned(), bytes);
        }
    }
    Ok((export, pictures))
}

/// A new file, never one there already, nor through a link.
fn new_file(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn random_name() -> Result<String, String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 8];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "no random numbers".to_owned())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playlists::Playlists;
    use crate::stations::Stations;
    use std::sync::atomic::AtomicBool;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("freya-transfer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A server's library of the fixture sine.flac, at `music/Sine/sine.flac`, scanned.
    fn library(dir: &Path) -> (Library, PathBuf) {
        let music = dir.join("music");
        std::fs::create_dir_all(music.join("Sine")).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../freya-decode/tests/fixtures/sine.flac"),
            music.join("Sine/sine.flac"),
        )
        .unwrap();
        let mut db = Library::open(&dir.join("library.sqlite3")).unwrap();
        freya_library::library::scan(
            &mut db,
            std::slice::from_ref(&music),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        (db, music)
    }

    #[test]
    fn what_users_made_moves_to_another_server() {
        let old = temp("old");
        let (db, music) = library(&old);
        let track = music.join("Sine/sine.flac");
        db.record_play(&track, None, std::time::SystemTime::now(), Some("Mix"))
            .unwrap();
        db.set_artist_favorite("Freya", true, std::time::SystemTime::now())
            .unwrap();
        let playlists = Playlists::new(old.join("playlists"));
        let entry = PlaylistEntry {
            path: track.clone(),
            part: None,
            title: Some("Sine".into()),
            artist: None,
            album: None,
            duration: None,
        };
        playlists.save("Mix", &[entry]).unwrap();
        let station = RadioStation {
            name: "SomaFM".into(),
            url: "https://ice1.somafm.com/groovesalad-128-mp3".into(),
        };
        let stations = Stations::new(old.join("stations.m3u8"));
        stations.save(std::slice::from_ref(&station)).unwrap();
        let kept_in = temp("kept");
        let file = export(
            &db,
            Kept {
                folders: std::slice::from_ref(&music),
                users: vec![ExportedUser {
                    id: OWNER,
                    name: "Alex".into(),
                }],
                playlists: playlists.all(),
                stations: stations.all(),
                pictures: None,
            },
            &kept_in,
        )
        .unwrap();
        assert!(is_export_name(&file));

        // Another server: the same music elsewhere, and a playlist of the same name already.
        let new = temp("new");
        let (mut other, other_music) = library(&new);
        let other_playlists = Playlists::new(new.join("playlists"));
        other_playlists.save("Mix", &[]).unwrap();
        let other_stations = Stations::new(new.join("stations.m3u8"));
        let into = |playlists, stations| ImportInto {
            folders: std::slice::from_ref(&other_music),
            users: vec![(OWNER, "Someone".into())],
            playlists,
            stations: Some(stations),
            pictures: None,
        };
        let summary = import(&mut other, into(&other_playlists, &other_stations), &file).unwrap();
        assert_eq!(summary.plays, 1);
        assert_eq!(summary.playlists, 1);
        assert_eq!(summary.stations, 1);
        assert_eq!(summary.favorites, 1);
        assert_eq!(summary.missing_count, 0);
        let names: Vec<String> = other_playlists.all().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Mix", "Mix (imported)"]);
        let imported = &other_playlists.all()[1];
        assert_eq!(imported.entries[0].path, other_music.join("Sine/sine.flac"));
        let history = other.history(std::time::UNIX_EPOCH, 10).unwrap();
        assert_eq!(history[0].playlist.as_deref(), Some("Mix (imported)"));
        assert_eq!(other_stations.all(), [station]);
        // Taken in again: a playlist more, but no play or station twice.
        let again = import(&mut other, into(&other_playlists, &other_stations), &file).unwrap();
        assert_eq!((again.plays, again.stations), (0, 0));
        // What is not an export is refused.
        assert!(
            import(
                &mut other,
                into(&other_playlists, &other_stations),
                &kept_in.join("x.zip")
            )
            .is_err()
        );
        for dir in [old, new, kept_in] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
