// SPDX-License-Identifier: BUSL-1.1
//! Light sync: starting from a verified snapshot of the
//! chain state instead of replaying every block since genesis. The phone app
//! syncs this way; any node could.
//!
//! The trust comes from the chain, not from keys built into the app. The
//! verifier starts from the genesis state it computes itself (the team key
//! as the only validator) and follows every change of the validator set:
//!
//! - A justification at height `h` is checked on chain against
//!   `validators_at(h)`, the set in force at `h`. A change made in block `c`
//!   therefore has the checkpoint after it finalized by the new set, which
//!   the verifier does not know yet.
//! - So each change travels as a [`Handoff`]: the last finalized state
//!   before it (finalized by the set the verifier trusts, its state checked
//!   against the header's state root), then the blocks from there to the
//!   first checkpoint after the change, applied with the same
//!   `ChainState::apply_block` every node runs (block signatures, state
//!   roots and all). The set they produce is the one that must have signed
//!   that checkpoint's justification.
//! - Last, the [`LightProof::tip`]: the newest finalized state, finalized by
//!   the set the handoffs ended with.
//!
//! A server that leaves a change out can only make verification fail or
//! hand over an older state, which the verifier then brings up to date with
//! ordinary blocks; it cannot get a state accepted that the validators did
//! not finalize.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::block::{Block, BlockHeader};
use crate::chain::{ChainError, ChainState};
use crate::extrinsic::DecodedCall;
use crate::finality::{FinalityCall, Justification};

/// A finalized state: the header, the justification that finalized it, and
/// the state that header's state root commits to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateBundle {
    pub header: BlockHeader,
    pub justification: Justification,
    pub state: ChainState,
}

