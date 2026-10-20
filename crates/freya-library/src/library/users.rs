//! Whose each play, favourite and playlist's own things are: the user's, by the server's id
//! for them (see `freya_server::accounts`). There is one user now, the server's owner, 1; every
//! row is theirs, and reads do not ask whose yet. Rows written without a user are the owner's.

use rusqlite::Connection;

use super::Result;

/// The server's owner, whose every row is until there are more users.
pub const OWNER: i64 = 1;

/// The tables that hold the users' own rows.
const TABLES: [&str; 6] = [
    "plays",
    "album_favorites",
    "artist_favorites",
    "playlist_favorites",
    "playlist_info",
    "playlist_added",
];

/// Gives the tables of a library from before users their `user` column, every row the
/// owner's: added to `plays`, which has no key; the others made again with the user in their
/// key, which SQLite cannot change in place. `schemas` makes the tables as they are now.
pub(super) fn migrate(db: &Connection, schemas: &[&str]) -> Result<()> {
    for table in TABLES {
        let columns: Vec<String> = db
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_>>()?;
        if columns.is_empty() || columns.iter().any(|c| c == "user") {
            continue;
        }
        log::info!("giving {table} its user");
        if table == "plays" {
            db.execute_batch(&format!(
                "ALTER TABLE plays ADD COLUMN user INTEGER NOT NULL DEFAULT {OWNER};"
            ))?;
            continue;
        }
        let tx = db.unchecked_transaction()?;
        tx.execute_batch(&format!(
            "ALTER TABLE {table} RENAME TO {table}_before_users;"
        ))?;
        for schema in schemas {
            tx.execute_batch(schema)?;
        }
        let columns = columns.join(", ");
        tx.execute_batch(&format!(
            "INSERT INTO {table} ({columns}, user)
                 SELECT {columns}, {OWNER} FROM {table}_before_users;
             DROP TABLE {table}_before_users;"
        ))?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Library;

    #[test]
    fn a_library_from_before_users_keeps_its_rows_as_the_owners() {
        let dir = std::env::temp_dir().join(format!("freya-users-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.sqlite3");
        {
            // As the tables were made before.
            let db = Connection::open(&path).unwrap();
            db.execute_batch(
                "CREATE TABLE plays (path TEXT NOT NULL, start_ms INTEGER, played INTEGER NOT NULL);
                 INSERT INTO plays VALUES ('/m/a.flac', NULL, 5);
                 CREATE TABLE album_favorites (album_artist TEXT NOT NULL, album TEXT NOT NULL,
                     at INTEGER NOT NULL, PRIMARY KEY (album_artist, album));
                 INSERT INTO album_favorites VALUES ('Freya', 'Sines', 7);
                 CREATE TABLE playlist_info (playlist TEXT NOT NULL PRIMARY KEY,
                     description TEXT, picture TEXT, backdrop TEXT);
                 INSERT INTO playlist_info VALUES ('Calm', 'Quiet', NULL, NULL);",
            )
            .unwrap();
        }
        let library = Library::open(&path).unwrap();
        let favorites = library.album_favorites().unwrap();
        assert!(favorites.contains_key(&(Some("Freya".into()), Some("Sines".into()))));
        assert_eq!(
            library
                .playlist_info("Calm")
                .unwrap()
                .description
                .as_deref(),
            Some("Quiet")
        );
        let owners: i64 = library
            .db
            .query_row(
                "SELECT (SELECT count(*) FROM plays WHERE user = 1)
                      + (SELECT count(*) FROM album_favorites WHERE user = 1)
                      + (SELECT count(*) FROM playlist_info WHERE user = 1)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owners, 3);
        // The same album may be another user's favourite too.
        library
            .db
            .execute(
                "INSERT INTO album_favorites (user, album_artist, album, at)
                 VALUES (2, 'Freya', 'Sines', 9)",
                [],
            )
            .unwrap();
        drop(library);
        // And opened again, nothing changes.
        let again = Library::open(&path).unwrap();
        let rows: i64 = again
            .db
            .query_row("SELECT count(*) FROM album_favorites", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
