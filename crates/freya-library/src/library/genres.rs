//! What Freya keeps of the library's genres: the user's picture, wide picture and description,
//! and what the internet says of each, asked for by a scan when a genre lacks it (see
//! [`super::fetch`]). By the genre's name, ignoring case.

use std::collections::HashMap;
use std::time::SystemTime;

use rusqlite::{OptionalExtension, params};

use super::{Asked, Library, Result, epoch_millis, from_epoch_millis};
pub use freya_protocol::{GenreArticle, GenreInfo};

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS genre_info (
        genre TEXT NOT NULL PRIMARY KEY COLLATE NOCASE,
        -- Pictures among Freya's own, by their SHA-256.
        picture TEXT,
        backdrop TEXT,
        -- The user's own, in place of the internet's.
        description TEXT
    );
    CREATE TABLE IF NOT EXISTS genre_articles (
        genre TEXT NOT NULL PRIMARY KEY COLLATE NOCASE,
        -- Empty when none was found.
        summary TEXT NOT NULL,
        description TEXT NOT NULL,
        url TEXT NOT NULL,
        -- Where it is from: 'Wikipedia' or 'Last.fm'.
        source TEXT NOT NULL,
        asked INTEGER NOT NULL
    );
";

impl Library {
    pub fn genre_info(&self, genre: &str) -> Result<GenreInfo> {
        Ok(self
            .db
            .prepare_cached(
                "SELECT picture, backdrop, description FROM genre_info WHERE genre = ?1",
            )?
            .query_row([genre], |row| {
                Ok(GenreInfo {
                    picture: row.get(0)?,
                    backdrop: row.get(1)?,
                    description: row.get(2)?,
                })
            })
            .optional()?
            .unwrap_or_default())
    }

    /// Every genre the user gave something, by its name in lower case.
    pub fn genre_infos(&self) -> Result<HashMap<String, GenreInfo>> {
        let mut statement = self
            .db
            .prepare_cached("SELECT genre, picture, backdrop, description FROM genre_info")?;
        let rows = statement.query_map([], |row| {
            let genre: String = row.get(0)?;
            Ok((
                genre.to_lowercase(),
                GenreInfo {
                    picture: row.get(1)?,
                    backdrop: row.get(2)?,
                    description: row.get(3)?,
                },
            ))
        })?;
        rows.collect()
    }

    /// Keeps what the user gave `genre`; with nothing, its row goes.
    pub fn set_genre_info(&self, genre: &str, info: &GenreInfo) -> Result<()> {
        let description = info
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty());
        if description.is_none() && info.picture.is_none() && info.backdrop.is_none() {
            self.db
                .prepare_cached("DELETE FROM genre_info WHERE genre = ?1")?
                .execute([genre])?;
            return Ok(());
        }
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO genre_info (genre, picture, backdrop, description)
                 VALUES (?1, ?2, ?3, ?4)",
            )?
            .execute(params![genre, info.picture, info.backdrop, description])?;
        Ok(())
    }

    /// What the internet says of `genre`, or that it found nothing; asked when.
    pub fn genre_article(&self, genre: &str) -> Result<Option<Asked<Option<GenreArticle>>>> {
        self.db
            .prepare_cached(
                "SELECT summary, description, url, source, asked FROM genre_articles
                 WHERE genre = ?1",
            )?
            .query_row([genre], |row| {
                let summary: String = row.get(0)?;
                let article = (!summary.is_empty()).then(|| -> rusqlite::Result<_> {
                    Ok(GenreArticle {
                        summary,
                        description: row.get(1)?,
                        url: row.get(2)?,
                        source: row.get(3)?,
                    })
                });
                Ok(Asked {
                    answer: article.transpose()?,
                    asked: from_epoch_millis(row.get(4)?),
                })
            })
            .optional()
    }

    pub fn set_genre_article(
        &self,
        genre: &str,
        article: Option<&GenreArticle>,
        asked: SystemTime,
    ) -> Result<()> {
        let text = |f: fn(&GenreArticle) -> &str| article.map_or("", f);
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO genre_articles
                     (genre, summary, description, url, source, asked)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![
                genre,
                text(|a| &a.summary),
                text(|a| &a.description),
                text(|a| &a.url),
                text(|a| &a.source),
                epoch_millis(asked)
            ])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_genres_pictures_and_what_is_said_of_it() {
        let library = Library::open_in_memory().unwrap();
        let info = GenreInfo {
            picture: Some("aa".into()),
            ..GenreInfo::default()
        };
        library.set_genre_info("Black Metal", &info).unwrap();
        assert_eq!(
            library.genre_info("black metal").unwrap(),
            info,
            "ignoring case"
        );
        assert!(library.genre_infos().unwrap().contains_key("black metal"));
        assert!(library.artist_picture_checksums().unwrap().contains("aa"));
        library
            .set_genre_info("Black Metal", &GenreInfo::default())
            .unwrap();
        assert!(library.genre_infos().unwrap().is_empty());

        let article = GenreArticle {
            summary: "A genre.".into(),
            description: "A genre.\n\nMore.".into(),
            url: String::new(),
            source: "Wikipedia".into(),
        };
        library
            .set_genre_article("Ambient", Some(&article), SystemTime::now())
            .unwrap();
        assert_eq!(
            library.genre_article("AMBIENT").unwrap().unwrap().answer,
            Some(article)
        );
        library
            .set_genre_article("Calssical", None, SystemTime::now())
            .unwrap();
        assert_eq!(
            library.genre_article("Calssical").unwrap().unwrap().answer,
            None
        );
    }
}
