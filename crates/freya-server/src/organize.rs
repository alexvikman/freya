//! Writing what was decided about the music's files to them, and putting them back.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use freya_library::identify::{
    self, FileToMatch, FolderToMatch, Found, Release, Source, album_and_edition,
};
use freya_library::library::organize::SavedFile;
use freya_library::library::organize::names::{album_folder, parse_folder_name};
use freya_library::tagging;
use freya_media::artist_image::{
    MUSICBRAINZ_INTERVAL, cover_art, musicbrainz_release, musicbrainz_release_search,
};
use freya_protocol::{OrganizeFile, SaveFailure, TagDraft, TagField, TagMap};

use crate::folders::music_file;

/// What became of drafts written to their files.
#[derive(Default)]
pub struct Written {
    pub saved: Vec<SavedFile>,
    pub failed: Vec<SaveFailure>,
    /// Files that already said what their drafts did.
    pub unchanged: Vec<PathBuf>,
}

/// Writes `drafts` to their files, each a music file within `folders`. A file that fails is
/// left as it was and the others go on.
pub fn write_drafts(drafts: &[TagDraft], folders: &[PathBuf]) -> Written {
    let mut written = Written::default();
    for draft in drafts {
        match write_draft(draft, folders) {
            Ok(Some(saved)) => written.saved.push(saved),
            Ok(None) => written.unchanged.push(draft.path.clone()),
            Err(why) => written.failed.push(SaveFailure {
                path: draft.path.clone(),
                why,
            }),
        }
    }
    written
}

fn write_draft(draft: &TagDraft, folders: &[PathBuf]) -> Result<Option<SavedFile>, String> {
    let path = library_file(&draft.path, folders)?;
    let now = tagging::read_fields(&path).ok_or("its tags could not be read")?;
    let after: TagMap = draft
        .changes
        .iter()
        .filter(|(field, value)| differs(&now, **field, value))
        .map(|(field, value)| (*field, value.trim().to_owned()))
        .collect();
    if after.is_empty() {
        return Ok(None);
    }
    let before = after
        .keys()
        .map(|field| (*field, now.get(field).cloned().unwrap_or_default()))
        .collect();
    tagging::write_fields(&path, &after).map_err(|e| e.to_string())?;
    Ok(Some(SavedFile {
        path: draft.path.clone(),
        path_after: draft.path.clone(),
        before,
        after,
    }))
}

fn differs(now: &TagMap, field: TagField, value: &str) -> bool {
    now.get(&field).map(String::as_str).unwrap_or("") != value.trim()
}

/// Puts `files`, a save's, back as they were. Each file put back is given as the undo made it:
/// from where it was to where it went, with what was written.
pub fn undo_files(files: &[SavedFile], folders: &[PathBuf]) -> Written {
    let mut written = Written::default();
    for file in files {
        match undo_file(file, folders) {
            Ok(undone) => written.saved.push(undone),
            Err(why) => written.failed.push(SaveFailure {
                path: file.path_after.clone(),
                why,
            }),
        }
    }
    written
}

fn undo_file(file: &SavedFile, folders: &[PathBuf]) -> Result<SavedFile, String> {
    let path = library_file(&file.path_after, folders)?;
    if !file.before.is_empty() {
        tagging::write_fields(&path, &file.before).map_err(|e| e.to_string())?;
    }
    Ok(SavedFile {
        path: file.path_after.clone(),
        path_after: file.path.clone(),
        before: TagMap::new(),
        after: file.before.clone(),
    })
}

/// The files listed, as their tags are on disk, with `drafts`: a CUE sheet's audio file has
/// its tracks in the sheet, so it cannot be tagged as a file.
pub fn files_as_they_are(
    listed: Vec<(PathBuf, Option<Duration>, bool)>,
    drafts: Vec<TagDraft>,
) -> Vec<OrganizeFile> {
    let mut drafts: HashMap<PathBuf, TagMap> = drafts
        .into_iter()
        .map(|draft| (draft.path, draft.changes))
        .collect();
    listed
        .into_iter()
        .map(|(path, duration, parts)| {
            let not_taggable = if parts {
                Some("its tracks are in a CUE sheet".to_owned())
            } else {
                tagging::why_not_taggable(&path)
            };
            OrganizeFile {
                fields: tagging::read_fields(&path).unwrap_or_default(),
                draft: drafts.remove(&path).unwrap_or_default(),
                path,
                duration,
                not_taggable,
            }
        })
        .collect()
}

