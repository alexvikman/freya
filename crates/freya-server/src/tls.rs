//! The server's own certificate, which clients on other computers pin the first time they
//! connect: self-signed, as no authority vouches for a computer at home, with an ECDSA P-256
//! key, which every TLS library takes, made once and kept in the server's folder, where only
//! the user can read it.
//!
//! It names the server [`TLS_NAME`], whatever its address on the network: a client connects
//! to that name and looks it up as the server's address itself, so the certificate holds
//! when the address changes. It does not expire: a client trusts it because it pinned it,
//! not because of its dates.

use std::path::Path;
use std::sync::Arc;

use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};

use crate::accounts::write_private;

/// What a client calls the server in TLS, whatever its address.
pub const TLS_NAME: &str = "freya-server.invalid";

/// The server's certificate, in DER, and its private key, in PKCS #8.
pub struct Identity {
    pub certificate: Vec<u8>,
    pub key: Vec<u8>,
}

impl Identity {
    /// The identity kept in `dir`, made there the first time.
    pub fn load_or_make(dir: &Path) -> Result<Self, String> {
        let (certificate, key) = (dir.join("tls-certificate.der"), dir.join("tls-key.pk8"));
        if let (Ok(cert), Ok(key)) = (std::fs::read(&certificate), std::fs::read(&key)) {
            return Ok(Self {
                certificate: cert,
                key,
            });
        }
        let identity = Self::make()?;
        write_private(&key, &identity.key).map_err(|e| e.to_string())?;
        write_private(&certificate, &identity.certificate).map_err(|e| e.to_string())?;
        log::info!("made the server's certificate");
        Ok(identity)
    }

    /// A new key, and a certificate of it signed by itself.
    pub fn make() -> Result<Self, String> {
        let random = SystemRandom::new();
        let algorithm = &ECDSA_P256_SHA256_ASN1_SIGNING;
        let key = EcdsaKeyPair::generate_pkcs8(algorithm, &random).map_err(|e| e.to_string())?;
        let pair = EcdsaKeyPair::from_pkcs8(algorithm, key.as_ref(), &random)
            .map_err(|e| e.to_string())?;
        let mut serial = [0; 16];
        ring::rand::SecureRandom::fill(&random, &mut serial).map_err(|e| e.to_string())?;
        // Positive, as a serial number must be.
        serial[0] &= 0x7f;
        let tbs = tbs_certificate(&serial, pair.public_key().as_ref());
        let signature = pair.sign(&random, &tbs).map_err(|e| e.to_string())?;
        let mut signed = bit_string(signature.as_ref());
        let mut certificate = tbs;
        certificate.extend(ecdsa_with_sha256());
        certificate.append(&mut signed);
        Ok(Self {
            certificate: der(0x30, &certificate),
            key: key.as_ref().to_vec(),
        })
    }

    /// For the server's TLS.
    pub fn server_config(&self) -> Result<Arc<rustls::ServerConfig>, String> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            // Freya's clients all speak TLS 1.3: nothing older is offered.
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_single_cert(
                vec![rustls::pki_types::CertificateDer::from(
                    self.certificate.clone(),
                )],
                rustls::pki_types::PrivateKeyDer::Pkcs8(self.key.clone().into()),
            )
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(config))
    }

    /// What tells this certificate from others, for the user to compare: its SHA-256, in
    /// groups of four hex digits.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.certificate)
    }
}

/// The SHA-256 of a certificate in DER, in groups of four hex digits.
pub fn fingerprint(certificate: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, certificate);
    let hex: Vec<String> = digest
        .as_ref()
        .chunks(2)
        .map(|pair| pair.iter().map(|b| format!("{b:02X}")).collect())
        .collect();
    hex.join(" ")
}

/// The certificate's contents, as RFC 5280 lays them out.
fn tbs_certificate(serial: &[u8], public_key: &[u8]) -> Vec<u8> {
    let name = der(
        0x30,
        &der(
            0x31,
            &der(
                0x30,
                &[oid(&[0x55, 0x04, 0x03]), der(0x0c, b"Freya server")].concat(),
            ),
        ),
    );
    let validity = der(
        0x30,
        &[
            der(0x17, b"200101000000Z"),
            // No well-defined end, as RFC 5280 writes it.
            der(0x18, b"99991231235959Z"),
        ]
        .concat(),
    );
    // id-ecPublicKey on the P-256 curve, and the point.
    let key_algorithm = der(
        0x30,
        &[
            oid(&[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01]),
            oid(&[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07]),
        ]
        .concat(),
    );
    let public_key = der(0x30, &[key_algorithm, bit_string(public_key)].concat());
    let alt_names = der(0x30, &der(0x82, TLS_NAME.as_bytes()));
    let server_auth = der(
        0x30,
        &oid(&[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01]),
    );
    let extensions = der(
        0x30,
        &[
            der(
                0x30,
                &[oid(&[0x55, 0x1d, 0x11]), der(0x04, &alt_names)].concat(),
            ),
            der(
                0x30,
                &[oid(&[0x55, 0x1d, 0x25]), der(0x04, &server_auth)].concat(),
            ),
        ]
        .concat(),
    );
    der(
        0x30,
        &[
            // Version 3.
            der(0xa0, &der(0x02, &[0x02])),
            der(0x02, serial),
            ecdsa_with_sha256(),
            name.clone(),
            validity,
            name,
            public_key,
            der(0xa3, &extensions),
        ]
        .concat(),
    )
}

/// The identifier of ECDSA with SHA-256, the certificate's signature.
fn ecdsa_with_sha256() -> Vec<u8> {
    der(
        0x30,
        &oid(&[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02]),
    )
}

fn oid(encoded: &[u8]) -> Vec<u8> {
    der(0x06, encoded)
}

fn bit_string(bytes: &[u8]) -> Vec<u8> {
    der(0x03, &[&[0][..], bytes].concat())
}

/// `content` with its tag and length, in DER.
fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let length = content.len();
    if length < 0x80 {
        out.push(length as u8);
    } else {
        let bytes: Vec<u8> = length
            .to_be_bytes()
            .into_iter()
            .skip_while(|b| *b == 0)
            .collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
    out.extend_from_slice(content);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_lengths_as_der_does() {
        assert_eq!(der(0x04, &[1, 2]), [0x04, 2, 1, 2]);
        assert_eq!(der(0x04, &[0; 200])[..3], [0x04, 0x81, 200]);
        assert_eq!(der(0x04, &[0; 300])[..4], [0x04, 0x82, 0x01, 0x2c]);
    }

    #[test]
    fn the_certificate_is_kept_and_rustls_takes_it() {
        let dir = std::env::temp_dir().join(format!("freya-tls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let identity = Identity::load_or_make(&dir).unwrap();
        identity.server_config().unwrap();
        let again = Identity::load_or_make(&dir).unwrap();
        assert_eq!(again.certificate, identity.certificate);
        assert_eq!(identity.fingerprint().len(), 16 * 5 - 1);
        let other = Identity::make().unwrap();
        assert_ne!(other.fingerprint(), identity.fingerprint());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
