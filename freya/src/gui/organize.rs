//! Organize: an admin's view of the music's folders, asked of the server on the errands'
//! thread, the window not waiting for it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use freya_client::Http;
use freya_protocol::{
    FolderProposal, FolderState, LibraryData, OrganizeFile, OrganizeFolder, OrganizeStatus,
    PlannedMove, Remote, SUGGESTED_PATTERN, SaveOutcome, ServerControl, TagDraft, TagField, TagMap,
    TagSave, Track,
};
use slint_interpreter::{Struct, Value};

use super::skin_state;

/// How often the folders are asked again while the view is shown: a scan changes them.
const ASK_EVERY: Duration = Duration::from_secs(5);
/// The saves looked through for one of the folder shown.
const SAVES_LOOKED_AT: usize = 50;
/// The fields of a folder's files that are the album's, shown when all of them are chosen.
const ALBUM_FIELDS: [TagField; 7] = [
    TagField::Album,
    TagField::AlbumArtist,
    TagField::Edition,
    TagField::Genre,
    TagField::Date,
    TagField::OriginalDate,
    TagField::DiscTotal,
];
/// The fields of a single file, shown before the album's.
const FILE_FIELDS: [TagField; 4] = [
    TagField::Title,
    TagField::Artist,
    TagField::Track,
    TagField::Disc,
];

type Remoted = Option<Arc<Remote<Http>>>;

/// What came back from the server.
enum Answer {
    Folders(Result<Vec<OrganizeFolder>, String>),
    Files(PathBuf, Result<(Vec<OrganizeFile>, Vec<Track>), String>),
    Saves(Vec<TagSave>),
    Proposal(PathBuf, Option<FolderProposal>),
    Status(OrganizeStatus),
    Pattern(String),
    Moves(PathBuf, Result<Vec<PlannedMove>, String>),
    Saved(Result<SaveOutcome, String>, &'static str),
    Failed(String),
}

type Answers = Arc<Mutex<Vec<Answer>>>;

/// What the view shows now.
pub struct Shown {
    pub rows: Vec<Value>,
    pub candidates: Vec<Value>,
    pub moves: Vec<Value>,
    pub files: Vec<Value>,
    pub fields: Vec<Value>,
    pub values: Vec<(&'static str, Value)>,
}

pub struct OrganizeState {
    folders: Vec<OrganizeFolder>,
    filter: String,
    search: String,
    selected: Option<PathBuf>,
    /// The one file chosen; all the folder's when none is.
    file: Option<PathBuf>,
    files: Vec<OrganizeFile>,
    tracks: Vec<Track>,
    saves: Vec<TagSave>,
    proposal: Option<FolderProposal>,
    status: OrganizeStatus,
    /// Where files belong by their tags, once the server said.
    pattern: Option<String>,
    moves: Vec<PlannedMove>,
    message: String,
    /// Things asked of the server that have not been answered.
    waiting: u32,
    asked: Option<Instant>,
    answers: Answers,
}

impl Default for OrganizeState {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            filter: "all".to_owned(),
            search: String::new(),
            selected: None,
            file: None,
            files: Vec::new(),
            tracks: Vec::new(),
            saves: Vec::new(),
            proposal: None,
            status: OrganizeStatus::default(),
            pattern: None,
            moves: Vec::new(),
            message: String::new(),
            waiting: 0,
            asked: None,
            answers: Answers::default(),
        }
    }
}

/// Runs `ask` on the errands' thread, keeping what it answers.
fn errand(answers: &Answers, ask: impl FnOnce() -> Answer + Send + 'static) {
    let answers = Arc::clone(answers);
    crate::errands::run(move || {
        let answer = ask();
        answers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(answer);
    });
}

fn state_name(state: FolderState) -> &'static str {
    match state {
        FolderState::Unidentified => "unidentified",
        FolderState::Review => "review",
        FolderState::Ready => "ready",
        FolderState::Identified => "identified",
    }
}

