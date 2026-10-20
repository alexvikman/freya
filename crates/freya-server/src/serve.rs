//! The library's API over HTTP, under [`API_PATH`]:
//!
//! - `POST /call/{method}`: a call of [`freya_protocol::LibraryData`], its arguments in the
//!   body as JSON, answered in JSON; `{"error": ...}` when it failed.
//! - `GET /pictures/{checksum}`: a picture the library keeps.
//! - `POST /pictures`, with `?largest=` to make it smaller: keeps the picture in the body,
//!   answering its checksum.
//! - `POST /album-art`: the cover of the album whose track `{"track", "largest"}` is.

use std::sync::Arc;

use freya_protocol::{API_PATH, LibraryApi, dispatch};

use crate::http::{Handler, Request, Response};

/// Answers requests to the API from `api`.
pub fn handler(api: Arc<dyn LibraryApi>) -> Handler {
    Arc::new(move |request| answer(api.as_ref(), request))
}

fn answer(api: &dyn LibraryApi, request: Request) -> Response {
    let Some(path) = request.path.strip_prefix(API_PATH) else {
        return Response::error(404, "no such page");
    };
    match (request.method.as_str(), path) {
        ("POST", path) if path.starts_with("/call/") => {
            let method = &path["/call/".len()..];
            let args = match serde_json::from_slice(&request.body) {
                Ok(args) => args,
                Err(e) => return Response::error(400, e),
            };
            match dispatch(api, method, args) {
                None => Response::error(404, format!("no such call: {method}")),
                Some(Ok(answer)) => Response::json(200, &answer),
                Some(Err(e)) => Response::error(500, e),
            }
        }
        ("GET", path) if path.starts_with("/pictures/") => {
            match api.picture(&path["/pictures/".len()..]) {
                Ok(Some(bytes)) => Response::bytes(200, "image/jpeg", bytes),
                Ok(None) => Response::error(404, "no such picture"),
                Err(e) => Response::error(500, e),
            }
        }
        ("POST", "/pictures") => {
            let largest = match request.query_value("largest").map(str::parse::<u32>) {
                None => None,
                Some(Ok(largest)) => Some(largest),
                Some(Err(e)) => return Response::error(400, e),
            };
            let kept = match largest {
                Some(largest) => api.take_picture(&request.body, largest),
                None => api.put_picture(&request.body),
            };
            match kept {
                Ok(checksum) => Response::json(200, &serde_json::Value::String(checksum)),
                Err(e) => Response::error(500, e),
            }
        }
        ("POST", "/album-art") => {
            #[derive(serde::Deserialize)]
            struct Asked {
                track: std::path::PathBuf,
                largest: u32,
            }
            let asked: Asked = match serde_json::from_slice(&request.body) {
                Ok(asked) => asked,
                Err(e) => return Response::error(400, e),
            };
            match api.album_art(&asked.track, asked.largest) {
                Ok(Some(bytes)) => Response::bytes(200, "image/jpeg", bytes),
                Ok(None) => Response::error(404, "no cover"),
                Err(e) => Response::error(500, e),
            }
        }
        _ => Response::error(404, "no such page"),
    }
}
