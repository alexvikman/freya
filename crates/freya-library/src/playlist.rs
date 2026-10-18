//! The playlist: entries, selection, current track and next/previous with shuffle and repeat.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cue::Part;
use crate::tags::TagInfo;

#[derive(Clone, Debug)]
pub struct Entry {
    /// Names this entry in its playlist, unique there and kept through every edit, so a track
    /// that plays can be told from another entry for the same file. Given by
    /// [`Playlist::insert`]; 0 outside a playlist. Not part of what makes entries equal.
    pub id: u64,
    pub path: PathBuf,
    /// The part of the file this entry plays, for a track of a CUE sheet.
    pub part: Option<Part>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
    pub selected: bool,
    /// Whether tags have been read from the file (as opposed to coming from a playlist file).
    pub tags_loaded: bool,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        // Everything but `id`.
        let Self {
            id: _,
            path,
            part,
            title,
            artist,
            album,
            duration,
            selected,
            tags_loaded,
        } = self;
        (
            path,
            part,
            title,
            artist,
            album,
            duration,
            selected,
            tags_loaded,
        ) == (
            &other.path,
            &other.part,
            &other.title,
            &other.artist,
            &other.album,
            &other.duration,
            &other.selected,
            &other.tags_loaded,
        )
    }
}

impl Entry {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            id: 0,
            path: path.into(),
            part: None,
            title: None,
            artist: None,
            album: None,
            duration: None,
            selected: false,
            tags_loaded: false,
        }
    }

    /// An entry whose tags are known already: a library track, or a track of a CUE sheet.
    pub fn with_tags(path: impl Into<PathBuf>, part: Option<Part>, tags: &TagInfo) -> Self {
        let duration = tags
            .duration
            .or_else(|| part.and_then(|p| p.duration(None)));
        Self {
            part,
            title: tags.title.clone(),
            artist: tags.artist.clone(),
            album: tags.album.clone(),
            duration,
            // The last track of a CUE sheet learns its length from the file.
            tags_loaded: part.is_none() || duration.is_some(),
            ..Self::new(path)
        }
    }

    /// Where in the file the entry starts: zero for a whole file.
    pub fn start(&self) -> Duration {
        self.part.map_or(Duration::ZERO, |p| p.start)
    }

    /// Whether this entry plays `path` from `start`.
    pub fn plays(&self, path: &Path, start: Duration) -> bool {
        self.path == path && self.start() == start
    }

    /// "Artist - Title" if known, otherwise the file name without extension.
    pub fn display_title(&self) -> String {
        match (&self.artist, &self.title) {
            (Some(artist), Some(title)) => format!("{artist} - {title}"),
            (None, Some(title)) => title.clone(),
            _ => self
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.path.to_string_lossy().into_owned()),
        }
    }

    /// Takes the tags read from the entry's file.
    pub fn apply_tags(&mut self, tags: &TagInfo) {
        match self.part {
            // The file's tags describe the whole disc. Only its length is of use, for the
            // length of its last track.
            Some(part) => {
                if self.duration.is_none() {
                    self.duration = part.duration(tags.duration);
                }
            }
            None => {
                if tags.title.is_some() {
                    self.title.clone_from(&tags.title);
                    self.artist.clone_from(&tags.artist);
                    self.album.clone_from(&tags.album);
                }
                if tags.duration.is_some() {
                    self.duration = tags.duration;
                }
            }
        }
        self.tags_loaded = true;
    }
}

/// How a click on a row should change the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectMode {
    /// Select only this row.
    Single,
    /// Toggle this row (Ctrl/Cmd-click).
    Toggle,
    /// Select from the last clicked row to this one (Shift-click).
    Range,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Title,
    FileName,
    Path,
}

#[derive(Default)]
pub struct Playlist {
    entries: Vec<Entry>,
    current: Option<usize>,
    /// Row a Shift-click range starts from.
    anchor: Option<usize>,
    /// Row the keyboard acts on: the last one clicked or moved to.
    cursor: Option<usize>,
    /// Play order while shuffling; a permutation of all indices.
    shuffle_order: Vec<usize>,
    rng: u64,
    /// The last [`Entry::id`] given.
    last_id: u64,
    /// Where the current entry was when it was removed: playback goes on from there, not
    /// from the top. Cleared when another entry becomes current.
    removed_at: Option<usize>,
}

