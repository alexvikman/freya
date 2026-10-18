//! The cover made for each album at a scan: kept among Freya's pictures, with a color for the
//! album's page, so the window shows covers without opening the music's files. Each says what
//! it was made from, so the next scan makes it again only when that changed. Never dropped
//! with the tracks: a rebuild keeps them.

use std::collections::{HashMap, HashSet};

use rusqlite::params;

use super::{Library, Result};

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS album_covers (
        -- The album, as `Thumbnails::key` names it from its artist and title.
        key TEXT NOT NULL PRIMARY KEY,
        -- The cover's SHA-256 among Freya's pictures; NULL when the album has none.
        cover TEXT,
        -- A color from it for the album's page, as \"rrggbb\".
        color TEXT,
        -- The track it was made for, and a stamp of the pictures in that track's folder.
        track TEXT NOT NULL,
        images INTEGER NOT NULL,
        made INTEGER NOT NULL
    );
";

/// An album's cover as the last scan made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumCover {
    pub cover: Option<String>,
    pub color: Option<[u8; 3]>,
    pub track: String,
    pub images: i64,
}

impl Library {
    /// Every album's cover, by the album's key.
    pub fn album_covers(&self) -> Result<HashMap<String, AlbumCover>> {
        let mut statement = self
            .db
            .prepare_cached("SELECT key, cover, color, track, images FROM album_covers")?;
        let rows = statement.query_map([], |row| {
            let color: Option<String> = row.get(2)?;
            Ok((
                row.get(0)?,
                AlbumCover {
                    cover: row.get(1)?,
                    color: color.as_deref().and_then(color_of),
                    track: row.get(3)?,
                    images: row.get(4)?,
                },
            ))
        })?;
        rows.collect()
    }

    pub fn set_album_cover(&self, key: &str, cover: &AlbumCover) -> Result<()> {
        let color = cover
            .color
            .map(|[r, g, b]| format!("{r:02x}{g:02x}{b:02x}"));
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO album_covers (key, cover, color, track, images, made)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![
                key,
                cover.cover,
                color,
                cover.track,
                cover.images,
                super::epoch_millis(std::time::SystemTime::now())
            ])?;
        Ok(())
    }

    /// Takes away the covers of albums no longer in the library.
    pub fn keep_album_covers(&self, keys: &HashSet<String>) -> Result<usize> {
        let kept: Vec<String> = self
            .album_covers()?
            .into_keys()
            .filter(|key| !keys.contains(key))
            .collect();
        for key in &kept {
            self.db
                .prepare_cached("DELETE FROM album_covers WHERE key = ?1")?
                .execute([key])?;
        }
        Ok(kept.len())
    }
}

fn color_of(text: &str) -> Option<[u8; 3]> {
    let value = u32::from_str_radix(text, 16).ok()?;
    let [_, r, g, b] = value.to_be_bytes();
    (text.len() == 6).then_some([r, g, b])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_each_albums_cover_and_forgets_those_gone() {
        let library = Library::open_in_memory().unwrap();
        let cover = AlbumCover {
            cover: Some("abc".into()),
            color: Some([10, 200, 33]),
            track: "/m/a/1.flac".into(),
            images: 42,
        };
        library.set_album_cover("a", &cover).unwrap();
        library
            .set_album_cover(
                "b",
                &AlbumCover {
                    cover: None,
                    color: None,
                    ..cover.clone()
                },
            )
            .unwrap();
        let covers = library.album_covers().unwrap();
        assert_eq!(covers["a"], cover);
        assert_eq!(covers["b"].cover, None);
        assert_eq!(
            library
                .keep_album_covers(&HashSet::from(["a".to_owned()]))
                .unwrap(),
            1
        );
        assert_eq!(library.album_covers().unwrap().len(), 1);
    }
}
