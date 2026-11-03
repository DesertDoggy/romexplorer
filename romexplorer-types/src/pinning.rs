//! Trusting one particular certificate, for talking to a romexplorer server over TLS.
//!
//! A romexplorer server makes its own self-signed certificate: there is no CA to check it
//! against, and its name is usually a DHCP address that can change. So a client checks the one
//! thing that is stable -- the certificate itself, by its SHA-256 -- the way SSH checks a host
//! key: the first connection shows the fingerprint and remembers it, and every later one must
//! present the same certificate.
//!
//! The handshake signatures are still verified against that certificate's key, so pinning
//! replaces *which certificate is trusted*, not *whether the peer holds its private key*.

use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

/// SHA-256 of a certificate's DER bytes, lowercase hex -- what is shown and stored.
pub fn fingerprint(der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(der).iter().map(|b| format!("{b:02x}")).collect()
}

/// `aa:bb:cc:...` in groups, easier to compare by eye.
pub fn display(fingerprint: &str) -> String {
    fingerprint.as_bytes().chunks(2).map(|c| std::str::from_utf8(c).unwrap_or("")).collect::<Vec<_>>().join(":")
}

/// What a connection should trust.
#[derive(Debug, Clone)]
pub enum Pin {
    /// Exactly this certificate.
    Expect(String),
    /// The first connection to this server: accept whatever it presents, and record it here so
    /// the caller can show it and store it.
    Capture(Arc<Mutex<Option<String>>>),
}

#[derive(Debug)]
pub struct PinnedVerifier {
    pin: Pin,
    algorithms: WebPkiSupportedAlgorithms,
}

impl PinnedVerifier {
    pub fn new(pin: Pin) -> Self {
        Self { pin, algorithms: provider().signature_verification_algorithms }
    }
}

fn provider() -> CryptoProvider {
    rustls::crypto::ring::default_provider()
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _intermediates: &[CertificateDer<'_>], _name: &ServerName<'_>, _ocsp: &[u8], _now: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        let presented = fingerprint(end_entity.as_ref());
        match &self.pin {
            Pin::Expect(expected) if expected.eq_ignore_ascii_case(&presented) => Ok(ServerCertVerified::assertion()),
            Pin::Expect(_) => Err(rustls::Error::General(format!("the server's certificate changed (it now presents {}) -- if that was deliberate, trust the new one", display(&presented)))),
            Pin::Capture(slot) => {
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(presented);
                Ok(ServerCertVerified::assertion())
            }
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// A client configuration that trusts only `pin`.
pub fn client_config(pin: Pin) -> rustls::ClientConfig {
    rustls::ClientConfig::builder_with_provider(Arc::new(provider()))
        .with_safe_default_protocol_versions()
        .expect("the ring provider supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier::new(pin)))
        .with_no_client_auth()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_are_sha256_hex() {
        assert_eq!(fingerprint(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(display("ab12cd"), "ab:12:cd");
    }
}
