//! Writing what was decided about the music's files to them, and putting them back.

use std::path::{Path, PathBuf};

use freya_library::library::organize::SavedFile;
use freya_library::tagging;
use freya_protocol::{SaveFailure, TagDraft, TagField, TagMap};

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
