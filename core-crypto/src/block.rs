// SPDX-License-Identifier: BUSL-1.1
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::extrinsic::Extrinsic;
use crate::identity::PeerId;

pub type BlockHash = [u8; 32];

pub const SLOT_DURATION_MS: u64 = 2_000;

pub const EARLY_BLOCK_EXT_THRESHOLD: usize = 8;

pub const EARLY_BLOCK_LOTTERY_THRESHOLD: usize = 4;

pub const SLOT_DRIFT_TOLERANCE: u64 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LotteryResult {
    pub hour_epoch: u64,
    pub msg_len: u64,
    pub n_engines: u64,
    pub room_salt: [u8; 32],
    pub winner: PeerId,
    #[serde(default)]
    pub dimension: u64,
    #[serde(default)]
    pub responder_model_b: u16,
    #[serde(default)]
    pub responder_tokens: u32,
    #[serde(default)]
    pub responder_gen_ms: u32,
}

impl LotteryResult {
    pub fn hash(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(self.hour_epoch.to_le_bytes());
        h.update(self.msg_len.to_le_bytes());
        h.update(self.n_engines.to_le_bytes());
        h.update(self.room_salt);
        h.update(self.winner);
        h.update(self.dimension.to_le_bytes());
        h.update(self.responder_model_b.to_le_bytes());
        h.update(self.responder_tokens.to_le_bytes());
        h.update(self.responder_gen_ms.to_le_bytes());
        h.finalize().into()
    }

