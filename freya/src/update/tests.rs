//! The whole update: a release list, checksums and a package served from a local web server,
//! signed with a key made for the test, and installed as an AppImage (one file, the same on
//! every system).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ring::signature::{Ed25519KeyPair, KeyPair};

use super::*;

/// A local web server for `files` (path → body), until the test ends.
struct Server {
    listener: TcpListener,
}

impl Server {
    fn new() -> Self {
        Self {
            listener: TcpListener::bind("127.0.0.1:0").unwrap(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.listener.local_addr().unwrap())
    }

    fn serve(self, files: Vec<(String, Vec<u8>)>) {
        let files = Arc::new(files);
        std::thread::spawn(move || {
            for stream in self.listener.incoming().flatten() {
                let files = Arc::clone(&files);
                std::thread::spawn(move || respond(&stream, &files));
            }
        });
    }
}

fn respond(stream: &std::net::TcpStream, files: &[(String, Vec<u8>)]) {
    let mut reader = BufReader::new(stream);
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
    // The headers end with an empty line.
    let mut header = String::new();
    while reader.read_line(&mut header).unwrap_or(0) > 2 {
        header.clear();
    }
    let mut stream = stream;
    let _ = match files.iter().find(|(p, _)| *p == path) {
        Some((_, body)) => stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .and_then(|()| stream.write_all(body)),
        None => stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
    };
}

/// An Ed25519 key made for one test, and its public half as an OpenSSH line.
struct Key {
    pair: Ed25519KeyPair,
    line: &'static str,
}

impl Key {
    fn new() -> Self {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let line = format!("ssh-ed25519 {} test", STANDARD.encode(public_blob(&pair)));
        Self {
            pair,
            line: Box::leak(line.into_boxed_str()),
        }
    }

