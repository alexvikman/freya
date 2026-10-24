//! Verifies release signatures made with `ssh-keygen -Y sign` (OpenSSH's SSHSIG format, with
//! Ed25519 keys), so Freya only trusts a release's `SHA256SUMS` if it was signed with a key
//! built into Freya.
//!
//! The private key lives in a GitHub environment that needs the owner's approval: someone who
//! can push to the repository or edit a release cannot make a valid signature.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// The namespace releases are signed in (`ssh-keygen -n`), so a signature made with the same
/// key for anything else is not taken for one.
pub const NAMESPACE: &str = "freya-release";

/// Public keys whose signatures Freya accepts for its releases, as in an OpenSSH `.pub` file.
/// More than one allows a key to be replaced: releases signed with the new key are accepted by
/// Freyas that know both.
pub const RELEASE_KEYS: &[&str] = &[
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGb2PDcF4r6PkdFcm4pzbCxzZPZuDls5ZJSICMFe91Bd freya-release",
];

/// Reads an SSH wire-format `string`: a u32 length, then that many bytes.
fn take<'a>(data: &mut &'a [u8]) -> Result<&'a [u8]> {
    let Some((len, rest)) = data.split_first_chunk::<4>() else {
        bail!("the signature is cut short");
    };
    let len = u32::from_be_bytes(*len) as usize;
    if rest.len() < len {
        bail!("the signature is cut short");
    }
    let (value, tail) = rest.split_at(len);
    *data = tail;
    Ok(value)
}

fn put(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

/// The raw 32-byte Ed25519 key of an OpenSSH public key line.
fn key_from_line(line: &str) -> Result<Vec<u8>> {
    let mut parts = line.split_whitespace();
    let (Some("ssh-ed25519"), Some(base64)) = (parts.next(), parts.next()) else {
        bail!("not an ssh-ed25519 public key");
    };
    let blob = STANDARD
        .decode(base64)
        .context("the public key is not valid base64")?;
    key_from_blob(&blob)
}

fn key_from_blob(blob: &[u8]) -> Result<Vec<u8>> {
    let mut data = blob;
    if take(&mut data)? != b"ssh-ed25519" {
        bail!("only ssh-ed25519 keys are supported");
    }
    let key = take(&mut data)?;
    if key.len() != 32 {
        bail!("an Ed25519 key has 32 bytes");
    }
    Ok(key.to_vec())
}

/// Checks an armored SSH signature over `message`, made in `namespace` with one of `allowed`.
pub fn verify(message: &[u8], armored: &str, namespace: &str, allowed: &[&str]) -> Result<()> {
    if allowed.is_empty() {
        bail!("this build of Freya has no release key, so it cannot check updates");
    }
    let base64: String = armored
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("-----"))
        .collect();
    let blob = STANDARD
        .decode(base64)
        .context("the signature is not valid base64")?;

    // PROTOCOL.sshsig: "SSHSIG", u32 version, then the strings publickey, namespace, reserved,
    // hash_algorithm and signature.
    let Some(mut data) = blob.strip_prefix(b"SSHSIG") else {
        bail!("not an SSH signature");
    };
    let Some((version, rest)) = data.split_first_chunk::<4>() else {
        bail!("the signature is cut short");
    };
    if u32::from_be_bytes(*version) != 1 {
        bail!("unsupported SSH signature version");
    }
    data = rest;
    let public_key = key_from_blob(take(&mut data)?)?;
    let signed_namespace = take(&mut data)?;
    let reserved = take(&mut data)?;
    let hash_algorithm = take(&mut data)?;
    let mut signature = take(&mut data)?;

    if signed_namespace != namespace.as_bytes() {
        bail!(
            "the signature is for {:?}, not {namespace:?}",
            String::from_utf8_lossy(signed_namespace)
        );
    }
    let trusted = allowed
        .iter()
        .filter_map(|line| key_from_line(line).ok())
        .any(|key| key == public_key);
    if !trusted {
        bail!("signed with a key Freya does not trust");
    }
    if take(&mut signature)? != b"ssh-ed25519" {
        bail!("only ssh-ed25519 signatures are supported");
    }
    let raw_signature = take(&mut signature)?;

    let digest = match hash_algorithm {
        b"sha512" => ring::digest::digest(&ring::digest::SHA512, message),
        b"sha256" => ring::digest::digest(&ring::digest::SHA256, message),
        other => bail!(
            "unsupported hash algorithm {:?}",
            String::from_utf8_lossy(other)
        ),
    };
    let mut signed = Vec::with_capacity(128);
    signed.extend_from_slice(b"SSHSIG");
    put(&mut signed, namespace.as_bytes());
    put(&mut signed, reserved);
    put(&mut signed, hash_algorithm);
    put(&mut signed, digest.as_ref());

    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &public_key)
        .verify(&signed, raw_signature)
        .map_err(|_| anyhow::anyhow!("the signature does not match"))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A key made for these tests only, and a signature made with it (`ssh-keygen -Y sign -n
    /// freya-release`). Not a release key: its private half was thrown away.
    pub const TEST_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHiGEPxpGMr0wfGaNueMTQ0O8fFfv/qSfQFqkr1tn8rz freya-test";
    const MESSAGE: &[u8] = b"abc123  Freya-0.2.0-macos-aarch64.app.zip\n";
    const SIGNATURE: &str = "-----BEGIN SSH SIGNATURE-----
U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAgeIYQ/GkYyvTB8Zo254xNDQ7x8V
+/+pJ9AWqSvW2fyvMAAAANZnJleWEtcmVsZWFzZQAAAAAAAAAGc2hhNTEyAAAAUwAAAAtz
c2gtZWQyNTUxOQAAAECfO0Y8290tnqg7SmGs9SxLtoMjs45CCyCA7G86XOtPKcUCvHk9z3
xcP61CPgOwOVIBNUtfu4L7oB6adYtK51EC
-----END SSH SIGNATURE-----
";
    /// Another key, to sign nothing.
    const OTHER_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILV3WD7EZCoPz5o0BfxJJkXKKsj4I8/b3uIxKxmhhrBZ other";

    #[test]
    fn accepts_a_valid_signature() {
        verify(MESSAGE, SIGNATURE, NAMESPACE, &[TEST_KEY]).unwrap();
        verify(MESSAGE, SIGNATURE, NAMESPACE, &[OTHER_KEY, TEST_KEY]).unwrap();
    }

    #[test]
    fn refuses_changes_and_other_keys() {
        let changed = b"ffffff  Freya-0.2.0-macos-aarch64.app.zip\n";
        assert!(verify(changed, SIGNATURE, NAMESPACE, &[TEST_KEY]).is_err());
        assert!(verify(MESSAGE, SIGNATURE, NAMESPACE, &[OTHER_KEY]).is_err());
        assert!(verify(MESSAGE, SIGNATURE, "file", &[TEST_KEY]).is_err());
        assert!(verify(MESSAGE, SIGNATURE, NAMESPACE, &[]).is_err());
        assert!(verify(MESSAGE, "garbage", NAMESPACE, &[TEST_KEY]).is_err());
        // Cut short anywhere.
        let armored: String = SIGNATURE.lines().take(3).collect::<Vec<_>>().join("\n");
        assert!(verify(MESSAGE, &armored, NAMESPACE, &[TEST_KEY]).is_err());
    }

    #[test]
    fn release_keys_parse() {
        for key in RELEASE_KEYS {
            key_from_line(key).unwrap();
        }
    }
}