    pub fn dimension_matches_inputs(&self) -> bool {
        let expected = crate::lottery::compute_dimension(
            self.responder_model_b as u64,
            self.responder_tokens as u64,
            self.responder_gen_ms as u64,
        );
        expected == self.dimension
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockHeader {
    pub version: u8,
    pub height: u64,
    pub slot: u64,
    pub parent_hash: BlockHash,
    pub state_root: [u8; 32],
    pub extrinsics_root: [u8; 32],
    pub lottery_root: [u8; 32],
    pub author: PeerId,
    #[serde(with = "crate::wire::bytes64")]
    pub signature: [u8; 64],
    pub timestamp_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockBody {
    pub extrinsics: Vec<Extrinsic>,
    pub lottery_results: Vec<LotteryResult>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub body: BlockBody,
}

impl BlockHeader {
    pub fn hash_for_signing(&self) -> [u8; 32] {
        crate::network::signing_digest(
            crate::network::domains::BLOCK_HEADER,
            &[
                &[self.version],
                &self.height.to_le_bytes(),
                &self.slot.to_le_bytes(),
                &self.parent_hash,
                &self.state_root,
                &self.extrinsics_root,
                &self.lottery_root,
                &self.author,
                &self.timestamp_ms.to_le_bytes(),
            ],
        )
    }

    pub fn block_hash(&self) -> BlockHash {
        let mut h = Sha256::new();
        h.update(self.hash_for_signing());
        h.update(self.signature);
        h.finalize().into()
    }

    pub fn sign(&mut self, key: &SigningKey) {
        let sig = key.sign(&self.hash_for_signing());
        self.signature = sig.to_bytes();
    }

    pub fn verify_signature(
        &self,
        pubkey: &VerifyingKey,
    ) -> Result<(), ed25519_dalek::SignatureError> {
        let sig = Signature::from_bytes(&self.signature);
        pubkey.verify_strict(&self.hash_for_signing(), &sig)
    }

    pub fn slot_matches_timestamp(&self) -> bool {
        let expected = self.timestamp_ms / SLOT_DURATION_MS;
        let diff = expected.abs_diff(self.slot);
        diff <= SLOT_DRIFT_TOLERANCE
    }
}

impl Block {
    pub fn extrinsics_root(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"nepheral/extrinsics-root/v2");
        h.update((self.body.extrinsics.len() as u32).to_be_bytes());
        for ext in &self.body.extrinsics {
            h.update(ext.hash_for_signing());
            h.update(ext.signature);
        }
        h.finalize().into()
    }

    pub fn lottery_root(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"nepheral/lottery-root/v2");
        h.update((self.body.lottery_results.len() as u32).to_be_bytes());
        for r in &self.body.lottery_results {
            h.update(r.hash());
        }
        h.finalize().into()
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

    fn sample_header(author: PeerId) -> BlockHeader {
        BlockHeader {
            version: 1,
            height: 7,
            slot: 1234,
            parent_hash: [9u8; 32],
            state_root: [1u8; 32],
            extrinsics_root: [2u8; 32],
            lottery_root: [3u8; 32],
            author,
            signature: [0u8; 64],
            timestamp_ms: 1234 * SLOT_DURATION_MS,
        }
    }

    #[test]
    fn sign_verify_roundtrip() {
        let key = rand_key();
        let mut author = [0u8; 32];
        author.copy_from_slice(key.verifying_key().as_bytes());
        let mut header = sample_header(author);

        header.sign(&key);
        assert!(header.verify_signature(&key.verifying_key()).is_ok());
    }

    #[test]
    fn signature_changes_block_hash() {
        let key = rand_key();
        let mut author = [0u8; 32];
        author.copy_from_slice(key.verifying_key().as_bytes());
        let mut header = sample_header(author);
        let before = header.block_hash();
        header.sign(&key);
        let after = header.block_hash();
        assert_ne!(before, after, "block_hash must mix in signature");
    }

    #[test]
    fn slot_drift_tolerance() {
        let header = sample_header([0u8; 32]);
        assert!(header.slot_matches_timestamp());

        let mut drifted = header.clone();
        drifted.slot += 1; // within tolerance
        assert!(drifted.slot_matches_timestamp());

        drifted.slot += 5; // way out
        assert!(!drifted.slot_matches_timestamp());
    }

    #[test]
    fn lottery_result_hash_stable() {
        let r = LotteryResult {
            hour_epoch: 42,
            msg_len: 100,
            n_engines: 3,
            room_salt: [7u8; 32],
            winner: [11u8; 32],
            dimension: 200,
            responder_model_b: 4,
            responder_tokens: 25,
            responder_gen_ms: 5_000,
        };
        assert_eq!(r.hash(), r.hash(), "same input → same hash");
    }

    #[test]
    fn lottery_result_hash_depends_on_dimension() {
        let base = LotteryResult {
            hour_epoch: 42,
            msg_len: 100,
            n_engines: 3,
            room_salt: [7u8; 32],
            winner: [11u8; 32],
            dimension: 200,
            responder_model_b: 4,
            responder_tokens: 25,
            responder_gen_ms: 5_000,
        };
        let mut alt = base.clone();
        alt.dimension = 2000;
        assert_ne!(base.hash(), alt.hash());
    }

    #[test]
    fn lottery_result_hash_depends_on_inputs() {
        let base = LotteryResult {
            hour_epoch: 42,
            msg_len: 100,
            n_engines: 3,
            room_salt: [7u8; 32],
            winner: [11u8; 32],
            dimension: 200,
            responder_model_b: 4,
            responder_tokens: 25,
            responder_gen_ms: 5_000,
        };
        let mut alt = base.clone();
        alt.responder_model_b = 72;
        assert_ne!(base.hash(), alt.hash());

        let mut alt2 = base.clone();
        alt2.responder_tokens = 5000;
        assert_ne!(base.hash(), alt2.hash());
    }

    #[test]
    fn dimension_matches_inputs_validates_compute_dimension() {
        let model_b = 4u64;
        let tokens = 25u64;
        let gen_ms = 5_000u64;
        let expected = crate::lottery::compute_dimension(model_b, tokens, gen_ms);

        let honest = LotteryResult {
            hour_epoch: 0,
            msg_len: 0,
            n_engines: 1,
            room_salt: [0u8; 32],
            winner: [0u8; 32],
            dimension: expected,
            responder_model_b: model_b as u16,
            responder_tokens: tokens as u32,
            responder_gen_ms: gen_ms as u32,
        };
        assert!(honest.dimension_matches_inputs());

        let mut cheat = honest.clone();
        cheat.dimension = expected * 100;
        assert!(!cheat.dimension_matches_inputs());
    }
}
