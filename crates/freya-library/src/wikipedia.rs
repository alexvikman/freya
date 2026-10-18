//! What Wikipedia says of an artist, found by their MusicBrainz id: MusicBrainz links them to
//! their Wikidata item, which names their article. So artists of one name, which Last.fm has
//! one page for, each get their own.

use crate::artist_image::get_json;

const MUSICBRAINZ_ARTIST: &str = "https://musicbrainz.org/ws/2/artist";
const WIKIDATA: &str = "https://www.wikidata.org/w/api.php";
const WIKIPEDIA: &str = "https://en.wikipedia.org/w/api.php";

/// The opening of an artist's article on Wikipedia, in plain text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Article {
    pub title: String,
    /// Its first paragraph.
    pub summary: String,
    /// The whole opening, before the first heading.
    pub biography: String,
    pub url: String,
}

/// The English Wikipedia article on the artist with the MusicBrainz id `mbid`; none when
/// MusicBrainz links them to none. An error means a service could not be asked.
pub fn article_of(mbid: &str) -> Result<Option<Article>, String> {
    let artist = get_json(
        &format!("{MUSICBRAINZ_ARTIST}/{mbid}"),
        &[("inc", "url-rels"), ("fmt", "json")],
    )?;
    let Some(item) = wikidata_item(&artist) else {
        return Ok(None);
    };
    let links = get_json(
        WIKIDATA,
        &[
            ("action", "wbgetentities"),
            ("ids", &item),
            ("props", "sitelinks"),
            ("sitefilter", "enwiki"),
            ("format", "json"),
        ],
    )?;
    let Some(title) = links
        .pointer(&format!("/entities/{item}/sitelinks/enwiki/title"))
        .and_then(|t| t.as_str())
    else {
        return Ok(None);
    };
    let page = get_json(
        WIKIPEDIA,
        &[
            ("action", "query"),
            ("prop", "extracts|info"),
            ("exintro", "1"),
            ("explaintext", "1"),
            ("inprop", "url"),
            ("redirects", "1"),
            ("titles", title),
            ("format", "json"),
            ("formatversion", "2"),
        ],
    )?;
    Ok(article(&page))
}

/// The Wikidata item MusicBrainz links an artist to: "Q763392".
fn wikidata_item(artist: &serde_json::Value) -> Option<String> {
    artist
        .get("relations")?
        .as_array()?
        .iter()
        .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("wikidata"))
        .find_map(|r| {
            let url = r.pointer("/url/resource")?.as_str()?;
            let item = url.rsplit('/').next()?;
            item.starts_with('Q').then(|| item.to_owned())
        })
}

fn article(page: &serde_json::Value) -> Option<Article> {
    let page = page.pointer("/query/pages/0")?;
    let biography = page
        .get("extract")?
        .as_str()?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if biography.is_empty() {
        return None;
    }
    let summary = biography
        .split("\n\n")
        .next()
        .unwrap_or_default()
        .to_owned();
    Some(Article {
        title: page.get("title")?.as_str()?.to_owned(),
        summary,
        biography,
        url: page
            .get("fullurl")
            .and_then(|u| u.as_str())
            .unwrap_or_default()
            .to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asks MusicBrainz, Wikidata and Wikipedia: `cargo test -- --ignored`.
    #[test]
    #[ignore = "asks the internet"]
    fn finds_the_article_on_one_of_two_bands_of_a_name() {
        let article = article_of("b16e3960-4e08-4dac-9606-32090f3ad5d4")
            .unwrap()
            .unwrap();
        assert_eq!(article.title, "Shining (Norwegian band)");
        assert!(article.summary.contains("Norwegian"));
    }

    #[test]
    fn finds_the_wikidata_item_musicbrainz_links_to() {
        let artist = serde_json::json!({ "relations": [
            { "type": "official homepage", "url": { "resource": "https://shining.no" } },
            { "type": "wikidata", "url": { "resource": "https://www.wikidata.org/wiki/Q763392" } },
        ]});
        assert_eq!(wikidata_item(&artist).as_deref(), Some("Q763392"));
        assert_eq!(wikidata_item(&serde_json::json!({ "relations": [] })), None);
    }

    #[test]
    fn reads_the_opening_of_an_article() {
        let page = serde_json::json!({ "query": { "pages": [{
            "title": "Shining (Norwegian band)",
            "fullurl": "https://en.wikipedia.org/wiki/Shining_(Norwegian_band)",
            "extract": "Shining is a Norwegian experimental band from Oslo.\nIts leading force is Jørgen Munkeby.\n\n",
        }]}});
        let article = article(&page).unwrap();
        assert_eq!(
            article.summary,
            "Shining is a Norwegian experimental band from Oslo."
        );
        assert_eq!(
            article.biography,
            "Shining is a Norwegian experimental band from Oslo.\n\nIts leading force is Jørgen Munkeby."
        );
        let missing =
            serde_json::json!({ "query": { "pages": [{ "title": "X", "missing": true }] } });
        assert_eq!(super::article(&missing), None);
    }
}
