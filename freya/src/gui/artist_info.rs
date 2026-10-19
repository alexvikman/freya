//! Last.fm, as a scan asks it what it says of artists who lack it: see
//! [`freya_library::library::Online`]. Pages only show what was kept.

use std::sync::Arc;

use freya_library::library::{ArtistText, LastFm, plain_name};

use crate::lastfm::{Client, Credentials};

struct Texts(Client);

impl LastFm for Texts {
    fn artist_text(&self, name: &str, mbid: Option<&str>) -> Result<ArtistText, String> {
        let info = self
            .0
            .artist_info(plain_name(name), mbid)
            .map_err(|e| e.to_string())?;
        Ok(ArtistText {
            summary: info.summary,
            biography: info.biography,
            tags: info.tags,
            url: info.url,
        })
    }

    fn tag_text(&self, name: &str) -> Result<ArtistText, String> {
        let info = self.0.tag_info(name).map_err(|e| e.to_string())?;
        Ok(ArtistText {
            summary: info.summary,
            biography: info.biography,
            tags: Vec::new(),
            url: info.url,
        })
    }
}

/// Last.fm for a scan to ask, when this build has Freya's key.
pub fn lastfm() -> Option<Arc<dyn LastFm>> {
    Credentials::of_build().map(|credentials| Arc::new(Texts(Client::new(credentials))) as _)
}