impl Playlist {
    pub fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x2545_f491, |d| d.as_nanos() as u64);
        Self::with_seed(seed)
    }

    /// A playlist with a fixed shuffle seed, for reproducible tests.
    pub fn with_seed(seed: u64) -> Self {
        Self {
            rng: seed | 1,
            ..Default::default()
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    pub fn current(&self) -> Option<usize> {
        self.current
    }

    pub fn current_entry(&self) -> Option<&Entry> {
        self.current.and_then(|i| self.entries.get(i))
    }

    /// Where the entry with `id` is now.
    pub fn index_of(&self, id: u64) -> Option<usize> {
        (id != 0)
            .then(|| self.entries.iter().position(|e| e.id == id))
            .flatten()
    }

    pub fn set_current(&mut self, index: Option<usize>) {
        self.removed_at = None;
        self.current = index.filter(|&i| i < self.entries.len());
    }

    /// Makes the first entry that plays `path` from `start` current, preferring the current
    /// one if it matches.
    pub fn set_current_track(&mut self, path: &Path, start: Duration) {
        if self.current_entry().is_some_and(|e| e.plays(path, start)) {
            return;
        }
        if let Some(i) = self.entries.iter().position(|e| e.plays(path, start)) {
            self.current = Some(i);
        }
    }

    /// Inserts entries at `at` (or the end) and returns the index of the first one.
    pub fn insert(&mut self, at: Option<usize>, entries: impl IntoIterator<Item = Entry>) -> usize {
        let at = at.unwrap_or(self.entries.len()).min(self.entries.len());
        let new: Vec<Entry> = entries
            .into_iter()
            .map(|mut entry| {
                self.last_id += 1;
                entry.id = self.last_id;
                entry
            })
            .collect();
        let count = new.len();
        self.entries.splice(at..at, new);
        if let Some(c) = self.current
            && c >= at
        {
            self.current = Some(c + count);
        }
        self.shuffle_order.clear();
        at
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.current = None;
        self.anchor = None;
        self.cursor = None;
        self.shuffle_order.clear();
    }

    /// Keeps only entries for which `keep` returns true, tracking the current entry.
    fn retain(&mut self, mut keep: impl FnMut(&Entry) -> bool) {
        let current = self.current;
        let mut new_current = None;
        // The entry that plays was removed: the next one is the first kept after it.
        let mut removed_at = None;
        let mut index = 0;
        let mut kept = 0;
        self.entries.retain(|e| {
            let k = keep(e);
            if Some(index) == current {
                if k {
                    new_current = Some(kept);
                } else {
                    removed_at = Some(kept);
                }
            }
            if k {
                kept += 1;
            }
            index += 1;
            k
        });
        if removed_at.is_some() {
            self.removed_at = removed_at;
        }
        self.current = new_current;
        self.anchor = None;
        self.cursor = None;
        self.shuffle_order.clear();
    }

    pub fn remove_selected(&mut self) {
        self.retain(|e| !e.selected);
    }

    pub fn crop(&mut self) {
        self.retain(|e| e.selected);
    }

    /// Removes entries whose files no longer exist.
    pub fn remove_missing(&mut self) {
        self.retain(|e| e.path.exists());
    }

    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub fn click(&mut self, index: usize, mode: SelectMode) {
        if index >= self.entries.len() {
            return;
        }
        self.cursor = Some(index);
        match mode {
            SelectMode::Single => {
                for (i, e) in self.entries.iter_mut().enumerate() {
                    e.selected = i == index;
                }
                self.anchor = Some(index);
            }
            SelectMode::Toggle => {
                self.entries[index].selected = !self.entries[index].selected;
                self.anchor = Some(index);
            }
            SelectMode::Range => {
                let anchor = self.anchor.unwrap_or(index);
                let (lo, hi) = (anchor.min(index), anchor.max(index));
                for (i, e) in self.entries.iter_mut().enumerate() {
                    e.selected = (lo..=hi).contains(&i);
                }
            }
        }
    }

    /// Moves the keyboard cursor by `delta` rows and selects it; with `extend`, selects the
    /// range from the anchor instead, like Shift+arrow.
    pub fn move_cursor(&mut self, delta: isize, extend: bool) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() as isize - 1;
        let target = match self.cursor {
            Some(c) => (c as isize + delta).clamp(0, last),
            // With nothing focused, the cursor is just before the top going down, and just
            // past the bottom going up: Down starts at the top, Up at the bottom, and Home
            // and End go to their ends.
            None if delta < 0 => (last + 1 + delta).clamp(0, last),
            None => (delta - 1).clamp(0, last),
        } as usize;
        let mode = if extend {
            SelectMode::Range
        } else {
            SelectMode::Single
        };
        if extend && self.anchor.is_none() {
            self.anchor = self.cursor.or(Some(target));
        }
        self.click(target, mode);
    }

    /// Moves every selected entry, in order, to just before the entry at `to` (counted before
    /// the move; `to == len()` means the end). The current entry and the selection follow.
    pub fn move_selected(&mut self, to: usize) {
        let to = to.min(self.entries.len());
        let current = self.current;
        let entries: Vec<(usize, Entry)> = std::mem::take(&mut self.entries)
            .into_iter()
            .enumerate()
            .collect();
        let (moving, staying): (Vec<_>, Vec<_>) =
            entries.into_iter().partition(|(_, e)| e.selected);
        if moving.is_empty() {
            self.entries = staying.into_iter().map(|(_, e)| e).collect();
            return;
        }
        let insert_at = staying.iter().filter(|(i, _)| *i < to).count();
        let mut ordered = staying;
        let first_moved = insert_at;
        let moved_count = moving.len();
        ordered.splice(insert_at..insert_at, moving);
        self.current = current.and_then(|c| ordered.iter().position(|(i, _)| *i == c));
        self.entries = ordered.into_iter().map(|(_, e)| e).collect();
        self.anchor = Some(first_moved);
        self.cursor = Some(first_moved + moved_count - 1);
        self.shuffle_order.clear();
    }

    pub fn select_all(&mut self) {
        self.entries.iter_mut().for_each(|e| e.selected = true);
    }

    pub fn select_none(&mut self) {
        self.entries.iter_mut().for_each(|e| e.selected = false);
    }

    pub fn invert_selection(&mut self) {
        self.entries
            .iter_mut()
            .for_each(|e| e.selected = !e.selected);
    }

    pub fn selected_indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.selected)
            .map(|(i, _)| i)
    }

    pub fn sort(&mut self, key: SortKey) {
        let current = self.current_entry().map(|e| (e.path.clone(), e.start()));
        let sort_key = |e: &Entry| -> String {
            match key {
                SortKey::Title => e.display_title().to_lowercase(),
                SortKey::FileName => e
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_lowercase())
                    .unwrap_or_default(),
                SortKey::Path => e.path.to_string_lossy().to_lowercase(),
            }
        };
        self.entries.sort_by_cached_key(sort_key);
        self.after_reorder(current);
    }

    pub fn reverse(&mut self) {
        let current = self.current_entry().map(|e| (e.path.clone(), e.start()));
        self.entries.reverse();
        self.after_reorder(current);
    }

    /// Shuffles the list itself (Winamp's "Randomize list").
    pub fn randomize(&mut self) {
        let current = self.current_entry().map(|e| (e.path.clone(), e.start()));
        for i in (1..self.entries.len()).rev() {
            let j = (self.next_random() % (i as u64 + 1)) as usize;
            self.entries.swap(i, j);
        }
        self.after_reorder(current);
    }

    fn after_reorder(&mut self, current: Option<(PathBuf, Duration)>) {
        self.current = current
            .and_then(|(path, start)| self.entries.iter().position(|e| e.plays(&path, start)));
        self.anchor = None;
        self.cursor = None;
        self.shuffle_order.clear();
    }

    /// Applies tags read from a file to every entry with that path.
    pub fn apply_tags(&mut self, path: &Path, tags: &TagInfo) {
        for e in self.entries.iter_mut().filter(|e| e.path == path) {
            e.apply_tags(tags);
        }
    }

    /// Applies the tags of many files in one pass over the entries: one pass per file made
    /// restoring a long Up Next take minutes.
    pub fn apply_all_tags(&mut self, tags: &HashMap<PathBuf, TagInfo>) {
        if tags.is_empty() {
            return;
        }
        for e in &mut self.entries {
            if let Some(tags) = tags.get(&e.path) {
                e.apply_tags(tags);
            }
        }
    }

    /// Total length of the given entries, and whether every length was known.
    fn duration_of<'a>(entries: impl Iterator<Item = &'a Entry>) -> (Duration, bool) {
        entries.fold((Duration::ZERO, true), |(sum, all), e| match e.duration {
            Some(d) => (sum.saturating_add(d), all),
            None => (sum, false),
        })
    }

    pub fn total_duration(&self) -> (Duration, bool) {
        Self::duration_of(self.entries.iter())
    }

    pub fn selected_duration(&self) -> (Duration, bool) {
        Self::duration_of(self.entries.iter().filter(|e| e.selected))
    }

    /// The entry that follows the current one, without changing anything.
    pub fn peek_next(&mut self, shuffle: bool, repeat: bool) -> Option<usize> {
        self.step(1, shuffle, repeat)
    }

    pub fn peek_previous(&mut self, shuffle: bool, repeat: bool) -> Option<usize> {
        self.step(-1, shuffle, repeat)
    }

    fn step(&mut self, delta: isize, shuffle: bool, repeat: bool) -> Option<usize> {
        let len = self.entries.len();
        if len == 0 {
            return None;
        }
        let Some(current) = self.current else {
            // Where a removed current entry was: the first kept after it comes next.
            if let Some(at) = self.removed_at.filter(|_| delta > 0 && !shuffle) {
                return if at < len {
                    Some(at)
                } else if repeat {
                    Some(0)
                } else {
                    None
                };
            }
            return Some(if shuffle { self.shuffled()[0] } else { 0 });
        };
        if shuffle {
            let order = self.shuffled().to_vec();
            let pos = order.iter().position(|&i| i == current).unwrap_or(0) as isize;
            let target = pos + delta;
            if (0..len as isize).contains(&target) {
                return Some(order[target as usize]);
            }
            if !repeat {
                return None;
            }
            // A fresh random order for the next round, which starts from the current entry:
            // asked again, it gives the same next one.
            self.shuffle_order.clear();
            let order = self.shuffled().to_vec();
            return order.get(1).or(order.first()).copied();
        }
        let target = current as isize + delta;
        if (0..len as isize).contains(&target) {
            Some(target as usize)
        } else if repeat {
            Some(target.rem_euclid(len as isize) as usize)
        } else {
            None
        }
    }

    /// The play order while shuffling. A new one, made after an edit or for a new round,
    /// starts with the current entry, so that everything else follows it: placed at random,
    /// the current entry would cut off the part of the order before it.
    fn shuffled(&mut self) -> &[usize] {
        if self.shuffle_order.len() != self.entries.len() {
            let mut order: Vec<usize> = (0..self.entries.len()).collect();
            for i in (1..order.len()).rev() {
                let j = (self.next_random() % (i as u64 + 1)) as usize;
                order.swap(i, j);
            }
            if let Some(at) = self
                .current
                .and_then(|c| order.iter().position(|&i| i == c))
            {
                order.swap(0, at);
            }
            self.shuffle_order = order;
        }
        &self.shuffle_order
    }

    fn next_random(&mut self) -> u64 {
        // xorshift64
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(n: usize) -> Playlist {
        let mut p = Playlist::with_seed(42);
        p.insert(None, (0..n).map(|i| Entry::new(format!("/music/{i}.mp3"))));
        p
    }

    #[test]
    fn next_and_previous_with_repeat() {
        let mut p = list(3);
        assert_eq!(p.peek_next(false, false), Some(0));
        p.set_current(Some(2));
        assert_eq!(p.peek_next(false, false), None);
        assert_eq!(p.peek_next(false, true), Some(0));
        p.set_current(Some(0));
        assert_eq!(p.peek_previous(false, true), Some(2));
    }

    #[test]
    fn shuffle_visits_every_entry_once() {
        let mut p = list(10);
        let mut seen = vec![];
        let first = p.peek_next(true, false);
        p.set_current(first);
        while let Some(i) = p.current() {
            seen.push(i);
            let next = p.peek_next(true, false);
            p.set_current(next);
        }
        seen.sort();
        assert_eq!(seen, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn shuffle_from_a_chosen_track_plays_all_the_rest() {
        // Started at entry 0 (Play on an album), a new order must not put it in the middle:
        // only what came after it would play.
        for seed in 1..50 {
            let mut p = list(10);
            p.rng = seed;
            p.set_current(Some(0));
            let mut seen = vec![0];
            while let Some(next) = p.peek_next(true, false) {
                p.set_current(Some(next));
                seen.push(next);
            }
            seen.sort();
            assert_eq!(seen, (0..10).collect::<Vec<_>>(), "seed {seed}");
        }
    }

    #[test]
    fn repeat_round_boundary_is_stable() {
        let mut p = list(5);
        p.set_current(Some(0));
        let mut last = 0;
        while let Some(next) = p.peek_next(true, false) {
            p.set_current(Some(next));
            last = next;
        }
        p.set_current(Some(last));
        let first = p.peek_next(true, true);
        assert_eq!(
            p.peek_next(true, true),
            first,
            "asking again changes nothing"
        );
        assert_ne!(first, Some(last));
    }

    #[test]
    fn removing_the_playing_entry_goes_on_after_it() {
        let mut p = list(5);
        p.set_current(Some(2));
        p.click(2, SelectMode::Single);
        p.remove_selected();
        assert_eq!(p.current(), None);
        let next = p.peek_next(false, false).unwrap();
        assert_eq!(p.get(next).unwrap().path, Path::new("/music/3.mp3"));

        // With rows before it removed too, it is still the one after it.
        let mut p = list(6);
        p.set_current(Some(3));
        p.click(0, SelectMode::Single);
        p.click(1, SelectMode::Toggle);
        p.click(3, SelectMode::Toggle);
        p.remove_selected();
        let next = p.peek_next(false, false).unwrap();
        assert_eq!(p.get(next).unwrap().path, Path::new("/music/4.mp3"));
    }

    #[test]
    fn removing_keeps_current_entry() {
        let mut p = list(5);
        p.set_current(Some(3));
        p.click(1, SelectMode::Single);
        p.click(2, SelectMode::Toggle);
        p.remove_selected();
        assert_eq!(p.len(), 3);
        assert_eq!(p.current_entry().unwrap().path, Path::new("/music/3.mp3"));
    }

    #[test]
    fn range_selection_and_crop() {
        let mut p = list(6);
        p.click(1, SelectMode::Single);
        p.click(3, SelectMode::Range);
        assert_eq!(p.selected_indices().collect::<Vec<_>>(), [1, 2, 3]);
        p.crop();
        assert_eq!(p.len(), 3);
    }

    #[test]
    fn insert_before_current_shifts_it() {
        let mut p = list(2);
        p.set_current(Some(1));
        p.insert(Some(0), [Entry::new("/a.mp3"), Entry::new("/b.mp3")]);
        assert_eq!(p.current(), Some(3));
    }

    #[test]
    fn sort_follows_current_entry() {
        let mut p = Playlist::with_seed(1);
        p.insert(None, ["/c.mp3", "/a.mp3", "/b.mp3"].map(Entry::new));
        p.set_current(Some(0));
        p.sort(SortKey::FileName);
        assert_eq!(p.current(), Some(2));
    }

    fn paths(p: &Playlist) -> Vec<String> {
        p.entries()
            .iter()
            .map(|e| e.path.file_stem().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn move_selected_keeps_order_and_current() {
        let mut p = list(6);
        p.set_current(Some(4));
        p.click(1, SelectMode::Single);
        p.click(4, SelectMode::Toggle);
        // Move rows 1 and 4 to before row 3.
        p.move_selected(3);
        assert_eq!(paths(&p), ["0", "2", "1", "4", "3", "5"]);
        assert_eq!(p.current(), Some(3));
        assert_eq!(p.selected_indices().collect::<Vec<_>>(), [2, 3]);
        // To the end.
        p.move_selected(p.len());
        assert_eq!(paths(&p), ["0", "2", "3", "5", "1", "4"]);
        assert_eq!(p.cursor(), Some(5));
    }

    #[test]
    fn cursor_moves_and_extends() {
        let mut p = list(5);
        p.move_cursor(1, false);
        assert_eq!(p.cursor(), Some(0));
        p.move_cursor(2, false);
        assert_eq!(p.selected_indices().collect::<Vec<_>>(), [2]);
        p.move_cursor(1, true);
        p.move_cursor(1, true);
        assert_eq!(p.selected_indices().collect::<Vec<_>>(), [2, 3, 4]);
        p.move_cursor(10, false);
        assert_eq!(p.cursor(), Some(4));
        let mut empty = Playlist::with_seed(1);
        empty.move_cursor(1, false);
        assert_eq!(empty.cursor(), None);
    }

    #[test]
    fn home_and_end_go_to_their_ends_without_a_cursor() {
        // Home went to the bottom and End to the top.
        let start = |delta: isize| {
            let mut p = list(5);
            p.move_cursor(delta, false);
            p.cursor()
        };
        assert_eq!(start(-5), Some(0));
        assert_eq!(start(5), Some(4));
        assert_eq!(start(1), Some(0));
        assert_eq!(start(-1), Some(4));
    }

    #[test]
    fn tracks_of_one_file_are_told_apart() {
        let part = |start: u64, end: Option<u64>| {
            Some(Part {
                start: Duration::from_secs(start),
                end: end.map(Duration::from_secs),
            })
        };
        let tags = |title: &str| TagInfo {
            title: Some(title.into()),
            duration: Some(Duration::from_secs(100)),
            ..Default::default()
        };
        let mut p = Playlist::with_seed(1);
        p.insert(
            None,
            [
                Entry::with_tags("/disc.flac", part(0, Some(100)), &tags("B")),
                Entry::with_tags("/disc.flac", part(100, None), &TagInfo::default()),
                Entry::with_tags("/disc.flac", part(200, Some(300)), &tags("A")),
            ],
        );
        assert!(
            !p.get(1).unwrap().tags_loaded,
            "the last track needs its length"
        );

        p.set_current_track(Path::new("/disc.flac"), Duration::from_secs(100));
        assert_eq!(p.current(), Some(1));
        p.sort(SortKey::Title);
        assert_eq!(p.current_entry().unwrap().start(), Duration::from_secs(100));

        // The whole file's tags give the last track its length, and nothing else.
        let file = TagInfo {
            title: Some("First song".into()),
            duration: Some(Duration::from_secs(250)),
            ..Default::default()
        };
        p.apply_tags(Path::new("/disc.flac"), &file);
        let titles: Vec<_> = p.entries().iter().map(|e| e.title.as_deref()).collect();
        assert_eq!(titles, [Some("A"), Some("B"), None]);
        let last = p.current_entry().unwrap();
        assert_eq!(last.duration, Some(Duration::from_secs(150)));
    }

    #[test]
    fn durations_report_unknowns() {
        let mut p = list(2);
        p.apply_tags(
            Path::new("/music/0.mp3"),
            &TagInfo {
                duration: Some(Duration::from_secs(60)),
                ..Default::default()
            },
        );
        assert_eq!(p.total_duration(), (Duration::from_secs(60), false));
    }

    #[test]
    fn huge_durations_add_up_without_overflowing() {
        // A damaged file's tags can claim any length; two such summed past Duration::MAX.
        let mut p = list(2);
        for i in 0..2 {
            p.apply_tags(
                Path::new(&format!("/music/{i}.mp3")),
                &TagInfo {
                    duration: Some(Duration::MAX),
                    ..Default::default()
                },
            );
        }
        assert_eq!(p.total_duration(), (Duration::MAX, true));
    }
}
