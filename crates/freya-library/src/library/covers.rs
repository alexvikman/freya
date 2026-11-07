//! The cover made for each album at a scan: kept among Freya's pictures, with a color for the
//! album's page, so the window shows covers without opening the music's files. Each says what
//! it was made from, so the next scan makes it again only when that changed. Never dropped
//! with the tracks: a rebuild keeps them.

use std::collections::{HashMap, HashSet};

use rusqlite::{OptionalExtension, params};

/// How albums' colors are made: counted up when that changes, so they are made again.
const COLORS: i64 = 2;

use super::{Library, Result};
pub use freya_protocol::AlbumCover;

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
    -- Covers from the internet for albums whose music has none, as the album was matched
    -- to one in MusicBrainz: kept apart, so a scan of the music never takes them away.
    CREATE TABLE IF NOT EXISTS album_online_covers (
        key TEXT NOT NULL PRIMARY KEY,
        -- NULL when the source had none.
        cover TEXT,
        color TEXT,
        -- The release group it is of, and when it was asked for.
        release_group TEXT NOT NULL,
        asked INTEGER NOT NULL
    );
    -- The covers the user chose, over what the music has and what the internet had: kept
    -- apart, so neither a scan nor a fetch takes them away.
    CREATE TABLE IF NOT EXISTS album_chosen_covers (
        key TEXT NOT NULL PRIMARY KEY,
        cover TEXT NOT NULL,
        color TEXT,
        chosen INTEGER NOT NULL
    );
";