    /// An armored SSH signature over `message`, as `ssh-keygen -Y sign -n freya-release` makes.
    fn sign(&self, message: &[u8]) -> String {
        let hash = ring::digest::digest(&ring::digest::SHA512, message);
        let mut signed = b"SSHSIG".to_vec();
        for part in [signing::NAMESPACE.as_bytes(), b"", b"sha512", hash.as_ref()] {
            string(&mut signed, part);
        }
        let mut signature = Vec::new();
        string(&mut signature, b"ssh-ed25519");
        string(&mut signature, self.pair.sign(&signed).as_ref());
        let mut blob = b"SSHSIG".to_vec();
        blob.extend_from_slice(&1u32.to_be_bytes());
        let public = public_blob(&self.pair);
        for part in [
            &public[..],
            signing::NAMESPACE.as_bytes(),
            b"",
            b"sha512",
            &signature,
        ] {
            string(&mut blob, part);
        }
        format!(
            "-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n",
            STANDARD.encode(blob)
        )
    }
}

fn public_blob(pair: &Ed25519KeyPair) -> Vec<u8> {
    let mut blob = Vec::new();
    string(&mut blob, b"ssh-ed25519");
    string(&mut blob, pair.public_key().as_ref());
    blob
}

fn string(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Serves a release 9.0.0 with `package` as the AppImage, and a SHA256SUMS listing `listed`,
/// signed by `signer`; next to a draft, a pre-release and other tags that must be passed over.
/// Returns the list's URL.
fn release_server(package: &[u8], listed: &[u8], signer: &Key) -> String {
    let file = Target::AppImage(PathBuf::from("/x/Freya.AppImage")).asset("9.0.0");
    let sums = format!("{}  {file}\n", sha256_hex(listed));
    let signature = signer.sign(sums.as_bytes());
    let server = Server::new();
    let asset = |name: &str| serde_json::json!({ "name": name, "browser_download_url": server.url(&format!("/files/{name}")) });
    let releases = serde_json::json!([
        { "tag_name": "v10.0.0", "draft": true, "html_url": "https://example.com", "assets": [] },
        { "tag_name": "v11.0.0", "prerelease": true, "html_url": "https://example.com", "assets": [] },
        { "tag_name": "nightly", "html_url": "https://example.com", "assets": [] },
        { "tag_name": "v9.0.0", "body": "## New\n- **Faster**", "html_url": "https://example.com/v9",
          "assets": [asset(&file), asset("SHA256SUMS"), asset("SHA256SUMS.sig")] },
        { "tag_name": "v0.0.1", "html_url": "https://example.com", "assets": [] },
    ]);
    let feed = server.url("/releases");
    server.serve(vec![
        ("/releases".to_owned(), releases.to_string().into_bytes()),
        (format!("/files/{file}"), package.to_vec()),
        ("/files/SHA256SUMS".to_owned(), sums.into_bytes()),
        ("/files/SHA256SUMS.sig".to_owned(), signature.into_bytes()),
    ]);
    feed
}

fn wait(updater: &mut Updater) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while updater.busy() {
        updater.poll();
        assert!(Instant::now() < deadline, "the updater never finished");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn temp_image(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("freya-update-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let image = dir.join("Freya.AppImage");
    std::fs::write(&image, b"old Freya").unwrap();
    image
}

fn test_updater(feed: String, key: &Key, image: &std::path::Path) -> Updater {
    let keys: &'static [&'static str] = Box::leak(vec![key.line].into_boxed_slice());
    Updater::with(feed, keys, Some(Target::AppImage(image.to_owned())))
}

#[test]
fn finds_verifies_and_installs_a_release() {
    let key = Key::new();
    let feed = release_server(b"new Freya", b"new Freya", &key);
    let image = temp_image("good");
    let mut updater = test_updater(feed, &key, &image);

    // Not offered on Freya's own check once skipped; offered when the user asks.
    updater.check(false, Some("9.0.0"));
    wait(&mut updater);
    assert!(matches!(updater.state(), State::Idle));
    updater.check(true, Some("9.0.0"));
    wait(&mut updater);
    let State::Available(release) = updater.state() else {
        panic!("9.0.0 is found, not {:?}", updater.state());
    };
    assert_eq!(release.version(), "9.0.0");
    assert_eq!(
        release::plain_notes(release.body.as_deref().unwrap()),
        "New\n• Faster"
    );

    updater.install();
    wait(&mut updater);
    assert!(
        matches!(updater.state(), State::Installed(_)),
        "{:?}",
        updater.state()
    );
    assert_eq!(std::fs::read(&image).unwrap(), b"new Freya");
}

#[test]
fn refuses_a_changed_package_or_another_key() {
    // The package is not the one SHA256SUMS lists.
    let key = Key::new();
    let feed = release_server(b"evil Freya", b"new Freya", &key);
    let image = temp_image("changed");
    let mut updater = test_updater(feed, &key, &image);
    updater.check(true, None);
    wait(&mut updater);
    updater.install();
    wait(&mut updater);
    let State::Failed(error) = updater.state() else {
        panic!("refused, not {:?}", updater.state());
    };
    assert!(error.contains("not the file that was released"), "{error}");
    assert_eq!(std::fs::read(&image).unwrap(), b"old Freya");

    // SHA256SUMS signed with a key Freya does not know.
    let stranger = Key::new();
    let feed = release_server(b"new Freya", b"new Freya", &stranger);
    let image = temp_image("stranger");
    let mut updater = test_updater(feed, &key, &image);
    updater.check(true, None);
    wait(&mut updater);
    updater.install();
    wait(&mut updater);
    let State::Failed(error) = updater.state() else {
        panic!("refused, not {:?}", updater.state());
    };
    assert!(
        error.contains("not signed by Freya's release key"),
        "{error}"
    );
    assert_eq!(std::fs::read(&image).unwrap(), b"old Freya");
}

#[test]
fn checks_once_a_day() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 24 * 60 * 60);
    let secs = |d: u64| Some(10 * 24 * 60 * 60 - d);
    assert!(due(None, now));
    assert!(!due(secs(60 * 60), now));
    assert!(due(secs(24 * 60 * 60), now));
}

#[test]
fn opens_only_plain_https_pages_and_freyas_own() {
    assert!(is_openable(
        "https://www.last.fm/api/auth/?api_key=k&token=t"
    ));
    assert!(is_openable(
        "http://127.0.0.1:47477/web/password?start=ab12"
    ));
    assert!(!is_openable("http://example.com/"));
    assert!(!is_openable("http://127.0.0.1:5555/"));
    assert!(!is_openable("file:///etc/passwd"));
    assert!(!is_openable("https://example.com/a b"));
    assert!(!is_openable("https://example.com/a,b"));
    assert!(!is_openable("https://example.com/\"--args"));
}
