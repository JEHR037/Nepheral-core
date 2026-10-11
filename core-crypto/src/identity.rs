// SPDX-License-Identifier: BUSL-1.1
use ed25519_dalek::pkcs8::EncodePrivateKey;
use ed25519_dalek::SigningKey;
use rcgen::{Certificate, CertificateParams, DnType, KeyPair, PKCS_ED25519};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::mnemonic::NodeMnemonic;

pub type PeerId = [u8; 32];

pub struct NodeIdentity {
    pub cert_der: Vec<u8>,
    key_der: SensitiveBytes,
    pub peer_id: PeerId,
}

#[derive(Zeroize, ZeroizeOnDrop)]
struct SensitiveBytes(Vec<u8>);

impl NodeIdentity {
    pub fn ephemeral() -> Result<Self, String> {
        let key_pair = KeyPair::generate(&PKCS_ED25519)
            .map_err(|e| format!("Failed to generate Ed25519 keypair: {}", e))?;

        let mut params = CertificateParams::default();
        params.alg = &PKCS_ED25519;
        params
            .distinguished_name
            .push(DnType::CommonName, "nepheral-ephemeral");
        params.key_pair = Some(key_pair);

        let cert = Certificate::from_params(params)
            .map_err(|e| format!("Failed to build certificate: {}", e))?;

        let cert_der = cert
            .serialize_der()
            .map_err(|e| format!("Failed to serialize cert DER: {}", e))?;
        let key_der = cert.serialize_private_key_der();

        let peer_id: PeerId = {
            let mut hasher = Sha256::new();
            hasher.update(&cert_der);
            hasher.finalize().into()
        };

        Ok(Self {
            cert_der,
            key_der: SensitiveBytes(key_der),
            peer_id,
        })
    }

    pub fn from_mnemonic(mnemonic: &NodeMnemonic) -> Result<Self, String> {
        Self::from_signing_key(&SigningKey::from_bytes(&mnemonic.to_seed()))
    }

    /// The identity of a node whose key is not derived from a phrase: the
    /// desktop's node key, kept apart from the wallet.
    pub fn from_signing_key(signing_key: &SigningKey) -> Result<Self, String> {
        let public_key_bytes = signing_key.verifying_key().to_bytes();

        let pkcs8_der = signing_key
            .to_pkcs8_der()
            .map_err(|e| format!("Failed to encode PKCS8: {}", e))?;

        let key_pair = KeyPair::from_der(pkcs8_der.as_bytes())
            .map_err(|e| format!("Failed to load keypair from PKCS8: {}", e))?;

        let mut params = CertificateParams::default();
        params.alg = &PKCS_ED25519;
        params
            .distinguished_name
            .push(DnType::CommonName, "nepheral-node");
        params.key_pair = Some(key_pair);

        let cert = Certificate::from_params(params)
            .map_err(|e| format!("Failed to build certificate: {}", e))?;

        let cert_der = cert
            .serialize_der()
            .map_err(|e| format!("Failed to serialize cert DER: {}", e))?;
        let key_der = cert.serialize_private_key_der();

        let peer_id: PeerId = public_key_bytes;

        Ok(Self {
            cert_der,
            key_der: SensitiveBytes(key_der),
            peer_id,
        })
    }

    pub fn key_der(&self) -> &[u8] {
        &self.key_der.0
    }

    pub fn fingerprint_short(&self) -> String {
        hex::encode(&self.peer_id[..8])
    }

    pub fn fingerprint_hex(&self) -> String {
        hex::encode(self.peer_id)
    }
}

pub fn peer_id_from_cert(cert_der: &[u8]) -> PeerId {
    let mut hasher = Sha256::new();
    hasher.update(cert_der);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_ephemeral_is_unique() {
        let a = NodeIdentity::ephemeral().unwrap();
        let b = NodeIdentity::ephemeral().unwrap();
        assert_ne!(a.peer_id, b.peer_id);
        assert_ne!(a.cert_der, b.cert_der);
    }

    #[test]
    fn peer_id_stable_for_same_cert() {
        let id = NodeIdentity::ephemeral().unwrap();
        assert_eq!(id.peer_id, peer_id_from_cert(&id.cert_der));
    }

    #[test]
    fn fingerprint_short_is_16_hex_chars() {
        let id = NodeIdentity::ephemeral().unwrap();
        assert_eq!(id.fingerprint_short().len(), 16);
    }
}