/// One or more changes of the validator set, carried across with the
/// blocks that made them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handoff {
    /// The last finalized state before the change. `None`: from genesis.
    pub base: Option<StateBundle>,
    /// The blocks after `base` up to and including the first finalized
    /// checkpoint after the change.
    pub blocks: Vec<Block>,
    /// That checkpoint's justification, signed by the new set.
    pub justification: Justification,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LightProof {
    pub handoffs: Vec<Handoff>,
    pub tip: StateBundle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LightError {
    /// The justification does not verify under the set in force.
    BadJustification { height: u64 },
    /// A justification or bundle names a block other than the one at its
    /// height.
    HeaderMismatch { height: u64 },
    /// The state is not the one the header commits to.
    StateRootMismatch { height: u64 },
    /// A bundle older than the state already verified.
    Stale { height: u64 },
    /// A handoff block did not apply.
    Block { height: u64, error: ChainError },
}

impl std::fmt::Display for LightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl std::error::Error for LightError {}

/// Verifies `proof` from `genesis` and returns the tip's state, imported so
/// that the blocks after it apply as on any node.
pub fn verify(proof: LightProof, genesis: ChainState) -> Result<ChainState, LightError> {
    let mut trusted = genesis;
    for handoff in proof.handoffs {
        if let Some(base) = handoff.base {
            trusted = import(&trusted, base)?;
        }
        for block in handoff.blocks {
            let height = block.header.height;
            trusted
                .apply_block(block)
                .map_err(|error| LightError::Block { height, error })?;
        }
        check_final(&trusted, &handoff.justification)?;
    }
    import(&trusted, proof.tip)
}

/// `j` finalizes a block of `chain`, signed by the set in force there.
fn check_final(chain: &ChainState, j: &Justification) -> Result<(), LightError> {
    if chain.hash_at(j.height) != Some(j.block_hash) {
        return Err(LightError::HeaderMismatch { height: j.height });
    }
    j.verify(&chain.authority.validators_at(j.height))
        .map_err(|_| LightError::BadJustification { height: j.height })
}

/// Adopts `bundle` if the set `trusted` holds finalized it and its state is
/// the one its header commits to.
fn import(trusted: &ChainState, bundle: StateBundle) -> Result<ChainState, LightError> {
    let height = bundle.header.height;
    if height < trusted.head_height() {
        return Err(LightError::Stale { height });
    }
    let j = &bundle.justification;
    if j.height != height || j.block_hash != bundle.header.block_hash() {
        return Err(LightError::HeaderMismatch { height });
    }
    j.verify(&trusted.authority.validators_at(height))
        .map_err(|_| LightError::BadJustification { height })?;
    if bundle.state.state_root() != bundle.header.state_root {
        return Err(LightError::StateRootMismatch { height });
    }
    Ok(bundle.state.imported_at(bundle.header))
}

/// The state alone, as a bundle carries it.
fn bundle_state(chain: &ChainState) -> ChainState {
    let mut state = chain.state_without_history();
    state.base = None;
    state
}

/// Every justification that landed in `blocks`, by height.
pub fn landed_justifications(blocks: &[Block]) -> BTreeMap<u64, Justification> {
    let mut out = BTreeMap::new();
    for block in blocks {
        for ext in &block.body.extrinsics {
            if let Ok(DecodedCall::Finality(FinalityCall::Finalize { justification })) =
                DecodedCall::decode(ext.pallet_idx, &ext.args)
            {
                out.insert(justification.height, justification);
            }
        }
    }
    out
}

/// Builds the proof for the chain made of `blocks` on top of `genesis`, by
/// replaying it. `None` if a block does not apply or nothing is finalized.
pub fn build_proof(genesis: ChainState, blocks: &[Block]) -> Option<LightProof> {
    let justified = landed_justifications(blocks);
    let mut chain = genesis;
    let mut set = chain.authority.authorities.clone();
    let mut handoffs = Vec::new();
    let mut segment_base: Option<StateBundle> = None;
    let mut segment_blocks: Vec<Block> = Vec::new();
    let mut changed = false;
    let mut tip: Option<StateBundle> = None;
    for block in blocks {
        chain.apply_block(block.clone()).ok()?;
        segment_blocks.push(block.clone());
        if chain.authority.authorities != set {
            set = chain.authority.authorities.clone();
            changed = true;
        }
        let Some(j) = justified.get(&block.header.height) else {
            continue;
        };
        if j.block_hash != block.header.block_hash() {
            continue;
        }
        let bundle = StateBundle {
            header: block.header.clone(),
            justification: j.clone(),
            state: bundle_state(&chain),
        };
        if changed {
            handoffs.push(Handoff {
                base: segment_base.take(),
                blocks: std::mem::take(&mut segment_blocks),
                justification: j.clone(),
            });
            changed = false;
        }
        segment_blocks.clear();
        segment_base = Some(bundle.clone());
        tip = Some(bundle);
    }
    Some(LightProof {
        handoffs,
        tip: tip?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BlockBody, SLOT_DURATION_MS};
    use crate::extrinsic::{Extrinsic, PALLET_AUTHORITY, PALLET_FINALITY};
    use crate::finality::FinalityVote;
    use crate::pallets::authority::AuthorityCall;
    use ed25519_dalek::SigningKey;

    struct Net {
        keys: Vec<SigningKey>,
        chain: ChainState,
        blocks: Vec<Block>,
        slot: u64,
        nonce: u64,
    }

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    impl Net {
        fn new() -> Self {
            let founder = key(1);
            Net {
                chain: ChainState::with_genesis_authority(founder.verifying_key().to_bytes()),
                keys: vec![founder],
                blocks: Vec::new(),
                slot: 900_000_000,
                nonce: 0,
            }
        }

        fn founder_ext(&mut self, pallet: u8, call: &impl Serialize) -> Extrinsic {
            self.nonce += 1;
            let f = &self.keys[0];
            Extrinsic::sign(f, pallet, call, f.verifying_key().to_bytes(), self.nonce)
        }

        fn add_validator(&mut self, n: u8) -> Extrinsic {
            let k = key(n);
            let ext = self.founder_ext(
                PALLET_AUTHORITY,
                &AuthorityCall::Add {
                    pubkey: k.verifying_key().to_bytes(),
                    channels: vec!["chat".into()],
                    attestation_hash: [0u8; 32],
                },
            );
            self.keys.push(k);
            ext
        }

        /// Seals the next block, in the next slot whose primary is one of
        /// the keys this network holds.
        fn seal(&mut self, extrinsics: Vec<Extrinsic>) {
            let set = self.chain.validator_set();
            let k = loop {
                self.slot += 1;
                let primary = set.primary_for_slot(self.slot).unwrap();
                if let Some(k) = self.keys.iter().find(|k| k.verifying_key() == *primary) {
                    break k.clone();
                }
            };
            let mut b = Block {
                header: BlockHeader {
                    version: 1,
                    height: self.chain.head_height() + 1,
                    slot: self.slot,
                    parent_hash: self.chain.head_hash(),
                    state_root: [0u8; 32],
                    extrinsics_root: [0u8; 32],
                    lottery_root: [0u8; 32],
                    author: crate::validators::authority_peer_id(&k.verifying_key()),
                    signature: [0u8; 64],
                    timestamp_ms: self.slot * SLOT_DURATION_MS,
                },
                body: BlockBody {
                    extrinsics,
                    lottery_results: Vec::new(),
                },
            };
            b.header.extrinsics_root = b.extrinsics_root();
            b.header.lottery_root = b.lottery_root();
            let mut probe = self.chain.clone();
            probe.apply_block_unverified(b.clone()).expect("dry run");
            b.header.state_root = probe.state_root();
            b.header.sign(&k);
            self.chain.apply_block(b.clone()).expect("applies");
            self.blocks.push(b);
        }

        /// Seals empty blocks up to `height`.
        fn seal_to(&mut self, height: u64) {
            while self.chain.head_height() < height {
                self.seal(Vec::new());
            }
        }

        /// The justification of `height`, signed by the set in force there.
        fn justify(&self, height: u64) -> Justification {
            let hash = self.chain.hash_at(height).unwrap();
            let set = self.chain.authority.validators_at(height);
            let votes: Vec<FinalityVote> = self
                .keys
                .iter()
                .filter(|k| set.contains(&k.verifying_key().to_bytes()))
                .map(|k| FinalityVote::sign(k, height, hash))
                .collect();
            Justification::from_votes(height, hash, &votes)
        }

        /// Lands the justification of `height` in the next block.
        fn finalize(&mut self, height: u64) {
            let j = self.justify(height);
            let ext = self.founder_ext(
                PALLET_FINALITY,
                &FinalityCall::Finalize { justification: j },
            );
            self.seal(vec![ext]);
        }
    }

    /// Two changes of the validator set: two members at block 1, a third at
    /// block 34. Finalized at 16, 32 and 48; head at 52.
    fn network() -> Net {
        let mut n = Net::new();
        let a = n.add_validator(2);
        let b = n.add_validator(3);
        n.seal(vec![a, b]);
        n.seal_to(16);
        n.finalize(16);
        n.seal_to(32);
        n.finalize(32);
        let c = n.add_validator(4);
        n.seal(vec![c]);
        n.seal_to(48);
        n.finalize(48);
        n.seal_to(52);
        n
    }

    fn genesis() -> ChainState {
        ChainState::with_genesis_authority(key(1).verifying_key().to_bytes())
    }

    fn proof_of(n: &Net) -> LightProof {
        build_proof(genesis(), &n.blocks).expect("a proof")
    }

    #[test]
    fn the_proof_carries_each_change_of_the_set() {
        let n = network();
        let proof = proof_of(&n);
        assert_eq!(proof.handoffs.len(), 2);
        assert!(
            proof.handoffs[0].base.is_none(),
            "the first starts at genesis"
        );
        assert_eq!(proof.handoffs[0].justification.height, 16);
        assert_eq!(proof.handoffs[1].base.as_ref().unwrap().header.height, 32);
        assert_eq!(proof.handoffs[1].justification.height, 48);
        assert_eq!(proof.tip.header.height, 48);
    }

    #[test]
    fn a_verified_proof_continues_with_ordinary_blocks_to_the_same_state() {
        let n = network();
        let mut light = verify(proof_of(&n), genesis()).expect("verifies");
        assert_eq!(light.head_height(), 48);
        assert_eq!(light.authority.authorities.len(), 4);
        for b in n.blocks.iter().filter(|b| b.header.height > 48) {
            light
                .apply_block(b.clone())
                .expect("block after the tip applies");
        }
        assert_eq!(light.head_hash(), n.chain.head_hash());
        assert_eq!(light.state_root(), n.chain.state_root());
    }

    #[test]
    fn the_proof_survives_the_wire() {
        let n = network();
        let bytes = crate::wire::encode(&proof_of(&n));
        let back: LightProof = crate::wire::decode(&bytes).expect("decodes");
        let light = verify(back, genesis()).expect("verifies");
        assert_eq!(light.head_height(), 48);
    }

    #[test]
    fn a_tampered_state_is_refused() {
        let n = network();
        let mut proof = proof_of(&n);
        proof.tip.state.balances.mint([9u8; 32], 1_000_000);
        assert_eq!(
            verify(proof, genesis()).unwrap_err(),
            LightError::StateRootMismatch { height: 48 }
        );
    }

    #[test]
    fn a_justification_from_outside_the_set_is_refused() {
        let n = network();
        let mut proof = proof_of(&n);
        let forger = key(9);
        let hash = proof.tip.header.block_hash();
        proof.tip.justification =
            Justification::from_votes(48, hash, &[FinalityVote::sign(&forger, 48, hash)]);
        assert_eq!(
            verify(proof, genesis()).unwrap_err(),
            LightError::BadJustification { height: 48 }
        );
    }

    #[test]
    fn a_handoff_that_stops_short_of_its_justified_block_is_refused() {
        let n = network();
        let mut proof = proof_of(&n);
        proof.handoffs[0].blocks.pop();
        assert_eq!(
            verify(proof, genesis()).unwrap_err(),
            LightError::HeaderMismatch { height: 16 }
        );
    }

    #[test]
    fn a_proof_for_another_genesis_is_refused() {
        let n = network();
        let other = ChainState::with_genesis_authority(key(7).verifying_key().to_bytes());
        assert!(matches!(
            verify(proof_of(&n), other).unwrap_err(),
            LightError::Block { height: 1, .. }
        ));
    }
}
