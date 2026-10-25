//! Radio Browser (radio-browser.info), the directory of internet radio stations anyone may add
//! to and read, without a key: its stations, fetched a page at a time from one of its servers,
//! and the stations played, which it counts to know the best known.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// Where Radio Browser's servers are listed. It asks programs to find them so, and to try the
/// next when one does not answer.
const SERVERS: &str = "https://all.api.radio-browser.info/json/servers";
/// The servers to try when the list cannot be had.
const KNOWN_SERVERS: &[&str] = &[
    "de1.api.radio-browser.info",
    "de2.api.radio-browser.info",
    "fi1.api.radio-browser.info",
];
/// How many stations are asked for at once: some 6 MB.
const PAGE: usize = 5000;
/// A page is at most this large, to keep a broken answer from filling memory.
const MAX_PAGE_BYTES: u64 = 64 * 1024 * 1024;
/// At most this many pages: Radio Browser has some 60,000 stations that work.
const MOST_PAGES: usize = 40;
const TIMEOUT: Duration = Duration::from_secs(60);
/// Radio Browser asks programs to say who they are.
const USER_AGENT: &str = concat!(
    "FreyaServer/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/alexvikman/freya )"
);

/// A station as Radio Browser lists it, with only what Freya keeps of it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Listed {
    pub stationuuid: String,
    pub name: String,
    pub url: String,
    /// The stream `url` leads to, as Radio Browser found it.
    pub url_resolved: String,
    pub homepage: String,
    pub favicon: String,
    /// Its own tags, apart by commas, as whoever added it wrote them.
    pub tags: String,
    pub countrycode: String,
    pub country: String,
    pub languagecodes: String,
    pub codec: String,
    pub bitrate: i64,
    pub hls: i64,
    pub lastcheckok: i64,
    pub clickcount: i64,
    pub votes: i64,
}

/// Radio Browser's stations, as fetched at some time: what is kept in the server's folder.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Fetched {
    pub fetched: Option<SystemTime>,
    pub stations: Vec<Listed>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent(USER_AGENT)
        .build()
        .into()
}

/// Radio Browser's servers, as "https://de1.api.radio-browser.info", in an order of their own
/// each time, so not every server asks the same one.
pub fn servers() -> Vec<String> {
    let listed: Vec<String> = agent()
        .get(SERVERS)
        .call()
        .ok()
        .and_then(|mut answer| answer.body_mut().read_to_string().ok())
        .and_then(|text| serde_json::from_str::<Vec<Server>>(&text).ok())
        .map(|servers| servers.into_iter().map(|s| s.name).collect())
        .unwrap_or_default();
    let mut names: Vec<String> = if listed.is_empty() {
        KNOWN_SERVERS.iter().map(|s| (*s).to_owned()).collect()
    } else {
        listed
    };
    names.retain(|name| is_server_name(name));
    names.sort();
    names.dedup();
    if !names.is_empty() {
        let turn = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() as usize);
        let len = names.len();
        names.rotate_left(turn % len);
    }
    names
        .into_iter()
        .map(|name| format!("https://{name}"))
        .collect()
}

#[derive(Deserialize)]
struct Server {
    name: String,
}

/// Whether `name` is one of Radio Browser's servers, as its list may name: the list is asked
/// over HTTPS, but a name is only ever put in an address of Radio Browser's own.
fn is_server_name(name: &str) -> bool {
    name.strip_suffix(".api.radio-browser.info")
        .is_some_and(|host| {
            !host.is_empty() && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// Every station of Radio Browser that worked when it last looked, from the first of `servers`
/// that gives them all; the one that did, and them. Gives up when `stop` is set.
pub fn fetch(servers: &[String], stop: &AtomicBool) -> Result<(String, Vec<Listed>), String> {
    let mut why = "no server of Radio Browser could be found".to_owned();
    for server in servers {
        match fetch_from(server, PAGE, stop) {
            Ok(stations) => return Ok((server.clone(), stations)),
            Err(e) => {
                log::warn!("could not fetch the radio stations from {server}: {e}");
                why = e;
            }
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
    }
    Err(why)
}

/// Every station of `server` that works, `page` at a time.
pub fn fetch_from(server: &str, page: usize, stop: &AtomicBool) -> Result<Vec<Listed>, String> {
    let agent = agent();
    let mut stations: Vec<Listed> = Vec::new();
    for at in 0..MOST_PAGES {
        if stop.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        let url = format!(
            "{server}/json/stations?hidebroken=true&order=stationuuid&limit={page}&offset={}",
            at * page
        );
        let text = agent
            .get(&url)
            .call()
            .map_err(|e| e.to_string())?
            .body_mut()
            .with_config()
            .limit(MAX_PAGE_BYTES)
            .read_to_string()
            .map_err(|e| e.to_string())?;
        let got: Vec<Listed> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let last = got.len() < page;
        stations.extend(got);
        if last {
            return Ok(stations);
        }
    }
    Ok(stations)
}

/// Tells `server` that the station `id` was played, as Radio Browser asks programs to.
pub fn count_play(server: &str, id: &str) {
    if !is_station_id(id) {
        return;
    }
    if let Err(e) = agent().get(format!("{server}/json/url/{id}")).call() {
        log::debug!("could not tell Radio Browser of a play: {e}");
    }
}

/// Whether `id` is one of Radio Browser's: a UUID.
pub fn is_station_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    use super::*;

    /// A server that answers each request for stations from `stations`, as Radio Browser does.
    fn serve(stations: Vec<Listed>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let mut reader = BufReader::new(&client);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                }
                let number = |name: &str| -> usize {
                    line.split(['?', '&', ' '])
                        .find_map(|part| part.strip_prefix(name))
                        .and_then(|n| n.parse().ok())
                        .unwrap()
                };
                let (limit, offset) = (number("limit="), number("offset="));
                let page: Vec<&Listed> = stations.iter().skip(offset).take(limit).collect();
                let body = serde_json::to_string(&page).unwrap();
                let mut client = &client;
                write!(
                    client,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        format!("http://{address}")
    }

    #[test]
    fn fetches_every_page() {
        let stations: Vec<Listed> = (0..5)
            .map(|i| Listed {
                stationuuid: format!("{i}"),
                name: format!("Station {i}"),
                ..Listed::default()
            })
            .collect();
        let server = serve(stations.clone());
        let stop = AtomicBool::new(false);
        assert_eq!(fetch_from(&server, 2, &stop).unwrap(), stations);
        assert_eq!(fetch_from(&server, 5, &stop).unwrap(), stations);
        stop.store(true, Ordering::Relaxed);
        assert!(fetch_from(&server, 2, &stop).is_err());
    }

    #[test]
    fn only_radio_browsers_servers_and_stations_are_asked() {
        assert!(is_server_name("de1.api.radio-browser.info"));
        assert!(!is_server_name("evil.example.org"));
        assert!(!is_server_name("evil.example.org/.api.radio-browser.info"));
        assert!(!is_server_name(".api.radio-browser.info"));
        assert!(is_station_id("96062a7b-0601-11e8-ae97-52543be04c81"));
        assert!(!is_station_id("../../json/stations"));
        assert!(!is_station_id("freya:radio-paradise"));
    }
}
