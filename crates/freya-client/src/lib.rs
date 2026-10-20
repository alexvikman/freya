//! A library elsewhere, as Freya's client asks of it: over HTTP, from a Freya server.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use freya_protocol::{API_PATH, ApiError, ApiResult, LibraryApi, Remote, Transport};

/// The largest answer taken: every track of a large library, as JSON.
const MAX_ANSWER: u64 = 512 * 1024 * 1024;

/// The library of the server at `address`, such as `http://127.0.0.1:47474`.
pub fn connect(address: &str) -> Arc<dyn LibraryApi> {
    Arc::new(Remote(Http::new(address)))
}

/// Calls carried over HTTP.
pub struct Http {
    agent: ureq::Agent,
    base: String,
}

impl Http {
    pub fn new(address: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(120)))
            .user_agent(concat!("Freya/", env!("CARGO_PKG_VERSION")))
            // Errors come with a body that says what went wrong.
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            base: format!("{}{API_PATH}", address.trim_end_matches('/')),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

fn failed(e: impl std::fmt::Display) -> ApiError {
    ApiError(format!("could not reach the server: {e}"))
}

type Answer = ureq::http::Response<ureq::Body>;

/// The body of `answer`; what went wrong when it is not one of `wanted`'s statuses.
fn read(mut answer: Answer) -> ApiResult<(u16, Vec<u8>)> {
    let status = answer.status().as_u16();
    let body = answer
        .body_mut()
        .with_config()
        .limit(MAX_ANSWER)
        .read_to_vec()
        .map_err(failed)?;
    Ok((status, body))
}

/// What the server said went wrong in `body`, answered with `status`.
fn error(status: u16, body: &[u8]) -> ApiError {
    let said = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(str::to_owned));
    ApiError(said.unwrap_or_else(|| format!("the server answered {status}")))
}

/// The bytes of `answer`, none when the server has none (404).
fn bytes_or_none(answer: Answer) -> ApiResult<Option<Vec<u8>>> {
    match read(answer)? {
        (200, body) => Ok(Some(body)),
        (404, _) => Ok(None),
        (status, body) => Err(error(status, &body)),
    }
}

/// The JSON of `answer`.
fn json(answer: Answer) -> ApiResult<serde_json::Value> {
    match read(answer)? {
        (200, body) => serde_json::from_slice(&body).map_err(failed),
        (status, body) => Err(error(status, &body)),
    }
}

impl Transport for Http {
    fn call(&self, method: &str, args: serde_json::Value) -> ApiResult<serde_json::Value> {
        let answer = self
            .agent
            .post(&self.url(&format!("/call/{method}")))
            .header("Content-Type", "application/json")
            .send(args.to_string())
            .map_err(failed)?;
        json(answer)
    }

    fn picture(&self, checksum: &str) -> ApiResult<Option<Vec<u8>>> {
        if !freya_protocol::is_checksum(checksum) {
            return Ok(None);
        }
        let answer = self
            .agent
            .get(&self.url(&format!("/pictures/{checksum}")))
            .call()
            .map_err(failed)?;
        bytes_or_none(answer)
    }

    fn keep_picture(&self, bytes: &[u8], largest: Option<u32>) -> ApiResult<String> {
        let mut request = self.agent.post(&self.url("/pictures"));
        if let Some(largest) = largest {
            request = request.query("largest", largest.to_string());
        }
        let answer = request.send(bytes).map_err(failed)?;
        serde_json::from_value(json(answer)?).map_err(failed)
    }

    fn album_art(&self, track: &Path, largest: u32) -> ApiResult<Option<Vec<u8>>> {
        let asked = serde_json::json!({ "track": track, "largest": largest });
        let answer = self
            .agent
            .post(&self.url("/album-art"))
            .header("Content-Type", "application/json")
            .send(asked.to_string())
            .map_err(failed)?;
        bytes_or_none(answer)
    }
}

#[cfg(test)]
mod tests;
