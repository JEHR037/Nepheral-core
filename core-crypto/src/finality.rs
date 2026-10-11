// SPDX-License-Identifier: BUSL-1.1
use std::collections::BTreeSet;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::block::BlockHash;
use crate::network::{domains, signing_digest};

pub const FINALITY_INTERVAL: u64 = 16;

pub const MAX_JUSTIFICATION_VOTES: usize = 64;

pub fn threshold(validators: usize) -> usize {
    validators * 2 / 3 + 1
}

pub fn target_for(head: u64) -> u64 {
    head - head % FINALITY_INTERVAL
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalityVote {
    pub height: u64,
    pub block_hash: BlockHash,
    pub voter: [u8; 32],
    #[serde(with = "crate::wire::bytes64")]
    pub signature: [u8; 64],
}

fn vote_digest(height: u64, block_hash: &BlockHash) -> [u8; 32] {
    signing_digest(domains::FINALITY_VOTE, &[&height.to_le_bytes(), block_hash])
}

impl FinalityVote {
    pub fn sign(key: &SigningKey, height: u64, block_hash: BlockHash) -> Self {
        let signature = key.sign(&vote_digest(height, &block_hash)).to_bytes();
        Self {
            height,
            block_hash,
            voter: key.verifying_key().to_bytes(),
            signature,
        }
    }

    pub fn verify(&self) -> bool {
        let Ok(vk) = VerifyingKey::from_bytes(&self.voter) else {
            return false;
        };
        vk.verify_strict(
            &vote_digest(self.height, &self.block_hash),
            &Signature::from_bytes(&self.signature),
        )
        .is_ok()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        crate::wire::encode(self)
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        crate::wire::decode(b)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Justification {
    pub height: u64,
    pub block_hash: BlockHash,
    pub votes: Vec<([u8; 32], Signed)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signed(#[serde(with = "crate::wire::bytes64")] pub [u8; 64]);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalityError {
    Malformed,
    UnknownVoter,
    BadSignature,
    BelowThreshold,
}

impl Justification {
    pub fn from_votes(height: u64, block_hash: BlockHash, votes: &[FinalityVote]) -> Self {
        let mut seen = BTreeSet::new();
        let votes = votes
            .iter()
            .filter(|v| v.height == height && v.block_hash == block_hash)
            .filter(|v| seen.insert(v.voter))
            .take(MAX_JUSTIFICATION_VOTES)
            .map(|v| (v.voter, Signed(v.signature)))
            .collect();
        Self {
            height,
            block_hash,
            votes,
        }
    }

    pub fn verify(&self, validators: &[[u8; 32]]) -> Result<(), FinalityError> {
        if self.votes.len() > MAX_JUSTIFICATION_VOTES {
            return Err(FinalityError::Malformed);
        }
        let mut voters = BTreeSet::new();
        for (voter, sig) in &self.votes {
            if !voters.insert(*voter) {
                return Err(FinalityError::Malformed);
            }
            if !validators.contains(voter) {
                return Err(FinalityError::UnknownVoter);
            }
            let vote = FinalityVote {
                height: self.height,
                block_hash: self.block_hash,
                voter: *voter,
                signature: sig.0,
            };
            if !vote.verify() {
                return Err(FinalityError::BadSignature);
            }
        }
        if voters.len() < threshold(validators.len()) {
            return Err(FinalityError::BelowThreshold);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EquivocationProof {
    pub first: FinalityVote,
    pub second: FinalityVote,
}

impl EquivocationProof {
    pub fn offender(&self) -> Option<[u8; 32]> {
        let same_slot = self.first.voter == self.second.voter
            && self.first.height == self.second.height
            && self.first.block_hash != self.second.block_hash;
        (same_slot && self.first.verify() && self.second.verify()).then_some(self.first.voter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinalityCall {
    Finalize { justification: Justification },
    ReportEquivocation { proof: Box<EquivocationProof> },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(n: u8) -> Vec<SigningKey> {
        (1..=n).map(|i| SigningKey::from_bytes(&[i; 32])).collect()
    }

    fn pubs(keys: &[SigningKey]) -> Vec<[u8; 32]> {
        keys.iter().map(|k| k.verifying_key().to_bytes()).collect()
    }

    #[test]
    fn thresholds_follow_two_thirds_plus_one() {
        assert_eq!(threshold(1), 1);
        assert_eq!(threshold(2), 2);
        assert_eq!(threshold(3), 3);
        assert_eq!(threshold(4), 3);
        assert_eq!(threshold(7), 5);
        assert_eq!(target_for(35), 32);
    }

    #[test]
    fn a_justification_needs_a_supermajority_of_the_set() {
        let ks = keys(4);
        let set = pubs(&ks);
        let votes: Vec<FinalityVote> = ks
            .iter()
            .map(|k| FinalityVote::sign(k, 16, [7u8; 32]))
            .collect();
        let two = Justification::from_votes(16, [7u8; 32], &votes[..2]);
        assert_eq!(two.verify(&set), Err(FinalityError::BelowThreshold));
        let three = Justification::from_votes(16, [7u8; 32], &votes[..3]);
        assert!(three.verify(&set).is_ok());
        let mut dup = three.clone();
        dup.votes.push(dup.votes[0]);
        assert_eq!(dup.verify(&set), Err(FinalityError::Malformed));
        assert_eq!(three.verify(&set[1..]), Err(FinalityError::UnknownVoter));
        let mut wrong = three;
        wrong.block_hash = [8u8; 32];
        assert_eq!(wrong.verify(&set), Err(FinalityError::BadSignature));
    }

    #[test]
    fn signing_two_blocks_at_one_height_is_provable() {
        let k = &keys(1)[0];
        let proof = EquivocationProof {
            first: FinalityVote::sign(k, 32, [1u8; 32]),
            second: FinalityVote::sign(k, 32, [2u8; 32]),
        };
        assert_eq!(proof.offender(), Some(k.verifying_key().to_bytes()));
        let honest = EquivocationProof {
            first: FinalityVote::sign(k, 32, [1u8; 32]),
            second: FinalityVote::sign(k, 48, [2u8; 32]),
        };
        assert_eq!(honest.offender(), None);
    }
}
