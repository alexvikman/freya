//! Freya's releases on GitHub: which one is newest, and its files, checked against the
//! release's `SHA256SUMS` after that list's signature is checked.

use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::signing;

/// Where Freya is released.
pub const REPOSITORY: &str = "alexvikman/freya";
/// Points Freya at another list of releases, in the shape of GitHub's, to try a release before
/// it is published. Its files must still be signed with a key built into Freya.
const FEED_VARIABLE: &str = "FREYA_UPDATE_FEED";
const MAX_FEED: u64 = 4 << 20;
const MAX_SUMS: u64 = 1 << 20;
const MAX_SIGNATURE: u64 = 64 << 10;
/// Far above any Freya package; stops a download that never ends.
const MAX_PACKAGE: u64 = 512 << 20;

#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    /// The release notes, in Markdown.
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    /// The release's page.
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

impl Release {
    /// `0.2.0` for the tag `v0.2.0`.
    pub fn version(&self) -> &str {
        version(&self.tag_name).unwrap_or(&self.tag_name)
    }

    fn asset(&self, name: &str) -> Result<&Asset> {
        self.assets
            .iter()
            .find(|asset| asset.name == name)
            .with_context(|| format!("the release has no {name}"))
    }
}

/// The version of a release tag: `0.2.0` for `v0.2.0`. Other tags are not Freya releases.
pub fn version(tag: &str) -> Option<&str> {
    let version = tag.strip_prefix('v')?;
    let parts: Vec<&str> = version.split('.').collect();
    let numbers = parts
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    (parts.len() == 3 && numbers).then_some(version)
}

fn version_key(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

/// Whether `latest` is a newer version than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    version_key(latest) > version_key(current)
}

/// The list of releases: GitHub's, or the one `FREYA_UPDATE_FEED` names.
pub fn feed_url() -> String {
    std::env::var(FEED_VARIABLE).unwrap_or_else(|_| {
        format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page=30")
    })
}

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(600)))
        .user_agent(concat!("Freya/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// The newest published release, leaving out drafts, pre-releases and tags that are not
/// versions.
pub fn latest(agent: &ureq::Agent, feed: &str) -> Result<Option<Release>> {
    let text = get(agent, feed, MAX_FEED, |_, _| {})?;
    let releases: Vec<Release> =
        serde_json::from_slice(&text).context("the list of releases is not as expected")?;
    Ok(releases
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease && version(&r.tag_name).is_some())
        .max_by_key(|r| version_key(r.version())))
}

/// Downloads `file` of `release`, after checking the signature of the release's `SHA256SUMS`
/// against `keys`, and then the file against its line there. `progress` is told the bytes
/// downloaded and, when known, the size.
pub fn download_verified(
    agent: &ureq::Agent,
    release: &Release,
    file: &str,
    keys: &[&str],
    progress: impl FnMut(u64, Option<u64>),
) -> Result<Vec<u8>> {
    let sums = get(
        agent,
        &release.asset("SHA256SUMS")?.browser_download_url,
        MAX_SUMS,
        |_, _| {},
    )?;
    let signature = get(
        agent,
        &release.asset("SHA256SUMS.sig")?.browser_download_url,
        MAX_SIGNATURE,
        |_, _| {},
    )?;
    let signature = String::from_utf8(signature).context("the signature is not text")?;
    signing::verify(&sums, &signature, signing::NAMESPACE, keys)
        .context("SHA256SUMS is not signed by Freya's release key")?;
    let sums = String::from_utf8(sums).context("SHA256SUMS is not text")?;
    let expected =
        checksum_for(&sums, file).with_context(|| format!("SHA256SUMS lists no {file}"))?;
    let bytes = get(
        agent,
        &release.asset(file)?.browser_download_url,
        MAX_PACKAGE,
        progress,
    )?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        bail!("{file} is not the file that was released (SHA-256 {actual}, expected {expected})");
    }
    Ok(bytes)
}

/// Downloads at most `limit` bytes.
fn get(
    agent: &ureq::Agent,
    url: &str,
    limit: u64,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<Vec<u8>> {
    let mut response = agent
        .get(url)
        .header(
            "Accept",
            "application/vnd.github+json, application/octet-stream",
        )
        .call()
        .with_context(|| format!("could not download {url}"))?;
    let size = response.body().content_length();
    let mut reader = response.body_mut().as_reader();
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 64 << 10];
    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.len() as u64 > limit {
            bail!("{url} is larger than expected");
        }
        progress(bytes.len() as u64, size);
    }
    Ok(bytes)
}

/// The checksum `SHA256SUMS` gives for `file`, from lines as `sha256sum` writes them.
fn checksum_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == file).then(|| hash.to_ascii_lowercase())
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Release notes as plain text: Markdown headings, emphasis and code marks taken out, list
/// items as bullets.
pub fn plain_notes(markdown: &str) -> String {
    markdown
        .lines()
        .map(|line| {
            let line = line.trim_end();
            let line = line.trim_start_matches('#').trim_start();
            let line = match line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
                Some(item) => format!("• {item}"),
                None => line.to_owned(),
            };
            line.replace("**", "").replace('`', "")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_versions_from_tags() {
        assert_eq!(version("v0.2.0"), Some("0.2.0"));
        assert_eq!(version("v10.0.12"), Some("10.0.12"));
        for tag in ["0.2.0", "v0.2", "v0.2.0-beta", "v0..1", "nightly"] {
            assert_eq!(version(tag), None, "{tag}");
        }
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
    }

    #[test]
    fn finds_checksums_and_flattens_notes() {
        let sums = "AB12  Freya-0.2.0-macos-aarch64.app.zip\ncd34 *Freya-0.2.0.dmg\n";
        assert_eq!(
            checksum_for(sums, "Freya-0.2.0-macos-aarch64.app.zip").as_deref(),
            Some("ab12")
        );
        assert_eq!(
            checksum_for(sums, "Freya-0.2.0.dmg").as_deref(),
            Some("cd34")
        );
        assert_eq!(checksum_for(sums, "Freya-0.2.0"), None);
        assert_eq!(
            plain_notes("## Changes\n\n- **Faster** search\n* `--version` works\n"),
            "Changes\n\n• Faster search\n• --version works"
        );
    }
}