fn structure(fields: Vec<(&str, Value)>) -> Value {
    Value::Struct(Struct::from_iter(
        fields
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value)),
    ))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn length(duration: Option<Duration>) -> String {
    let Some(duration) = duration else {
        return String::new();
    };
    let seconds = duration.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

impl OrganizeState {
    /// Takes in what the server answered, and asks the folders again when it is time to.
    pub fn poll(&mut self, remote: Remoted) {
        let answers: Vec<Answer> =
            std::mem::take(&mut *self.answers.lock().unwrap_or_else(|e| e.into_inner()));
        for answer in answers {
            self.waiting = self.waiting.saturating_sub(1);
            self.take(answer, &remote);
        }
        if self.pattern.is_none()
            && self.asked.is_none()
            && let Some(asking) = remote.clone()
        {
            self.waiting += 1;
            errand(&self.answers, move || match asking.settings() {
                Ok(settings) => Answer::Pattern(settings.naming_pattern),
                Err(e) => Answer::Failed(e.to_string()),
            });
        }
        if self.asked.is_none_or(|at| at.elapsed() >= ASK_EVERY) {
            self.asked = Some(Instant::now());
            self.ask_folders(&remote);
            if let Some(remote) = remote {
                self.waiting += 1;
                errand(&self.answers, move || match remote.organize_status() {
                    Ok(status) => Answer::Status(status),
                    Err(e) => Answer::Failed(e.to_string()),
                });
            }
        }
    }

    fn take(&mut self, answer: Answer, remote: &Remoted) {
        match answer {
            Answer::Folders(Ok(folders)) => self.folders = folders,
            Answer::Folders(Err(e)) | Answer::Failed(e) => self.message = e,
            Answer::Files(folder, Ok((files, tracks))) => {
                if self.selected.as_deref() == Some(folder.as_path()) {
                    self.files = files;
                    self.tracks = tracks;
                }
            }
            Answer::Files(_, Err(e)) => self.message = e,
            Answer::Saves(saves) => self.saves = saves,
            Answer::Proposal(folder, proposal) => {
                if self.selected.as_deref() == Some(folder.as_path()) {
                    self.proposal = proposal;
                }
            }
            Answer::Pattern(pattern) => {
                self.pattern = Some(pattern);
                self.ask_moves(remote);
            }
            Answer::Moves(folder, moves) => {
                if self.selected.as_deref() == Some(folder.as_path()) {
                    match moves {
                        Ok(moves) => self.moves = moves,
                        Err(e) => {
                            self.moves.clear();
                            self.message = e;
                        }
                    }
                }
            }
            Answer::Status(status) => {
                // When it comes to an end, or moves on, what is shown may have changed.
                let moved = status.done != self.status.done;
                self.status = status;
                if moved {
                    self.reload(remote);
                }
            }
            Answer::Saved(Ok(outcome), done) => {
                self.message = said(&outcome, done);
                self.reload(remote);
            }
            Answer::Saved(Err(e), _) => self.message = e,
        }
    }

    /// Asks where the files of the folder chosen would go.
    fn ask_moves(&mut self, remote: &Remoted) {
        let pattern = self.pattern.clone().unwrap_or_default();
        let (Some(folder), Some(remote), false) =
            (self.selected.clone(), remote.clone(), pattern.is_empty())
        else {
            self.moves.clear();
            return;
        };
        self.waiting += 1;
        errand(&self.answers, move || {
            let moves = remote.plan_moves(&folder, &pattern);
            Answer::Moves(folder, moves.map_err(|e| e.to_string()))
        });
    }

    fn set_pattern(&mut self, remote: &Remoted, pattern: &str) {
        let pattern = pattern.trim().to_owned();
        self.pattern = Some(pattern.clone());
        self.ask_moves(remote);
        let Some(remote) = remote.clone() else { return };
        self.waiting += 1;
        errand(&self.answers, move || {
            let kept = remote.settings().and_then(|mut settings| {
                settings.naming_pattern = pattern.clone();
                remote.set_settings(&settings)
            });
            match kept {
                Ok(()) => Answer::Pattern(pattern),
                Err(e) => Answer::Failed(e.to_string()),
            }
        });
    }

    fn ask_folders(&mut self, remote: &Remoted) {
        let Some(remote) = remote.clone() else { return };
        self.waiting += 1;
        errand(&self.answers, move || {
            Answer::Folders(remote.organize_folders().map_err(|e| e.to_string()))
        });
    }

    /// Asks again for the folder chosen, its files and the saves that can be undone.
    fn reload(&mut self, remote: &Remoted) {
        self.ask_folders(remote);
        self.ask_moves(remote);
        let Some(remote) = remote.clone() else { return };
        if let Some(folder) = self.selected.clone() {
            let of = Arc::clone(&remote);
            let asked = folder.clone();
            self.waiting += 1;
            errand(&self.answers, move || match of.folder_proposal(&asked) {
                Ok(proposal) => Answer::Proposal(asked, proposal),
                Err(e) => Answer::Failed(e.to_string()),
            });
            let from = Arc::clone(&remote);
            self.waiting += 1;
            errand(&self.answers, move || {
                let found = from.organize_files(&folder).and_then(|files| {
                    let tracks = from.tracks_under(std::slice::from_ref(&folder))?;
                    Ok((files, tracks))
                });
                Answer::Files(folder, found.map_err(|e| e.to_string()))
            });
        }
        self.waiting += 1;
        errand(&self.answers, move || {
            match remote.tag_saves(SAVES_LOOKED_AT) {
                Ok(saves) => Answer::Saves(saves),
                Err(e) => Answer::Failed(e.to_string()),
            }
        });
    }

    /// Does what the skin asked; the tracks to play and which first, when that is it.
    pub fn act(
        &mut self,
        remote: Remoted,
        action: &str,
        value: &str,
    ) -> Option<(Vec<Track>, usize)> {
        match action {
            "filter" => self.filter = value.to_owned(),
            "search" => self.search = value.to_lowercase(),
            "select" => {
                self.selected = Some(PathBuf::from(value));
                self.file = None;
                self.files.clear();
                self.tracks.clear();
                self.proposal = None;
                self.message.clear();
                self.reload(&remote);
            }
            "select-file" => {
                self.file = (!value.is_empty()).then(|| PathBuf::from(value));
            }
            "play-file" => {
                let start = self
                    .tracks
                    .iter()
                    .position(|track| track.path == Path::new(value))?;
                return Some((self.tracks.clone(), start));
            }
            "save" => self.save(&remote),
            "discard" => self.discard(&remote),
            "undo" => self.undo(&remote),
            "identify" => {
                if let Some(folder) = self.selected.clone() {
                    self.message = "Asking MusicBrainz…".to_owned();
                    self.command(&remote, move |r| r.identify_folders(&[folder]));
                }
            }
            "identify-all" => self.command(&remote, |r| r.identify_folders(&[])),
            "save-ready" => self.command(&remote, |r| r.save_ready_folders()),
            "stop" => self.command(&remote, |r| r.stop_organizing()),
            "set-pattern" => self.set_pattern(&remote, value),
            "suggest-pattern" => self.set_pattern(&remote, SUGGESTED_PATTERN),
            "move" => {
                let pattern = self.pattern.clone().unwrap_or_default();
                if let Some(folder) = self.selected.clone() {
                    self.command(&remote, move |r| r.move_folders(&[folder], &pattern));
                }
            }
            "move-all" => {
                let pattern = self.pattern.clone().unwrap_or_default();
                let folders: Vec<PathBuf> = self
                    .folders
                    .iter()
                    .filter(|f| f.state == FolderState::Identified && !f.mixed)
                    .map(|f| f.path.clone())
                    .collect();
                self.command(&remote, move |r| r.move_folders(&folders, &pattern));
            }
            "choose" => {
                if let Some(folder) = self.selected.clone() {
                    let release = value.to_owned();
                    let asking = remote.clone()?;
                    self.waiting += 1;
                    errand(&self.answers, move || {
                        match asking.choose_release(&folder, &release) {
                            Ok(()) => Answer::Saved(Ok(SaveOutcome::default()), "Chose it for"),
                            Err(e) => Answer::Failed(e.to_string()),
                        }
                    });
                }
            }
            _ => {}
        }
        None
    }

    /// Asks the server to do something it says nothing back about but how it goes.
    fn command(
        &mut self,
        remote: &Remoted,
        ask: impl FnOnce(&Remote<Http>) -> freya_protocol::ApiResult<()> + Send + 'static,
    ) {
        let Some(remote) = remote.clone() else { return };
        self.asked = None;
        self.waiting += 1;
        errand(&self.answers, move || match ask(&remote) {
            Ok(()) => Answer::Status(OrganizeStatus::default()),
            Err(e) => Answer::Failed(e.to_string()),
        });
    }

    fn drafted_paths(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter(|file| !file.draft.is_empty())
            .map(|file| file.path.clone())
            .collect()
    }

    fn save(&mut self, remote: &Remoted) {
        let paths = self.drafted_paths();
        let Some(remote) = remote.clone().filter(|_| !paths.is_empty()) else {
            return;
        };
        self.message = "Saving…".to_owned();
        self.waiting += 1;
        errand(&self.answers, move || {
            Answer::Saved(
                remote.save_tag_drafts(&paths).map_err(|e| e.to_string()),
                "Saved",
            )
        });
    }

    fn discard(&mut self, remote: &Remoted) {
        let drafts: Vec<TagDraft> = self
            .drafted_paths()
            .into_iter()
            .map(|path| TagDraft {
                path,
                changes: TagMap::new(),
            })
            .collect();
        for file in &mut self.files {
            file.draft.clear();
        }
        self.send_drafts(remote, drafts);
    }

    /// The latest save of the folder chosen that can still be undone.
    fn undoable(&self) -> Option<&TagSave> {
        let folder = self.selected.as_deref()?;
        self.saves
            .iter()
            .find(|save| !save.undone && save.folder.starts_with(folder))
    }

    fn undo(&mut self, remote: &Remoted) {
        let Some(save) = self.undoable().map(|save| save.id) else {
            return;
        };
        let Some(remote) = remote.clone() else { return };
        self.message = "Undoing…".to_owned();
        self.waiting += 1;
        errand(&self.answers, move || {
            Answer::Saved(
                remote.undo_tag_save(save).map_err(|e| e.to_string()),
                "Undid",
            )
        });
    }

    fn send_drafts(&mut self, remote: &Remoted, drafts: Vec<TagDraft>) {
        let Some(remote) = remote.clone().filter(|_| !drafts.is_empty()) else {
            return;
        };
        self.waiting += 1;
        errand(&self.answers, move || {
            match remote.set_tag_drafts(&drafts) {
                Ok(()) => Answer::Folders(remote.organize_folders().map_err(|e| e.to_string())),
                Err(e) => Answer::Failed(e.to_string()),
            }
        });
    }

    /// The files `fields` is of: those of the folder that can be tagged, or the one chosen.
    fn scope(&self) -> impl Iterator<Item = &OrganizeFile> {
        self.files.iter().filter(|file| {
            file.not_taggable.is_none()
                && self.file.as_ref().is_none_or(|chosen| *chosen == file.path)
        })
    }

    /// Has `field` of what is chosen become `value`: drafted where that is a change.
    pub fn edit_field(&mut self, remote: Remoted, field: &str, value: &str) {
        let Some(field) = TagField::from_name(field) else {
            return;
        };
        let value = value.trim().to_owned();
        let chosen = self.file.clone();
        let mut drafts = Vec::new();
        for file in &mut self.files {
            let in_scope =
                file.not_taggable.is_none() && chosen.as_ref().is_none_or(|one| *one == file.path);
            if !in_scope {
                continue;
            }
            let now = file.fields.get(&field).map(String::as_str).unwrap_or("");
            let before = file.draft.clone();
            if now == value {
                file.draft.remove(&field);
            } else {
                file.draft.insert(field, value.clone());
            }
            if file.draft != before {
                drafts.push(TagDraft {
                    path: file.path.clone(),
                    changes: file.draft.clone(),
                });
            }
        }
        self.send_drafts(&remote, drafts);
    }

    fn row_matches(&self, folder: &OrganizeFolder) -> bool {
        let wanted = self.filter == "all" || self.filter == state_name(folder.state);
        wanted
            && (self.search.is_empty()
                || folder.name.to_lowercase().contains(&self.search)
                || folder.group.to_lowercase().contains(&self.search))
    }

    fn rows(&self) -> Vec<Value> {
        let mut rows = Vec::new();
        let mut group = None;
        for folder in self.folders.iter().filter(|f| self.row_matches(f)) {
            if group != Some(folder.group.as_str()) {
                group = Some(folder.group.as_str());
                rows.push(row("group", &folder.group, "", "", "", false));
            }
            let mut detail = plural(folder.files as usize, "file", "files");
            if folder.mixed {
                detail.push_str(" · mixed");
            }
            rows.push(row(
                "folder",
                &folder.name,
                &detail,
                state_name(folder.state),
                &folder.path.to_string_lossy(),
                self.selected.as_deref() == Some(folder.path.as_path()),
            ));
        }
        rows
    }

    fn move_rows(&self) -> Vec<Value> {
        self.moves
            .iter()
            .map(|one| {
                let from = file_name(&one.from);
                let to = one
                    .to
                    .components()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<PathBuf>();
                row(
                    "move",
                    &to.to_string_lossy(),
                    &from,
                    one.conflict.as_deref().unwrap_or(""),
                    "",
                    false,
                )
            })
            .collect()
    }

    fn candidate_rows(&self) -> Vec<Value> {
        let Some(proposal) = &self.proposal else {
            return Vec::new();
        };
        proposal
            .candidates
            .iter()
            .map(|release| {
                let mut facts = vec![plural(release.tracks as usize, "track", "tracks")];
                for fact in [
                    &release.date,
                    &release.country,
                    &release.format,
                    &release.label,
                    &release.disambiguation,
                ] {
                    if !fact.is_empty() {
                        facts.push(fact.clone());
                    }
                }
                row(
                    "candidate",
                    &format!("{} — {}", release.artist, release.title),
                    &facts.join(" · "),
                    &format!("{:.0}%", release.score * 100.0),
                    &release.release_id,
                    proposal.chosen.as_deref() == Some(release.release_id.as_str()),
                )
            })
            .collect()
    }

    /// What the server is going through, in words.
    fn status_text(&self) -> String {
        let total = self.status.waiting + self.status.done;
        let mut text = if self.status.waiting > 0 {
            format!(
                "Going through folder {} of {total}",
                (self.status.done + 1).min(total)
            )
        } else {
            String::new()
        };
        if let Some(error) = &self.status.error {
            if !text.is_empty() {
                text.push_str(" · ");
            }
            text.push_str(error);
        }
        text
    }

    fn file_rows(&self) -> Vec<Value> {
        self.files
            .iter()
            .map(|file| {
                let get = |field| file.fields.get(&field).cloned().unwrap_or_default();
                let number = match (get(TagField::Disc), get(TagField::Track)) {
                    (disc, track) if disc.is_empty() || disc == "1" => track,
                    (disc, track) => format!("{disc}-{track}"),
                };
                let title = get(TagField::Title);
                let new_title = file
                    .draft
                    .get(&TagField::Title)
                    .cloned()
                    .unwrap_or_else(|| title.clone());
                structure(vec![
                    ("key", skin_state::string(file.path.to_string_lossy())),
                    ("number", skin_state::string(number)),
                    ("title", skin_state::string(title)),
                    ("new-title", skin_state::string(new_title)),
                    ("file", skin_state::string(file_name(&file.path))),
                    ("length", skin_state::string(length(file.duration))),
                    ("changed", Value::Bool(!file.draft.is_empty())),
                    (
                        "selected",
                        Value::Bool(self.file.as_ref() == Some(&file.path)),
                    ),
                    (
                        "note",
                        skin_state::string(file.not_taggable.clone().unwrap_or_default()),
                    ),
                ])
            })
            .collect()
    }

    fn field_rows(&self) -> Vec<Value> {
        let single = self.file.is_some();
        let fields = FILE_FIELDS
            .iter()
            .filter(|_| single)
            .chain(ALBUM_FIELDS.iter());
        fields
            .map(|field| {
                let mut currents = BTreeMap::new();
                let mut drafts = BTreeMap::new();
                for file in self.scope() {
                    let now = file.fields.get(field).cloned().unwrap_or_default();
                    let draft = file
                        .draft
                        .get(field)
                        .cloned()
                        .unwrap_or_else(|| now.clone());
                    *currents.entry(now).or_insert(0) += 1;
                    *drafts.entry(draft).or_insert(0) += 1;
                }
                let one = |values: &BTreeMap<String, i32>| {
                    (values.len() == 1)
                        .then(|| values.keys().next().cloned())
                        .flatten()
                };
                let several = currents.len() > 1;
                let draft = one(&drafts);
                let changed = self.scope().any(|file| file.draft.contains_key(field));
                structure(vec![
                    ("field", skin_state::string(field.name())),
                    ("label", skin_state::string(field.label())),
                    (
                        "current",
                        skin_state::string(one(&currents).unwrap_or_default()),
                    ),
                    ("draft", skin_state::string(draft.unwrap_or_default())),
                    ("changed", Value::Bool(changed)),
                    ("several", Value::Bool(several)),
                ])
            })
            .collect()
    }

    fn summary(&self) -> String {
        let count = |state| self.folders.iter().filter(|f| f.state == state).count();
        let mut parts = vec![plural(self.folders.len(), "folder", "folders")];
        for (state, words) in [
            (FolderState::Unidentified, "not identified"),
            (FolderState::Review, "to review"),
            (FolderState::Ready, "ready to save"),
        ] {
            let count = count(state);
            if count > 0 {
                parts.push(format!("{count} {words}"));
            }
        }
        parts.join(" · ")
    }

    pub fn shown(&self) -> Shown {
        let folder = self
            .selected
            .as_ref()
            .and_then(|path| self.folders.iter().find(|f| f.path == *path));
        let title = folder.map(|f| f.name.clone()).unwrap_or_default();
        let detail = folder
            .map(|f| f.path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let taggable = self.scope().count();
        let scope = match &self.file {
            Some(file) => file_name(file),
            None if self.files.len() == taggable => format!("All {taggable} files"),
            None => format!("{taggable} of {} files", self.files.len()),
        };
        let values = vec![
            ("filter", skin_state::string(&self.filter)),
            ("summary", skin_state::string(self.summary())),
            ("title", skin_state::string(title)),
            ("detail", skin_state::string(detail)),
            ("message", skin_state::string(&self.message)),
            ("busy", Value::Bool(self.waiting > 0)),
            (
                "can-save",
                Value::Bool(self.files.iter().any(|file| !file.draft.is_empty())),
            ),
            ("can-undo", Value::Bool(self.undoable().is_some())),
            ("scope", skin_state::string(scope)),
            ("status", skin_state::string(self.status_text())),
            (
                "pattern",
                skin_state::string(self.pattern.clone().unwrap_or_default()),
            ),
        ];
        Shown {
            rows: self.rows(),
            candidates: self.candidate_rows(),
            moves: self.move_rows(),
            files: self.file_rows(),
            fields: self.field_rows(),
            values,
        }
    }
}

fn row(kind: &str, title: &str, detail: &str, state: &str, key: &str, selected: bool) -> Value {
    structure(vec![
        ("kind", skin_state::string(kind)),
        ("title", skin_state::string(title)),
        ("detail", skin_state::string(detail)),
        ("state", skin_state::string(state)),
        ("key", skin_state::string(key)),
        ("selected", Value::Bool(selected)),
    ])
}

/// What a save or an undo came to, in words.
fn said(outcome: &SaveOutcome, done: &str) -> String {
    let files = plural(outcome.saved as usize, "file", "files");
    match outcome.failed.first() {
        None => format!("{done} {files}."),
        Some(first) => format!(
            "{done} {files}; {} could not be written. {}: {}",
            outcome.failed.len(),
            file_name(&first.path),
            first.why
        ),
    }
}

#[cfg(test)]
mod tests {
    use freya_protocol::FolderName;

    use super::*;

    fn folder(group: &str, name: &str, state: FolderState) -> OrganizeFolder {
        OrganizeFolder {
            path: PathBuf::from(format!("/m/{group}/{name}")),
            group: group.to_owned(),
            name: name.to_owned(),
            parsed: FolderName::default(),
            files: 2,
            state,
            mixed: false,
        }
    }

    fn file(path: &str, title: &str, album: &str) -> OrganizeFile {
        OrganizeFile {
            path: PathBuf::from(path),
            fields: TagMap::from([
                (TagField::Title, title.to_owned()),
                (TagField::Album, album.to_owned()),
            ]),
            draft: TagMap::new(),
            duration: Some(Duration::from_secs(61)),
            not_taggable: None,
        }
    }

    fn text(value: &Value, name: &str) -> String {
        let Value::Struct(s) = value else {
            panic!("not a row")
        };
        match s.get_field(name) {
            Some(Value::String(text)) => text.to_string(),
            other => panic!("{name}: {other:?}"),
        }
    }

    fn truth(value: &Value, name: &str) -> bool {
        let Value::Struct(s) = value else {
            panic!("not a row")
        };
        matches!(s.get_field(name), Some(Value::Bool(true)))
    }

    fn state() -> OrganizeState {
        OrganizeState {
            folders: vec![
                folder("Metallica", "Load", FolderState::Unidentified),
                folder("Metallica", "Reload", FolderState::Identified),
                folder("Opeth", "Damnation", FolderState::Ready),
            ],
            ..OrganizeState::default()
        }
    }

    #[test]
    fn folders_are_listed_under_what_they_are_in() {
        let rows = state().rows();
        let kinds: Vec<String> = rows.iter().map(|r| text(r, "title")).collect();
        assert_eq!(kinds, ["Metallica", "Load", "Reload", "Opeth", "Damnation"]);
        assert_eq!(text(&rows[0], "kind"), "group");
        assert_eq!(text(&rows[1], "state"), "unidentified");
    }

    #[test]
    fn a_filter_and_a_search_narrow_the_folders() {
        let mut organize = state();
        organize.filter = "ready".to_owned();
        assert_eq!(organize.rows().len(), 2);
        organize.filter = "all".to_owned();
        organize.search = "metal".to_owned();
        assert_eq!(organize.rows().len(), 3);
    }

    #[test]
    fn a_value_all_the_files_share_is_shown_and_others_are_several() {
        let mut organize = state();
        organize.files = vec![
            file("/m/a.flac", "One", "Album"),
            file("/m/b.flac", "Two", "Other"),
        ];
        let fields = organize.field_rows();
        let album = &fields[0];
        assert_eq!(text(album, "field"), "album");
        assert!(truth(album, "several"));
        organize.file = Some(PathBuf::from("/m/a.flac"));
        let fields = organize.field_rows();
        assert_eq!(text(&fields[0], "current"), "One", "the title comes first");
        assert!(!truth(&fields[0], "several"));
    }

    #[test]
    fn an_edit_drafts_a_change_and_undoing_it_takes_the_draft_away() {
        let mut organize = state();
        organize.files = vec![
            file("/m/a.flac", "One", "Album"),
            file("/m/b.flac", "Two", "Album"),
        ];
        organize.edit_field(None, "album", "New Album");
        assert!(organize.files.iter().all(|f| f.draft.len() == 1));
        assert!(
            organize
                .shown()
                .values
                .iter()
                .any(|(name, value)| *name == "can-save" && *value == Value::Bool(true))
        );
        organize.edit_field(None, "album", "Album");
        assert!(organize.files.iter().all(|f| f.draft.is_empty()));
    }

    #[test]
    fn a_file_that_cannot_be_tagged_gets_no_draft() {
        let mut organize = state();
        let mut cue = file("/m/a.flac", "One", "Album");
        cue.not_taggable = Some("its tracks are in a CUE sheet".to_owned());
        organize.files = vec![cue, file("/m/b.flac", "Two", "Album")];
        organize.edit_field(None, "album", "New");
        assert!(organize.files[0].draft.is_empty());
        assert_eq!(organize.files[1].draft.len(), 1);
    }
}
