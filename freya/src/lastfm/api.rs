//! Last.fm's web service: signed calls for signing in, "now playing" and scrobbles.
//! <https://www.last.fm/api>

use std::time::Duration;

use serde_json::Value;

use super::md5;

const ENDPOINT: &str = "https://ws.audioscrobbler.com/2.0/";
/// Where the user lets Freya scrobble to their account.
const AUTH_PAGE: &str = "https://www.last.fm/api/auth/";
/// Last.fm takes at most this many scrobbles in one call.
pub const MAX_BATCH: usize = 50;

/// Freya's API key and shared secret, given to the build (see docs/RELEASING.md); never in the
/// repository. Builds without them cannot scrobble.
#[derive(Clone, Copy)]
pub struct Credentials {
    key: &'static str,
    secret: &'static str,
}

impl Credentials {
    /// The credentials this build was made with, if it was given them.
    pub fn of_build() -> Option<Self> {
        let key = option_env!("FREYA_LASTFM_API_KEY")?;
        let secret = option_env!("FREYA_LASTFM_SECRET")?;
        (key.len() == 32 && !secret.is_empty()).then_some(Self { key, secret })
    }
}

/// A track as Last.fm is told about it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    /// Whole seconds.
    pub duration: Option<u64>,
}

/// A play to scrobble: the track, and when it started (seconds since 1970, UTC).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Scrobble {
    pub track: Track,
    pub started: u64,
}

/// What Last.fm says of an artist, in plain text: a short biography, the whole one, the tags
/// listeners gave them, and their page. Empty for an artist Last.fm does not know.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArtistInfo {
    pub summary: String,
    pub biography: String,
    pub tags: Vec<String>,
    pub url: String,
}

/// A signed-in user: their name, and the session key that lets Freya scrobble for them.
pub struct Session {
    pub user: String,
    pub key: String,
}

#[derive(Debug)]
pub enum Error {
    /// Last.fm could not be reached, or answered with something that is not its API.
    Network(String),
    /// Last.fm refused, with its error code and message.
    Api(u32, String),
}

impl Error {
    /// Whether trying again later may work: the network, or Last.fm being busy.
    pub fn is_temporary(&self) -> bool {
        match self {
            Self::Network(_) => true,
            // Operation failed, service offline, temporarily unavailable, rate limit exceeded.
            Self::Api(code, _) => matches!(code, 8 | 11 | 16 | 29),
        }
    }

    /// Whether the session no longer works, so the user has to sign in again.
    pub fn is_session_invalid(&self) -> bool {
        // Invalid session key; authentication failed; suspended API key.
        matches!(self, Self::Api(9 | 4 | 26, _))
    }

    /// Whether Last.fm finds the call's parameters invalid: for a scrobble, a play it will
    /// never take.
    pub fn is_invalid_play(&self) -> bool {
        matches!(self, Self::Api(6, _))
    }

    /// Whether the user has not yet allowed Freya on Last.fm's page.
    pub fn is_unauthorized_token(&self) -> bool {
        matches!(self, Self::Api(14, _))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(e) => write!(f, "Last.fm could not be reached: {e}"),
            Self::Api(_, message) => write!(f, "Last.fm: {message}"),
        }
    }
}

pub struct Client {
    agent: ureq::Agent,
    credentials: Credentials,
}

