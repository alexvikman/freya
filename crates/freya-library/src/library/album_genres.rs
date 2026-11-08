//! The genres of albums whose tracks' tags give none: as MusicBrainz's users gave the album's
//! release group, asked by a scan, and kept apart, so a scan of the music never takes them
//! away. A track with a genre of its own is in its own genres alone.

use rusqlite::{OptionalExtension, params};

use super::{Asked, Library, Result};

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS album_online_genres (
        -- The album, as its tracks are filed: their album artist and album.
        album_artist TEXT NOT NULL,
        album TEXT NOT NULL,
        -- Its genres, most voted first, as a JSON array; empty when none were found.
        genres TEXT NOT NULL,
        -- The release group they are of; NULL when none was found.
        release_group TEXT,
        -- When MusicBrainz was asked, in milliseconds since 1970.
        asked INTEGER NOT NULL,
        PRIMARY KEY (album_artist, album)
    );
";

/// Each track and each genre it is in: its tags', else its album's from the internet. For a
/// query's `WITH`.
pub(super) const TRACK_GENRES: &str = "
    track_genre (track, genre) AS (
        SELECT track, genre FROM track_genres
        UNION ALL
        SELECT t.id, j.value
        FROM listed_tracks t
        JOIN album_online_genres g ON g.album_artist = t.album_artist AND g.album = t.album,
             json_each(g.genres) j
        WHERE t.id NOT IN (SELECT track FROM track_genres)
    )";

/// An album none of whose tracks' tags give a genre.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ungenred {
    pub album_artist: String,
    pub album: String,
    /// The release group the user matched it to, else the one found for its kind, if any.
    pub group: Option<String>,
}

/// What MusicBrainz said of an album's genres: of which release group, and when it was asked.
pub type OnlineGenres = (Option<String>, Asked<Vec<String>>);

impl Library {
    /// The albums, with an artist and a title, none of whose tracks has a genre in its tags.
    pub fn ungenred_albums(&self) -> Result<Vec<Ungenred>> {
        let mut query = self.db.prepare_cached(
            "SELECT t.album_artist, t.album,
                    ifnull(
                        (SELECT m.release_group FROM album_matches m
                         WHERE ifnull(m.artist_key, m.album_artist) = t.album_artist
                               AND m.album = t.album),
                        (SELECT k.release_group FROM album_kinds k
                         WHERE k.album_artist = t.album_artist AND k.album = t.album))
             FROM listed_tracks t
             WHERE t.album_artist IS NOT NULL AND t.album IS NOT NULL
             GROUP BY t.album_artist, t.album
             HAVING count(CASE WHEN t.id IN (SELECT track FROM track_genres) THEN 1 END) = 0
             ORDER BY t.album_artist, t.album",
        )?;
        query
            .query_map([], |row| {
                Ok(Ungenred {
                    album_artist: row.get(0)?,
                    album: row.get(1)?,
                    group: row.get(2)?,
                })
            })?
            .collect()
    }

    /// What MusicBrainz said of the album's genres, of which release group, and when.
    pub fn album_online_genres(
        &self,
        album_artist: &str,
        album: &str,
    ) -> Result<Option<OnlineGenres>> {
        self.db
            .prepare_cached(
                "SELECT release_group, genres, asked FROM album_online_genres
                 WHERE album_artist = ?1 AND album = ?2",
            )?
            .query_row([album_artist, album], |row| {
                let genres: String = row.get(1)?;
                Ok((
                    row.get(0)?,
                    Asked {
                        answer: serde_json::from_str(&genres).unwrap_or_default(),
                        asked: super::from_epoch_millis(row.get(2)?),
                    },
                ))
            })
            .optional()
    }

    /// Keeps the genres MusicBrainz gave the album's release group `group`, or that it found
    /// none; asked at `asked`.
    pub fn set_album_online_genres(
        &self,
        album_artist: &str,
        album: &str,
        group: Option<&str>,
        genres: &[String],
        asked: std::time::SystemTime,
    ) -> Result<()> {
        let genres = serde_json::to_string(genres).unwrap_or_else(|_| "[]".to_owned());
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO album_online_genres
                 (album_artist, album, genres, release_group, asked)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![
                album_artist,
                album,
                genres,
                group,
                super::epoch_millis(asked)
            ])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::tests::{add, tags};

    #[test]
    fn an_album_without_genres_takes_musicbrainzs_and_a_tagged_one_keeps_its_own() {
        let mut library = Library::open_in_memory().unwrap();
        let mut tagged = tags("Agalloch", "The Mantle", "A Celebration", 1);
        tagged.genre = Some("Neofolk".into());
        add(&mut library, "/m/a/1.mp3", &tagged);
        add(
            &mut library,
            "/m/p/1.mp3",
            &tags("Agalloch", "Pale Folklore", "She Painted Fire", 1),
        );
        add(
            &mut library,
            "/m/p/2.mp3",
            &tags("Agalloch", "Pale Folklore", "The Misshapen Steed", 2),
        );
        let ungenred = library.ungenred_albums().unwrap();
        assert_eq!(ungenred.len(), 1);
        assert_eq!(ungenred[0].album, "Pale Folklore");
        assert_eq!(
            library
                .album_online_genres("Agalloch", "Pale Folklore")
                .unwrap(),
            None
        );

        let at = std::time::SystemTime::now();
        let found = ["Folk Metal".to_owned(), "Black Metal".to_owned()];
        library
            .set_album_online_genres("Agalloch", "Pale Folklore", Some("group"), &found, at)
            .unwrap();
        let (group, asked) = library
            .album_online_genres("Agalloch", "Pale Folklore")
            .unwrap()
            .unwrap();
        assert_eq!(
            (group.as_deref(), asked.answer),
            (Some("group"), found.to_vec())
        );

        let genres: Vec<(Option<String>, u32, u32)> = library
            .genres()
            .unwrap()
            .into_iter()
            .map(|g| (g.name, g.albums, g.tracks))
            .collect();
        assert_eq!(
            genres,
            vec![
                (Some("Black Metal".to_owned()), 1, 2),
                (Some("Folk Metal".to_owned()), 1, 2),
                (Some("Neofolk".to_owned()), 1, 1),
            ]
        );
        let albums = library.genre_albums(Some("folk metal")).unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].name.as_deref(), Some("Pale Folklore"));
        assert_eq!(library.genre_tracks(Some("Black Metal")).unwrap().len(), 2);
        // Those in no genre: none now.
        assert!(library.genre_albums(None).unwrap().is_empty());
    }
}