/// What to find a folder's release by: what its files and its name say.
pub fn folder_to_match(folder: &Path, files: &[OrganizeFile]) -> FolderToMatch {
    let taggable: Vec<&OrganizeFile> = files.iter().filter(|f| f.not_taggable.is_none()).collect();
    let most_said = |field: TagField| -> Option<String> {
        let mut counts: HashMap<&str, u32> = HashMap::new();
        for file in &taggable {
            if let Some(value) = file.fields.get(&field) {
                *counts.entry(value.as_str()).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .max_by_key(|(value, count)| (*count, std::cmp::Reverse(*value)))
            .map(|(value, _)| value.to_owned())
    };
    let name = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parsed = parse_folder_name(&name);
    let (album, edition) = album_and_edition(most_said(TagField::Album).as_deref(), &parsed);
    let number = |file: &OrganizeFile, field| {
        file.fields
            .get(&field)
            .and_then(|value: &String| value.parse().ok())
    };
    let artist = most_said(TagField::AlbumArtist)
        .or_else(|| most_said(TagField::Artist))
        .or_else(|| {
            folder
                .parent()
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
        });
    FolderToMatch {
        artist,
        album,
        edition,
        release_id: most_said(TagField::ReleaseId),
        files: taggable
            .iter()
            .map(|file| FileToMatch {
                path: file.path.clone(),
                title: file.fields.get(&TagField::Title).cloned(),
                track: number(file, TagField::Track),
                disc: number(file, TagField::Disc),
                duration: file.duration,
            })
            .collect(),
    }
}

/// Adds the release group's cover beside the music of `saved`, when they became a release and
/// their folder has no picture. The cover written, if one was.
pub fn add_cover(saved: &[SavedFile]) -> Option<PathBuf> {
    let group = saved
        .iter()
        .find_map(|file| file.after.get(&TagField::ReleaseGroupId))?;
    let folder = album_folder(&saved.first()?.path_after);
    let has_picture = std::fs::read_dir(&folder).ok()?.flatten().any(|entry| {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        name.ends_with(".jpg") || name.ends_with(".jpeg") || name.ends_with(".png")
    });
    if has_picture {
        return None;
    }
    let picture = match cover_art(group) {
        Ok(picture) => picture?,
        Err(e) => {
            log::info!("no cover for {}: {e}", folder.display());
            return None;
        }
    };
    let path = folder.join("cover.jpg");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .ok()?;
    std::io::Write::write_all(&mut file, &picture).ok()?;
    Some(path)
}

/// MusicBrainz, asked no faster than it wants to be.
#[derive(Default)]
pub struct MusicBrainz {
    last: Mutex<Option<Instant>>,
}

impl MusicBrainz {
    fn wait(&self) {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(wait) = last.and_then(|at| MUSICBRAINZ_INTERVAL.checked_sub(at.elapsed())) {
            std::thread::sleep(wait);
        }
        *last = Some(Instant::now());
    }
}

impl Source for MusicBrainz {
    fn search(&self, query: &str) -> Result<Vec<Found>, String> {
        self.wait();
        Ok(identify::parse_search(&musicbrainz_release_search(query)?))
    }

    fn release(&self, id: &str) -> Result<Release, String> {
        self.wait();
        identify::parse_release(&musicbrainz_release(id)?)
            .ok_or_else(|| format!("MusicBrainz said nothing of {id}"))
    }
}

/// The file at `path`, when it is music within `folders`: a client names it.
fn library_file(path: &Path, folders: &[PathBuf]) -> Result<PathBuf, String> {
    music_file(path, folders).ok_or_else(|| "it is not in the library's folders".to_owned())
}

/// The folders holding `files`.
pub fn parents(files: &[SavedFile]) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = files
        .iter()
        .flat_map(|file| [file.path.parent(), file.path_after.parent()])
        .flatten()
        .map(Path::to_path_buf)
        .collect();
    folders.sort();
    folders.dedup();
    folders
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::SystemTime;

    use freya_library::Library;

    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("freya-organize-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    fn fixture(to: &Path, name: &str) -> PathBuf {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../freya-decode/tests/fixtures")
            .join(name);
        let path = to.join(name);
        fs::copy(source, &path).unwrap();
        path
    }

    fn draft(path: &Path, title: &str) -> TagDraft {
        TagDraft {
            path: path.to_path_buf(),
            changes: TagMap::from([(TagField::Title, title.to_owned())]),
        }
    }

    /// Asks MusicBrainz: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn finds_a_real_release() {
        let source = MusicBrainz::default();
        let folder = FolderToMatch {
            artist: Some("Metallica".to_owned()),
            album: Some("Master of Puppets".to_owned()),
            files: [
                ("Battery", 313),
                ("Master of Puppets", 516),
                ("The Thing That Should Not Be", 397),
                ("Welcome Home (Sanitarium)", 387),
                ("Disposable Heroes", 497),
                ("Leper Messiah", 340),
                ("Orion", 508),
                ("Damage, Inc.", 332),
            ]
            .iter()
            .enumerate()
            .map(|(at, (title, seconds))| FileToMatch {
                path: PathBuf::from(format!("/m/{at}.flac")),
                title: Some((*title).to_owned()),
                track: Some(at as u32 + 1),
                disc: None,
                duration: Some(Duration::from_secs(*seconds)),
            })
            .collect(),
            ..FolderToMatch::default()
        };
        let found = identify::identify(&folder, &source).unwrap();
        println!("{:?} {:#?}", found.state, found.candidates);
        assert_eq!(found.drafts.len(), 8);
        assert_eq!(found.drafts[0].changes[&TagField::AlbumArtist], "Metallica");
        assert!(found.drafts[0].changes.contains_key(&TagField::RecordingId));
    }

    #[test]
    fn a_save_is_written_and_undone() {
        let dir = folder("undo");
        let path = fixture(&dir, "sine.flac");
        let was = tagging::read_fields(&path).unwrap();
        let folders = [dir.clone()];
        let mut library = Library::open_in_memory().unwrap();
        let drafts = [draft(&path, "Battery")];
        library.set_tag_drafts(&drafts).unwrap();

        let written = write_drafts(&drafts, &folders);
        assert_eq!(written.saved.len(), 1, "{:?}", written.failed);
        let save = library
            .note_tag_save(&dir, &written.saved, SystemTime::now())
            .unwrap();
        assert_eq!(
            tagging::read_fields(&path).unwrap().get(&TagField::Title),
            Some(&"Battery".to_owned())
        );
        assert!(
            library
                .tag_drafts(std::slice::from_ref(&path))
                .unwrap()
                .is_empty()
        );

        let files = library.tag_save_files(save).unwrap();
        let undone = undo_files(&files, &folders);
        assert!(undone.failed.is_empty());
        library.note_tag_undo(save, &undone.saved).unwrap();
        assert_eq!(tagging::read_fields(&path).unwrap(), was);
        assert!(library.tag_saves(10).unwrap()[0].undone);
        assert!(
            library.tag_save_files(save).unwrap().is_empty(),
            "only once"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_draft_that_changes_nothing_writes_nothing() {
        let dir = folder("unchanged");
        let path = fixture(&dir, "sine.mp3");
        let written = write_drafts(&[draft(&path, "Sine Test")], std::slice::from_ref(&dir));
        assert!(written.saved.is_empty());
        assert_eq!(written.unchanged, [path]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_outside_the_librarys_folders_is_not_written() {
        let inside = folder("inside");
        let outside = folder("outside");
        let path = fixture(&outside, "sine.flac");
        let before = fs::read(&path).unwrap();
        let written = write_drafts(&[draft(&path, "Mine now")], std::slice::from_ref(&inside));
        assert_eq!(written.failed.len(), 1);
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::remove_dir_all(inside).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn one_failure_leaves_the_others_written() {
        let dir = folder("partly");
        let good = fixture(&dir, "sine.flac");
        let bad = dir.join("bad.mp3");
        fs::write(&bad, b"not music").unwrap();
        let written = write_drafts(
            &[draft(&bad, "A"), draft(&good, "B")],
            std::slice::from_ref(&dir),
        );
        assert_eq!((written.saved.len(), written.failed.len()), (1, 1));
        fs::remove_dir_all(dir).unwrap();
    }
}