impl Client {
    pub fn new(credentials: Credentials) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent(concat!("Freya/", env!("CARGO_PKG_VERSION")))
            // Last.fm answers errors with a status and a JSON body that says which.
            .http_status_as_error(false)
            .build()
            .into();
        Self { agent, credentials }
    }

    /// A request token, to be allowed by the user on [`Self::auth_page`].
    pub fn token(&self) -> Result<String, Error> {
        let answer = self.call("auth.getToken", &[])?;
        text(&answer, &["token"])
    }

    /// The page where the user lets Freya scrobble for them, for `token`.
    pub fn auth_page(&self, token: &str) -> String {
        format!(
            "{AUTH_PAGE}?api_key={}&token={}",
            self.credentials.key,
            encode(token)
        )
    }

    /// The session, once the user has allowed `token`.
    pub fn session(&self, token: &str) -> Result<Session, Error> {
        let answer = self.call("auth.getSession", &[("token", token)])?;
        Ok(Session {
            user: text(&answer, &["session", "name"])?,
            key: text(&answer, &["session", "key"])?,
        })
    }

    pub fn now_playing(&self, session_key: &str, track: &Track) -> Result<(), Error> {
        let duration = track.duration.map(|d| d.to_string());
        let mut params = vec![
            ("artist", track.artist.as_str()),
            ("track", track.title.as_str()),
            ("sk", session_key),
        ];
        if let Some(album) = &track.album {
            params.push(("album", album));
        }
        if let Some(duration) = &duration {
            params.push(("duration", duration));
        }
        self.call("track.updateNowPlaying", &params).map(|_| ())
    }

    /// Submits up to [`MAX_BATCH`] scrobbles. Returns how many Last.fm accepted; the others
    /// it ignored for good (too old, a filtered artist), and are not sent again.
    pub fn scrobble(&self, session_key: &str, scrobbles: &[Scrobble]) -> Result<usize, Error> {
        let scrobbles = &scrobbles[..scrobbles.len().min(MAX_BATCH)];
        let mut owned: Vec<(String, String)> = Vec::new();
        for (i, s) in scrobbles.iter().enumerate() {
            owned.push((format!("artist[{i}]"), s.track.artist.clone()));
            owned.push((format!("track[{i}]"), s.track.title.clone()));
            owned.push((format!("timestamp[{i}]"), s.started.to_string()));
            if let Some(album) = &s.track.album {
                owned.push((format!("album[{i}]"), album.clone()));
            }
            if let Some(duration) = s.track.duration {
                owned.push((format!("duration[{i}]"), duration.to_string()));
            }
        }
        let mut params: Vec<(&str, &str)> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        params.push(("sk", session_key));
        let answer = self.call("track.scrobble", &params)?;
        // Anything else, from a proxy say, is not Last.fm's answer: the plays may not have
        // arrived, so they are kept and sent again.
        let accepted = answer
            .pointer("/scrobbles/@attr/accepted")
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
            .ok_or_else(|| Error::Network("the answer is not Last.fm's".into()))?;
        Ok(accepted as usize)
    }

    /// What Last.fm says of the artist `name`; empty for one it does not know. Needs no user.
    /// What Last.fm says of the artist with the MusicBrainz id `mbid`, which tells apart
    /// artists of one name; else of the artist named `name`.
    pub fn artist_info(&self, name: &str, mbid: Option<&str>) -> Result<ArtistInfo, Error> {
        if let Some(mbid) = mbid {
            let info = self.ask_artist_info(&[("mbid", mbid)])?;
            // Last.fm does not know every id.
            if info != ArtistInfo::default() {
                return Ok(info);
            }
        }
        self.ask_artist_info(&[("artist", name), ("autocorrect", "1")])
    }

    fn ask_artist_info(&self, by: &[(&str, &str)]) -> Result<ArtistInfo, Error> {
        match self.get("artist.getInfo", by) {
            Ok(answer) => Ok(artist_info(&answer)),
            // "The artist you supplied could not be found."
            Err(Error::Api(6, _)) => Ok(ArtistInfo::default()),
            Err(e) => Err(e),
        }
    }

    /// An unsigned GET, for what anyone may read, answered in JSON.
    fn get(&self, method: &str, params: &[(&str, &str)]) -> Result<Value, Error> {
        let mut request = self
            .agent
            .get(ENDPOINT)
            .query("method", method)
            .query("api_key", self.credentials.key)
            .query("format", "json");
        for (name, value) in params {
            request = request.query(*name, *value);
        }
        let body = request
            .call()
            .and_then(|mut response| response.body_mut().read_to_string())
            .map_err(|e| Error::Network(e.to_string()))?;
        answer_of(&body)
    }

    /// A signed POST to the API, answered in JSON.
    fn call(&self, method: &str, params: &[(&str, &str)]) -> Result<Value, Error> {
        let mut params = params.to_vec();
        params.push(("method", method));
        params.push(("api_key", self.credentials.key));
        let signature = signature(&params, self.credentials.secret);
        params.push(("api_sig", &signature));
        params.push(("format", "json"));

        let body = self
            .agent
            .post(ENDPOINT)
            .send_form(params.iter().copied())
            .and_then(|mut response| response.body_mut().read_to_string())
            .map_err(|e| Error::Network(e.to_string()))?;
        answer_of(&body)
    }
}

/// An answer's JSON, or the error Last.fm answered with.
fn answer_of(body: &str) -> Result<Value, Error> {
    let answer: Value = serde_json::from_str(body).map_err(|e| Error::Network(e.to_string()))?;
    if let Some(code) = answer.get("error").and_then(Value::as_u64) {
        let message = answer
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_owned();
        return Err(Error::Api(code as u32, message));
    }
    Ok(answer)
}

