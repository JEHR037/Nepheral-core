// SPDX-License-Identifier: BUSL-1.1
use bip39::Mnemonic;
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::identity::PeerId;

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct NodeMnemonic {
    phrase: String,
    entropy: Vec<u8>,
}

impl NodeMnemonic {
    pub fn generate() -> Self {
        let mut entropy = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut entropy);
        let m = Mnemonic::from_entropy(&entropy).expect("32-byte entropy always valid for BIP-39");
        Self {
            phrase: m.to_string(),
            entropy: entropy.to_vec(),
        }
    }

    pub fn from_phrase(phrase: &str) -> Result<Self, String> {
        let m = Mnemonic::parse(phrase.trim()).map_err(|e| e.to_string())?;
        Ok(Self {
            entropy: m.to_entropy(),
            phrase: m.to_string(),
        })
    }

    pub fn phrase(&self) -> &str {
        &self.phrase
    }

    pub fn to_peer_id(&self) -> PeerId {
        let seed = self.to_seed();
        let public_key = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
        let mut h = Sha256::new();
        h.update(b"nepheral-v1-peer-id");
        h.update(public_key);
        h.finalize().into()
    }

    pub fn to_seed(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"nepheral-v1-seed");
        h.update(&self.entropy);
        h.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_24_words() {
        let m = NodeMnemonic::generate();
        assert_eq!(m.phrase().split_whitespace().count(), 24);
    }

    #[test]
    fn from_phrase_roundtrip() {
        let m = NodeMnemonic::generate();
        let phrase = m.phrase().to_string();
        let m2 = NodeMnemonic::from_phrase(&phrase).unwrap();
        assert_eq!(m.to_peer_id(), m2.to_peer_id());
    }

    #[test]
    fn same_phrase_same_peer_id() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";
        let m1 = NodeMnemonic::from_phrase(phrase).unwrap();
        let m2 = NodeMnemonic::from_phrase(phrase).unwrap();
        assert_eq!(m1.to_peer_id(), m2.to_peer_id());
    }

    #[test]
    fn different_phrase_different_peer_id() {
        let m1 = NodeMnemonic::generate();
        let m2 = NodeMnemonic::generate();
        assert_ne!(m1.to_peer_id(), m2.to_peer_id());
    }

    #[test]
    fn rejects_invalid_phrase() {
        assert!(NodeMnemonic::from_phrase("not valid bip39 words at all here yes").is_err());
    }
}
