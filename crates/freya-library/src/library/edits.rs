//! What the user changed about artists in Freya: the name shown, the name sorted by, their
//! MusicBrainz id and their biography. Their music's files and tags are left as they are, and
//! a scan does not undo any of it: never dropped.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};

use super::{Library, Result};
pub use freya_protocol::{ArtistEdit, Header};

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS artist_edits (
        -- The artist as the tags name them: the album artist.
        artist TEXT NOT NULL PRIMARY KEY,
        -- Each NULL where the user changed nothing.
        name TEXT,
        sort_name TEXT,
        mbid TEXT,
        biography TEXT,
        -- MusicBrainz's words to tell them from others of the name, with the id.
        note TEXT,
        -- What the top of their page shows: 'wide' or 'picture'; NULL for what they have.
        header TEXT
    );
";

/// Adds what came after the table was first made, to a library from before.
pub(super) fn migrate(db: &rusqlite::Connection) -> Result<()> {
    if db.prepare("SELECT note FROM artist_edits LIMIT 0").is_err() {
        db.execute_batch("ALTER TABLE artist_edits ADD COLUMN note TEXT;")?;
    }
    if db
        .prepare("SELECT header FROM artist_edits LIMIT 0")
        .is_err()
    {
        db.execute_batch("ALTER TABLE artist_edits ADD COLUMN header TEXT;")?;
    }
    Ok(())
}

/// The header kept in column `i` of `row`: none for a name Freya does not know.
fn header_at(row: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<Option<Header>> {
    Ok(row
        .get::<_, Option<String>>(i)?
        .as_deref()
        .and_then(Header::from_name))
}

impl Library {
    /// What the user changed about `artist`.
    pub fn artist_edit(&self, artist: &str) -> Result<ArtistEdit> {
        Ok(self
            .db
            .prepare_cached(
                "SELECT name, sort_name, mbid, biography, note, header FROM artist_edits
                 WHERE artist = ?1",
            )?
            .query_row([artist], edit_from_row)
            .optional()?
            .unwrap_or_default())
    }

    /// Every artist the user changed something about.
    pub fn artist_edits(&self) -> Result<HashMap<String, ArtistEdit>> {
        let mut statement = self.db.prepare_cached(
            "SELECT artist, name, sort_name, mbid, biography, note, header FROM artist_edits",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get(0)?,
                ArtistEdit {
                    name: row.get(1)?,
                    sort_name: row.get(2)?,
                    mbid: row.get(3)?,
                    biography: row.get(4)?,
                    note: row.get(5)?,
                    header: header_at(row, 6)?,
                },
            ))
        })?;
        rows.collect()
    }

    /// Keeps what the user changed about `artist`; blank fields are unchanged. Nothing
    /// changed takes their row away.
    pub fn set_artist_edit(&self, artist: &str, edit: &ArtistEdit) -> Result<()> {
        let edit = edit.trimmed();
        if edit.is_empty() {
            self.db
                .prepare_cached("DELETE FROM artist_edits WHERE artist = ?1")?
                .execute([artist])?;
            return Ok(());
        }
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO artist_edits
                     (artist, name, sort_name, mbid, biography, note, header)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?
            .execute(params![
                artist,
                edit.name,
                edit.sort_name,
                edit.mbid,
                edit.biography,
                edit.note,
                edit.header.map(Header::name)
            ])?;
        Ok(())
    }
}

fn edit_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtistEdit> {
    Ok(ArtistEdit {
        name: row.get(0)?,
        sort_name: row.get(1)?,
        mbid: row.get(2)?,
        biography: row.get(3)?,
        note: row.get(4)?,
        header: header_at(row, 5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_the_note_to_a_library_from_before() {
        let dir = std::env::temp_dir().join(format!("freya-edits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.sqlite3");
        drop(Library::open(&path).unwrap());
        {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch(
                "DROP TABLE artist_edits;
                 CREATE TABLE artist_edits (artist TEXT NOT NULL PRIMARY KEY, name TEXT,
                     sort_name TEXT, mbid TEXT, biography TEXT);
                 INSERT INTO artist_edits (artist, name) VALUES ('Opeth', 'OPETH');",
            )
            .unwrap();
        }
        let library = Library::open(&path).unwrap();
        let edit = library.artist_edit("Opeth").unwrap();
        assert_eq!((edit.name.as_deref(), edit.note), (Some("OPETH"), None));
        let noted = ArtistEdit {
            mbid: Some("id".into()),
            note: Some("Swedish band".into()),
            ..edit
        };
        library.set_artist_edit("Opeth", &noted).unwrap();
        assert_eq!(
            library.artist_edit("Opeth").unwrap().note.as_deref(),
            Some("Swedish band")
        );
        drop(library);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_what_the_user_changed_and_forgets_it_when_cleared() {
        let library = Library::open_in_memory().unwrap();
        assert_eq!(library.artist_edit("Opeth").unwrap(), ArtistEdit::default());

        let edit = ArtistEdit {
            name: Some("  Anna von Hausswolff ".into()),
            sort_name: Some("".into()),
            biography: Some("A Swedish singer.".into()),
            header: Some(Header::Picture),
            ..ArtistEdit::default()
        };
        library
            .set_artist_edit("Anna Von Hausswolff", &edit)
            .unwrap();
        let kept = library.artist_edit("Anna Von Hausswolff").unwrap();
        assert_eq!(kept.name.as_deref(), Some("Anna von Hausswolff"), "trimmed");
        assert_eq!(kept.sort_name, None, "blank is unchanged");
        assert_eq!(kept.biography.as_deref(), Some("A Swedish singer."));
        assert_eq!(kept.header, Some(Header::Picture));
        assert_eq!(library.artist_edits().unwrap().len(), 1);

        library
            .set_artist_edit("Anna Von Hausswolff", &ArtistEdit::default())
            .unwrap();
        assert!(library.artist_edits().unwrap().is_empty());
    }
}
