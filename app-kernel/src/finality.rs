// SPDX-License-Identifier: BUSL-1.1

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use core_crypto::block::BlockHash;
use core_crypto::extrinsic::PALLET_FINALITY;
use core_crypto::finality::{
    target_for, threshold, EquivocationProof, FinalityCall, FinalityVote, Justification,
};
use ed25519_dalek::SigningKey;
use mesh_network::protocol::message_types;

use crate::state::AppState;

pub const MAX_TRACKED_TARGETS: usize = 64;
const REBROADCAST_EVERY: u32 = 10;
/// Where a validator remembers the last height it voted for, per key, so a
/// restart cannot make it sign a second block at a height it already signed.
///
/// The file is named after the chain it belongs to. It used to be one file
/// for every chain, so after a chain reset a validator remembered voting at,
/// say, height 2,500 of the old chain and refused to vote at 16, 32, 48... of
/// the new one - finality stalled until the new chain overtook the old. Votes
/// on another chain cannot equivocate with this one anyway: every signature
/// is bound to the chain id.
fn vote_file(dir: &Path) -> std::path::PathBuf {
    let chain = core_crypto::network::active().chain_id();
    dir.join(format!("finality_votes-{}.json", hex::encode(&chain[..8])))
}

#[derive(Debug, Default)]
pub struct FinalityTracker {
    pub votes: BTreeMap<(u64, BlockHash), BTreeMap<[u8; 32], FinalityVote>>,
    pub first_vote: HashMap<([u8; 32], u64), FinalityVote>,
    pub observed: (u64, BlockHash),
    pub justification: Option<Justification>,
    pub reported: BTreeMap<[u8; 32], Vec<u64>>,
}

impl FinalityTracker {
    fn prune(&mut self) {
        let floor = self.observed.0;
        self.votes.retain(|(h, _), _| *h > floor);
        self.first_vote.retain(|(_, h), _| *h > floor);
        while self.votes.len() > MAX_TRACKED_TARGETS {
            let Some(first) = self.votes.keys().next().copied() else {
                break;
            };
            self.votes.remove(&first);
        }
    }
}

