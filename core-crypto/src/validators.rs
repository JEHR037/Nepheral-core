// SPDX-License-Identifier: BUSL-1.1
use ed25519_dalek::VerifyingKey;
use sha2::{Digest, Sha256};

use crate::block::BlockHeader;
use crate::identity::PeerId;

#[derive(Clone, Debug, Default)]
pub struct ValidatorSet {
    pub authorities: Vec<VerifyingKey>,
    pub peer_ids: Vec<PeerId>,
}

#[derive(Debug)]
pub enum ValidatorError {
    InvalidKey,
    Empty,
    WrongSlotAuthor([u8; 32]),
    BadSignature,
    UnknownAuthor([u8; 32]),
}

impl std::fmt::Display for ValidatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidKey => write!(f, "invalid pubkey bytes"),
            Self::Empty => write!(f, "empty validator set"),
            Self::WrongSlotAuthor(p) => {
                write!(
                    f,
                    "author {} does not match the slot's primary",
                    hex::encode(&p[..8])
                )
            }
            Self::BadSignature => write!(f, "signature verification failed"),
            Self::UnknownAuthor(p) => {
                write!(
                    f,
                    "validator set has no authority key {}",
                    hex::encode(&p[..8])
                )
            }
        }
    }
}

impl std::error::Error for ValidatorError {}

pub fn authority_peer_id(key: &VerifyingKey) -> PeerId {
    let mut h = Sha256::new();
    h.update(b"nepheral-v1-authority-id");
    h.update(key.as_bytes());
    h.finalize().into()
}

impl ValidatorSet {
    pub fn new(authorities: Vec<VerifyingKey>) -> Result<Self, ValidatorError> {
        if authorities.is_empty() {
            return Err(ValidatorError::Empty);
        }
        let peer_ids: Vec<PeerId> = authorities.iter().map(authority_peer_id).collect();
        Ok(Self {
            authorities,
            peer_ids,
        })
    }

    pub fn len(&self) -> usize {
        self.authorities.len()
    }
    pub fn is_empty(&self) -> bool {
        self.authorities.is_empty()
    }

    pub fn is_authority(&self, peer_id: &PeerId) -> bool {
        self.peer_ids.iter().any(|p| p == peer_id)
    }

    pub fn primary_for_slot(&self, slot: u64) -> Option<&VerifyingKey> {
        if self.authorities.is_empty() {
            return None;
        }
        let idx = (slot as usize) % self.authorities.len();
        Some(&self.authorities[idx])
    }

    pub fn finality_threshold(&self) -> usize {
        (self.authorities.len() * 2 / 3) + 1
    }

    pub fn verify_block_signature(&self, header: &BlockHeader) -> Result<(), ValidatorError> {
        let primary = self
            .primary_for_slot(header.slot)
            .ok_or(ValidatorError::Empty)?;
        let primary_peer = authority_peer_id(primary);
        if primary_peer != header.author {
            return Err(ValidatorError::WrongSlotAuthor(header.author));
        }
        header
            .verify_signature(primary)
            .map_err(|_| ValidatorError::BadSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::RngCore;

    fn rand_key() -> SigningKey {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        SigningKey::from_bytes(&bytes)
    }

    #[test]
    fn empty_set_errors() {
        assert!(matches!(
            ValidatorSet::new(vec![]),
            Err(ValidatorError::Empty)
        ));
    }

    #[test]
    fn round_robin_assignment() {
        let keys: Vec<_> = (0..3).map(|_| rand_key()).collect();
        let set = ValidatorSet::new(keys.iter().map(|k| k.verifying_key()).collect()).unwrap();
        for slot in 0..9 {
            let expected_idx = (slot as usize) % 3;
            let got = set.primary_for_slot(slot).unwrap();
            assert_eq!(
                got.as_bytes(),
                keys[expected_idx].verifying_key().as_bytes()
            );
        }
    }

    #[test]
    fn is_authority_membership() {
        let key = rand_key();
        let stranger = rand_key();
        let set = ValidatorSet::new(vec![key.verifying_key()]).unwrap();
        assert!(set.is_authority(&authority_peer_id(&key.verifying_key())));
        assert!(!set.is_authority(&authority_peer_id(&stranger.verifying_key())));
    }

    #[test]
    fn finality_threshold_2of3_plus_1() {
        let single = ValidatorSet::new(vec![rand_key().verifying_key()]).unwrap();
        assert_eq!(single.finality_threshold(), 1); // (1*2/3)+1 = 1

        let three =
            ValidatorSet::new((0..3).map(|_| rand_key().verifying_key()).collect()).unwrap();
        assert_eq!(three.finality_threshold(), 3); // (3*2/3)+1 = 3

        let four = ValidatorSet::new((0..4).map(|_| rand_key().verifying_key()).collect()).unwrap();
        assert_eq!(four.finality_threshold(), 3); // (4*2/3)+1 = 3

        let seven =
            ValidatorSet::new((0..7).map(|_| rand_key().verifying_key()).collect()).unwrap();
        assert_eq!(seven.finality_threshold(), 5); // (7*2/3)+1 = 5
    }
}