/// An artist.getInfo answer as plain text.
fn artist_info(answer: &Value) -> ArtistInfo {
    let artist = &answer["artist"];
    let text = |value: &Value| plain_text(value.as_str().unwrap_or(""));
    // One tag comes as an object, several as a list.
    let tags = match &artist["tags"]["tag"] {
        Value::Array(tags) => tags.iter().collect(),
        tag @ Value::Object(_) => vec![tag],
        _ => Vec::new(),
    };
    ArtistInfo {
        summary: text(&artist["bio"]["summary"]),
        biography: text(&artist["bio"]["content"]),
        tags: tags
            .into_iter()
            .filter_map(|t| t["name"].as_str().map(str::to_owned))
            .collect(),
        url: artist["url"].as_str().unwrap_or("").to_owned(),
    }
}

/// Last.fm's biography HTML as plain text: without its tags and its "Read more on Last.fm"
/// link, entities decoded, and paragraphs kept.
fn plain_text(html: &str) -> String {
    let html = match html.find("<a href=\"https://www.last.fm") {
        Some(link) => &html[..link],
        None => html,
    };
    let mut text = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = text
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let paragraphs: Vec<&str> = text
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    paragraphs.join("\n\n")
}

/// Last.fm's signature: every parameter but `format` and `callback`, sorted by name, each
/// name followed by its value, then the shared secret, through MD5.
fn signature(params: &[(&str, &str)], secret: &str) -> String {
    let mut sorted: Vec<&(&str, &str)> = params
        .iter()
        .filter(|(name, _)| *name != "format" && *name != "callback")
        .collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut text = String::new();
    for (name, value) in sorted {
        text.push_str(name);
        text.push_str(value);
    }
    text.push_str(secret);
    md5::hex(text.as_bytes())
}

/// The string at `path` in a JSON answer.
fn text(answer: &Value, path: &[&str]) -> Result<String, Error> {
    let mut value = answer;
    for key in path {
        value = value
            .get(key)
            .ok_or_else(|| Error::Network(format!("Last.fm's answer has no {}", path.join("."))))?;
    }
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::Network(format!("Last.fm's answer has no {}", path.join("."))))
}

/// Percent-encodes `value` for a URL's query.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_artists_biography_as_plain_text() {
        let answer = serde_json::json!({ "artist": {
            "name": "Ane Brun",
            "url": "https://www.last.fm/music/Ane+Brun",
            "tags": { "tag": [ { "name": "female vocalists" }, { "name": "folk" } ] },
            "bio": {
                "summary": "Ane Brun is a &quot;Norwegian&quot; singer &amp; songwriter. <a href=\"https://www.last.fm/music/Ane+Brun\">Read more on Last.fm</a>",
                "content": "First paragraph.\n\n<b>Second</b> paragraph.\n\n <a href=\"https://www.last.fm/music/Ane+Brun\">Read more on Last.fm</a>. User-contributed text"
            }
        }});
        let info = artist_info(&answer);
        assert_eq!(
            info.summary,
            "Ane Brun is a \"Norwegian\" singer & songwriter."
        );
        assert_eq!(info.biography, "First paragraph.\n\nSecond paragraph.");
        assert_eq!(info.tags, ["female vocalists", "folk"]);
        assert_eq!(info.url, "https://www.last.fm/music/Ane+Brun");

        // One tag alone is an object, not a list.
        let one = serde_json::json!({ "artist": { "tags": { "tag": { "name": "folk" } } } });
        assert_eq!(artist_info(&one).tags, ["folk"]);
    }

    #[test]
    fn signs_as_last_fm_describes() {
        // Sorted by name, `format` left out, the secret last.
        let params = [
            ("method", "auth.getSession"),
            ("token", "abc"),
            ("api_key", "key"),
            ("format", "json"),
        ];
        assert_eq!(
            signature(&params, "secret"),
            md5::hex(b"api_keykeymethodauth.getSessiontokenabcsecret")
        );
    }

    #[test]
    fn tells_errors_apart() {
        assert!(Error::Network("down".into()).is_temporary());
        assert!(Error::Api(29, "rate limit".into()).is_temporary());
        assert!(!Error::Api(9, "invalid session".into()).is_temporary());
        assert!(Error::Api(9, "invalid session".into()).is_session_invalid());
        assert!(Error::Api(14, "not authorized".into()).is_unauthorized_token());
        assert!(Error::Api(6, "invalid parameters".into()).is_invalid_play());
        // A refusal Freya does not know is neither dropped as invalid nor taken as temporary.
        assert!(!Error::Api(13, "invalid signature".into()).is_invalid_play());
        assert!(!Error::Api(13, "invalid signature".into()).is_temporary());
    }

    #[test]
    fn encodes_query_values() {
        assert_eq!(encode("a b/c"), "a%20b%2Fc");
    }
}