fn load_voted(dir: &Path) -> HashMap<String, u64> {
    std::fs::read(vote_file(dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn store_voted(dir: &Path, voted: &HashMap<String, u64>) {
    if let Ok(bytes) = serde_json::to_vec(voted) {
        let path = vote_file(dir);
        let tmp = path.with_extension("json.tmp");
        if std::fs::create_dir_all(dir).is_ok() && std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// Whether a justification still has to go on chain: it is newer than
/// what the chain has finalized, it is for the block the chain holds at that
/// height, and this node has not already submitted it.
pub fn finalize_due(
    finalized: u64,
    hash_at: Option<BlockHash>,
    j: &Justification,
    in_flight: bool,
) -> bool {
    !in_flight && finalized < j.height && hash_at == Some(j.block_hash)
}

pub enum VoteOutcome {
    Ignored,
    Stored,
    Finalized(Justification),
    Equivocation(Box<EquivocationProof>),
}

impl AppState {
    pub async fn observed_finality(&self) -> (u64, BlockHash) {
        let chain = self.chain.read().await.finalized();
        let observed = self.finality.lock().await.observed;
        if observed.0 > chain.0 {
            observed
        } else {
            chain
        }
    }

    async fn my_validator_keys(&self, height: u64) -> Vec<Arc<SigningKey>> {
        let chain = self.chain.read().await;
        let set = &chain.authority.validators_at(height);
        let mut keys: Vec<Arc<SigningKey>> = Vec::new();
        if let Some(k) = &self.authority_key {
            if set.contains(&k.verifying_key().to_bytes()) {
                keys.push(k.clone());
            }
        }
        if set.contains(&self.signing_key.verifying_key().to_bytes())
            && !keys
                .iter()
                .any(|k| k.verifying_key() == self.signing_key.verifying_key())
        {
            keys.push(self.signing_key.clone());
        }
        keys
    }

    pub async fn record_finality_vote(&self, vote: FinalityVote) -> VoteOutcome {
        if !vote.verify() {
            return VoteOutcome::Ignored;
        }
        let set = self.chain.read().await.authority.validators_at(vote.height);
        if !set.contains(&vote.voter) {
            return VoteOutcome::Ignored;
        }
        let mut tracker = self.finality.lock().await;
        if vote.height <= tracker.observed.0 {
            return VoteOutcome::Ignored;
        }
        let slot = (vote.voter, vote.height);
        if let Some(prior) = tracker.first_vote.get(&slot).cloned() {
            if prior.block_hash == vote.block_hash {
                return VoteOutcome::Ignored;
            }
            let already = tracker
                .reported
                .get(&vote.voter)
                .is_some_and(|hs| hs.contains(&vote.height));
            if already {
                return VoteOutcome::Ignored;
            }
            tracker
                .reported
                .entry(vote.voter)
                .or_default()
                .push(vote.height);
            return VoteOutcome::Equivocation(Box::new(EquivocationProof {
                first: prior,
                second: vote,
            }));
        }
        tracker.first_vote.insert(slot, vote.clone());
        let key = (vote.height, vote.block_hash);
        let bucket = tracker.votes.entry(key).or_default();
        bucket.insert(vote.voter, vote.clone());
        let collected: Vec<FinalityVote> = bucket.values().cloned().collect();
        let outcome = if collected.len() >= threshold(set.len()) {
            let j = Justification::from_votes(vote.height, vote.block_hash, &collected);
            if j.verify(&set).is_ok() {
                tracker.observed = (j.height, j.block_hash);
                tracker.justification = Some(j.clone());
                VoteOutcome::Finalized(j)
            } else {
                VoteOutcome::Stored
            }
        } else {
            VoteOutcome::Stored
        };
        tracker.prune();
        outcome
    }

    pub async fn handle_finality_vote(&self, vote: FinalityVote) -> bool {
        match self.record_finality_vote(vote).await {
            VoteOutcome::Ignored => false,
            VoteOutcome::Stored => true,
            VoteOutcome::Finalized(j) => {
                tracing::info!(height = j.height, votes = j.votes.len(), "Block finalized");
                true
            }
            VoteOutcome::Equivocation(proof) => {
                tracing::warn!(
                    voter = %hex::encode(&proof.first.voter[..8]),
                    height = proof.first.height,
                    "Validator signed two blocks at one height; reporting it"
                );
                self.sign_and_submit(PALLET_FINALITY, &FinalityCall::ReportEquivocation { proof })
                    .await;
                false
            }
        }
    }

    /// Queues the justification this node holds for the block it is about
    /// to seal, if the chain still needs it.
    ///
    /// Only the producer of the next block submits it. Every validator that
    /// saw a quorum used to submit its own: the first to land was valid and
    /// the others were dropped as `BadJustification`, so a validator that
    /// always lost the race never landed a transaction at all, and anything
    /// else it signed queued behind a nonce that could not apply. Validators
    /// that are not producing keep rebroadcasting their votes, so whoever
    /// holds the next slot can assemble the same justification.
    pub async fn queue_due_justification(&self) {
        let Some(j) = self.finality.lock().await.justification.clone() else {
            return;
        };
        let me = *self.my_peer_id.read().await;
        let in_flight = self.my_inflight_txs.lock().await.iter().any(|(_, e)| {
            e.signer == me
                && crate::chain_node::finalized_height_of(e).is_some_and(|h| h >= j.height)
        });
        let due = {
            let chain = self.chain.read().await;
            finalize_due(chain.finalized().0, chain.hash_at(j.height), &j, in_flight)
        };
        if due {
            self.sign_and_submit(
                PALLET_FINALITY,
                &FinalityCall::Finalize { justification: j },
            )
            .await;
        }
    }

    pub async fn run_finality_voter(self) {
        let mut voted = load_voted(&self.data_dir);
        let mut last_votes: Vec<FinalityVote> = Vec::new();
        let mut tick: u32 = 0;
        loop {
            if self.sleep_or_shutdown(Duration::from_secs(1)).await {
                break;
            }
            tick = tick.wrapping_add(1);
            let (head, target_hash, target) = {
                let chain = self.chain.read().await;
                let head = chain.head_height();
                let target = target_for(head);
                (head, chain.hash_at(target), target)
            };
            let floor = self.observed_finality().await.0;
            if target == 0 || target <= floor || head == 0 {
                continue;
            }
            let Some(hash) = target_hash else {
                continue;
            };
            let mut fresh = Vec::new();
            for key in self.my_validator_keys(target).await {
                let id = hex::encode(key.verifying_key().to_bytes());
                if voted.get(&id).copied().unwrap_or(0) >= target {
                    continue;
                }
                voted.insert(id, target);
                store_voted(&self.data_dir, &voted);
                fresh.push(FinalityVote::sign(&key, target, hash));
            }
            if !fresh.is_empty() {
                last_votes = fresh.clone();
            } else if tick.is_multiple_of(REBROADCAST_EVERY) {
                fresh = last_votes
                    .iter()
                    .filter(|v| v.height > floor)
                    .cloned()
                    .collect();
            }
            for vote in fresh {
                let bytes = vote.to_bytes();
                self.handle_finality_vote(vote).await;
                self.broadcast(message_types::FINALITY_VOTE, &bytes, None)
                    .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::finalize_due;
    use core_crypto::finality::{FinalityVote, Justification};

    fn justification(height: u64, hash: [u8; 32]) -> Justification {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        Justification::from_votes(height, hash, &[FinalityVote::sign(&key, height, hash)])
    }

    #[test]
    fn a_justification_ahead_of_the_chain_is_due() {
        assert!(finalize_due(
            16,
            Some([1u8; 32]),
            &justification(32, [1u8; 32]),
            false
        ));
    }

    #[test]
    fn a_height_already_final_is_not_due() {
        assert!(!finalize_due(
            32,
            Some([1u8; 32]),
            &justification(32, [1u8; 32]),
            false
        ));
        assert!(!finalize_due(
            48,
            Some([1u8; 32]),
            &justification(32, [1u8; 32]),
            false
        ));
    }

    #[test]
    fn a_justification_for_another_block_is_not_due() {
        assert!(!finalize_due(
            16,
            Some([2u8; 32]),
            &justification(32, [1u8; 32]),
            false
        ));
        assert!(!finalize_due(
            16,
            None,
            &justification(32, [1u8; 32]),
            false
        ));
    }

    #[test]
    fn one_already_submitted_is_not_signed_again() {
        assert!(!finalize_due(
            16,
            Some([1u8; 32]),
            &justification(32, [1u8; 32]),
            true
        ));
    }
}
