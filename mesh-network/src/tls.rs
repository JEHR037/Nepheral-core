// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use core_crypto::identity::PeerId;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls13_signature, CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, Error as TlsError, SignatureScheme};

const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

#[derive(Clone)]
pub struct TlsIdentity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

impl std::fmt::Debug for TlsIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsIdentity")
            .field(
                "peer_id",
                &peer_id_from_cert_der(&self.cert_der).map(hex::encode),
            )
            .finish_non_exhaustive()
    }
}

impl TlsIdentity {
    fn cert_chain(&self) -> Vec<CertificateDer<'static>> {
        vec![CertificateDer::from(self.cert_der.clone())]
    }

    fn private_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_der.clone()))
    }
}

pub fn peer_id_from_cert_der(cert_der: &[u8]) -> Option<PeerId> {
    let mut found: Option<usize> = None;
    let mut start = 0usize;
    while let Some(pos) = cert_der[start..]
        .windows(ED25519_SPKI_PREFIX.len())
        .position(|w| w == ED25519_SPKI_PREFIX)
    {
        if found.is_some() {
            return None;
        }
        found = Some(start + pos);
        start += pos + 1;
    }
    let at = found? + ED25519_SPKI_PREFIX.len();
    let key: [u8; 32] = cert_der.get(at..at + 32)?.try_into().ok()?;
    ed25519_dalek::VerifyingKey::from_bytes(&key).ok()?;
    Some(key)
}

pub fn peer_id_of(conn: &quinn::Connection) -> Option<PeerId> {
    let identity = conn.peer_identity()?;
    let certs = identity.downcast::<Vec<CertificateDer<'static>>>().ok()?;
    let first = certs.first()?;
    peer_id_from_cert_der(first.as_ref())
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn ed25519_only(provider: &CryptoProvider) -> WebPkiSupportedAlgorithms {
    provider.signature_verification_algorithms
}

#[derive(Debug)]
struct PeerCertVerifier {
    expected: Option<PeerId>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl PeerCertVerifier {
    fn check(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
    ) -> Result<(), TlsError> {
        if !intermediates.is_empty() {
            return Err(TlsError::General(
                "peer certificates are self-standing".into(),
            ));
        }
        let pid = peer_id_from_cert_der(end_entity.as_ref()).ok_or_else(|| {
            TlsError::General("certificate does not carry one Ed25519 key".into())
        })?;
        if let Some(expected) = self.expected {
            if pid != expected {
                return Err(TlsError::General(format!(
                    "peer presented {} but {} was expected",
                    hex::encode(&pid[..8]),
                    hex::encode(&expected[..8])
                )));
            }
        }
        Ok(())
    }

    fn check_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        if dss.scheme != SignatureScheme::ED25519 {
            return Err(TlsError::General(
                "only Ed25519 handshakes are accepted".into(),
            ));
        }
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }
}

impl ServerCertVerifier for PeerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        self.check(end_entity, intermediates)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Err(TlsError::General("TLS 1.2 is not accepted".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.check_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

impl ClientCertVerifier for PeerCertVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, TlsError> {
        self.check(end_entity, intermediates)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Err(TlsError::General("TLS 1.2 is not accepted".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.check_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

pub fn server_config(identity: &TlsIdentity) -> Result<rustls::ServerConfig, TlsError> {
    let provider = provider();
    let verifier = Arc::new(PeerCertVerifier {
        expected: None,
        algorithms: ed25519_only(&provider),
    });
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(identity.cert_chain(), identity.private_key())?;
    config.alpn_protocols = vec![core_crypto::network::active().alpn()];
    Ok(config)
}

pub fn client_config(
    identity: &TlsIdentity,
    expected: Option<PeerId>,
) -> Result<rustls::ClientConfig, TlsError> {
    let provider = provider();
    let verifier = Arc::new(PeerCertVerifier {
        expected,
        algorithms: ed25519_only(&provider),
    });
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(identity.cert_chain(), identity.private_key())?;
    config.alpn_protocols = vec![core_crypto::network::active().alpn()];
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> (TlsIdentity, PeerId) {
        let words = core_crypto::NodeMnemonic::generate();
        let id = core_crypto::NodeIdentity::from_mnemonic(&words).unwrap();
        (
            TlsIdentity {
                cert_der: id.cert_der.clone(),
                key_der: id.key_der().to_vec(),
            },
            id.peer_id,
        )
    }

    #[test]
    fn the_peer_id_is_the_certificate_key() {
        let (tls, pid) = identity();
        assert_eq!(peer_id_from_cert_der(&tls.cert_der), Some(pid));
    }

    #[test]
    fn a_certificate_with_two_ed25519_keys_is_ambiguous() {
        let (tls, _) = identity();
        let mut doubled = tls.cert_der.clone();
        doubled.extend_from_slice(&ED25519_SPKI_PREFIX);
        doubled.extend_from_slice(&[1u8; 32]);
        assert_eq!(peer_id_from_cert_der(&doubled), None);
    }

    #[test]
    fn configs_build_for_a_real_identity() {
        let (tls, pid) = identity();
        assert!(server_config(&tls).is_ok());
        assert!(client_config(&tls, Some(pid)).is_ok());
    }
}