impl Library {
    /// Every album's cover, by the album's key: the one the user chose, else its music's, else
    /// one from the internet.
    pub fn album_covers(&self) -> Result<HashMap<String, AlbumCover>> {
        let mut covers = self.made_album_covers()?;
        let mut statement = self
            .db
            .prepare_cached("SELECT key, cover, color FROM album_chosen_covers")?;
        let chosen = statement.query_map([], |row| {
            let color: Option<String> = row.get(2)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                color.as_deref().and_then(color_of),
            ))
        })?;
        for row in chosen {
            let (key, cover, color) = row?;
            let made = covers.entry(key).or_insert_with(|| AlbumCover {
                cover: None,
                color: None,
                track: String::new(),
                images: 0,
                chosen: false,
            });
            made.cover = Some(cover);
            made.color = color.or(made.color);
            made.chosen = true;
        }
        Ok(covers)
    }

    /// Keeps `cover`, among Freya's pictures, as the cover the user chose for the album `key`,
    /// with `color` for its page; none goes back to its music's, or the internet's.
    pub fn set_chosen_album_cover(
        &self,
        key: &str,
        cover: Option<&str>,
        color: Option<[u8; 3]>,
    ) -> Result<()> {
        match cover {
            Some(cover) => {
                self.db
                    .prepare_cached(
                        "INSERT OR REPLACE INTO album_chosen_covers (key, cover, color, chosen)
                         VALUES (?1, ?2, ?3, ?4)",
                    )?
                    .execute(params![
                        key,
                        cover,
                        color.map(|[r, g, b]| format!("{r:02x}{g:02x}{b:02x}")),
                        super::epoch_millis(std::time::SystemTime::now())
                    ])?;
            }
            None => {
                self.db
                    .prepare_cached("DELETE FROM album_chosen_covers WHERE key = ?1")?
                    .execute([key])?;
            }
        }
        Ok(())
    }

    /// Every album's cover as its music or the internet gave it, without the user's choice:
    /// what a scan looks at, to know which to make again.
    pub(super) fn made_album_covers(&self) -> Result<HashMap<String, AlbumCover>> {
        let mut statement = self.db.prepare_cached(
            "SELECT c.key, coalesce(c.cover, o.cover), coalesce(c.color, o.color), c.track,
                    c.images
             FROM album_covers c LEFT JOIN album_online_covers o ON o.key = c.key
             UNION ALL
             SELECT o.key, o.cover, o.color, '', 0
             FROM album_online_covers o
             WHERE o.cover IS NOT NULL AND o.key NOT IN (SELECT key FROM album_covers)",
        )?;
        let rows = statement.query_map([], |row| {
            let color: Option<String> = row.get(2)?;
            Ok((
                row.get(0)?,
                AlbumCover {
                    cover: row.get(1)?,
                    color: color.as_deref().and_then(color_of),
                    track: row.get(3)?,
                    images: row.get(4)?,
                    chosen: false,
                },
            ))
        })?;
        rows.collect()
    }

    /// Keeps the cover from the internet of the album `key`, of the release group `group`, or
    /// that there is none; asked at `asked`.
    pub fn set_album_online_cover(
        &self,
        key: &str,
        group: &str,
        cover: Option<(&str, [u8; 3])>,
        asked: std::time::SystemTime,
    ) -> Result<()> {
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO album_online_covers (key, cover, color, release_group, asked)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![
                key,
                cover.map(|(c, _)| c),
                cover.map(|(_, [r, g, b])| format!("{r:02x}{g:02x}{b:02x}")),
                group,
                super::epoch_millis(asked)
            ])?;
        Ok(())
    }

    /// The release group the album `key`'s cover from the internet was asked of, and when,
    /// and whether there was one.
    pub fn album_online_cover(&self, key: &str) -> Result<Option<(String, super::Asked<bool>)>> {
        self.db
            .prepare_cached(
                "SELECT release_group, cover IS NOT NULL, asked FROM album_online_covers
                 WHERE key = ?1",
            )?
            .query_row([key], |row| {
                Ok((
                    row.get(0)?,
                    super::Asked {
                        answer: row.get(1)?,
                        asked: super::from_epoch_millis(row.get(2)?),
                    },
                ))
            })
            .optional()
    }

    /// Whether the albums' colors are made as Freya makes them now.
    pub fn cover_colors_current(&self) -> Result<bool> {
        let made: Option<i64> = self
            .db
            .prepare_cached("SELECT at FROM library_state WHERE key = 'cover-colors'")?
            .query_row([], |row| row.get(0))
            .optional()?;
        Ok(made == Some(COLORS))
    }

    /// Makes every album's color again from its cover among `pictures`, as Freya makes them
    /// now, without reading the music's files. Stops early when `cancel` is set. Returns how
    /// many were made.
    pub fn make_cover_colors(
        &self,
        pictures: &crate::pictures::Pictures,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<usize> {
        let covers: Vec<(String, String)> = self
            .album_covers()?
            .into_iter()
            .filter_map(|(key, made)| Some((key, made.cover?)))
            .collect();
        let mut made = 0;
        for (key, cover) in covers {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Ok(made);
            }
            let Some(picture) = pictures
                .read(&cover)
                .and_then(|bytes| image::load_from_memory(&bytes).ok())
            else {
                continue;
            };
            let color = crate::cover::page_color(picture.thumbnail(64, 64).to_rgb8().as_raw());
            self.db
                .prepare_cached("UPDATE album_covers SET color = ?2 WHERE key = ?1")?
                .execute(params![
                    key,
                    format!("{:02x}{:02x}{:02x}", color[0], color[1], color[2])
                ])?;
            made += 1;
        }
        self.db
            .prepare_cached(
                "INSERT OR REPLACE INTO library_state (key, at) VALUES ('cover-colors', ?1)",
            )?
            .execute([COLORS])?;
        Ok(made)
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
            .made_album_covers()?
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
    fn a_cover_from_the_internet_stands_in_for_the_musics_and_stays() {
        let library = Library::open_in_memory().unwrap();
        let at = std::time::SystemTime::now();
        // Matched before a scan made its row.
        library
            .set_album_online_cover("k", "group", Some(("cafe", [10, 20, 30])), at)
            .unwrap();
        assert_eq!(
            library.album_covers().unwrap()["k"].cover.as_deref(),
            Some("cafe")
        );
        // A scan finds no cover in its music: the internet's stays.
        let none = AlbumCover {
            cover: None,
            color: None,
            track: "/m/1.mp3".into(),
            images: 0,
            chosen: false,
        };
        library.set_album_cover("k", &none).unwrap();
        let kept = &library.album_covers().unwrap()["k"];
        assert_eq!(
            (kept.cover.as_deref(), kept.color),
            (Some("cafe"), Some([10, 20, 30]))
        );
        assert_eq!(kept.track, "/m/1.mp3");
        // Its music's comes first.
        let own = AlbumCover {
            cover: Some("beef".into()),
            color: Some([1, 2, 3]),
            ..none
        };
        library.set_album_cover("k", &own).unwrap();
        assert_eq!(
            library.album_covers().unwrap()["k"].cover.as_deref(),
            Some("beef")
        );
        let (group, asked) = library.album_online_cover("k").unwrap().unwrap();
        assert_eq!((group.as_str(), asked.answer), ("group", true));
        assert!(library.artist_picture_checksums().unwrap().contains("cafe"));
    }

    #[test]
    fn a_chosen_cover_comes_first_and_stays_until_unchosen() {
        let library = Library::open_in_memory().unwrap();
        let own = AlbumCover {
            cover: Some("beef".into()),
            color: Some([1, 2, 3]),
            track: "/m/1.mp3".into(),
            images: 4,
            chosen: false,
        };
        library.set_album_cover("k", &own).unwrap();
        library
            .set_chosen_album_cover("k", Some("feed"), Some([9, 8, 7]))
            .unwrap();
        let shown = &library.album_covers().unwrap()["k"];
        assert_eq!(
            (shown.cover.as_deref(), shown.color, shown.chosen),
            (Some("feed"), Some([9, 8, 7]), true)
        );
        // What the music has is still known, for a scan.
        assert_eq!(library.made_album_covers().unwrap()["k"], own);
        // A scan makes the music's again: the chosen one stays.
        library.set_album_cover("k", &own).unwrap();
        assert_eq!(
            library.album_covers().unwrap()["k"].cover.as_deref(),
            Some("feed")
        );
        assert!(library.artist_picture_checksums().unwrap().contains("feed"));
        // Chosen for an album with no cover of its own.
        library
            .set_chosen_album_cover("other", Some("f00d"), None)
            .unwrap();
        assert!(library.album_covers().unwrap()["other"].chosen);
        // Unchosen, the music's again.
        library.set_chosen_album_cover("k", None, None).unwrap();
        assert_eq!(library.album_covers().unwrap()["k"], own);
    }

    #[test]
    fn keeps_each_albums_cover_and_forgets_those_gone() {
        let library = Library::open_in_memory().unwrap();
        let cover = AlbumCover {
            cover: Some("abc".into()),
            color: Some([10, 200, 33]),
            track: "/m/a/1.flac".into(),
            images: 42,
            chosen: false,
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
