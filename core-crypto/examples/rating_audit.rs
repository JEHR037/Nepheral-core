// SPDX-License-Identifier: BUSL-1.1
// Replays a chain file and reports every rating: who rated whom, and whether
// the ledger actually counted it.
//
//   cargo run -q --example rating_audit -p core-crypto -- <path/to/chain.bin>

use std::collections::{BTreeSet, HashMap};

use core_crypto::extrinsic::{DecodedCall, PALLET_CONTRIBUTIONS, PALLET_REPUTATION};
use core_crypto::pallets::contributions::ContributionsCall;
use core_crypto::pallets::reputation::{ReputationCall, Verdict, EPOCH_BLOCKS};

fn short(p: &[u8; 32]) -> String {
    hex::encode(&p[..4])
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: rating_audit <chain.bin>");
    let data = std::fs::read(&path).expect("read chain file");
    let body = core_crypto::chain_file::strip_header(&data).expect("not a current chain file");
    let blocks = core_crypto::ChainState::deserialize_blocks(body).expect("blocks decode");

    let mut responder_of: HashMap<[u8; 32], [u8; 32]> = HashMap::new();
    let mut counted_in_epoch: HashMap<([u8; 32], u64), BTreeSet<[u8; 32]>> = HashMap::new();
    let mut last_slot = 0u64;
    let mut first_slot = None;

    println!(
        "{:>6}  {:>5}  {:8} -> {:8}  {:8}  counted",
        "height", "epoch", "rater", "node", "verdict"
    );
    for b in &blocks {
        first_slot.get_or_insert(b.header.slot);
        last_slot = b.header.slot;
        let height = b.header.height;
        for ext in &b.body.extrinsics {
            let Ok(call) = DecodedCall::decode(ext.pallet_idx, &ext.args) else {
                continue;
            };
            match (ext.pallet_idx, call) {
                (
                    PALLET_CONTRIBUTIONS,
                    DecodedCall::Contributions(ContributionsCall::SubmitReceipt { receipt }),
                ) => {
                    responder_of.insert(receipt.claim.query_digest, receipt.claim.responder_pid);
                }
                (
                    PALLET_REPUTATION,
                    DecodedCall::Reputation(ReputationCall::Rate {
                        query_digest,
                        verdict,
                    }),
                ) => {
                    let node = responder_of
                        .get(&query_digest)
                        .copied()
                        .unwrap_or([0u8; 32]);
                    let epoch = height / EPOCH_BLOCKS;
                    let fresh = counted_in_epoch
                        .entry((node, epoch))
                        .or_default()
                        .insert(ext.signer);
                    let v = match verdict {
                        Verdict::Like => "like".to_string(),
                        Verdict::Dislike { reason, .. } => format!("dislike/{}", reason.as_str()),
                    };
                    println!(
                        "{height:>6}  {epoch:>5}  {} -> {}  {v:18}  {}",
                        short(&ext.signer),
                        short(&node),
                        if fresh {
                            "yes"
                        } else {
                            "NO - already rated this node this epoch"
                        }
                    );
                }
                _ => {}
            }
        }
    }

    let head = blocks.last().map(|b| b.header.height).unwrap_or(0);
    let wall_secs = (last_slot - first_slot.unwrap_or(0)) * 2;
    println!();
    println!(
        "head height {head} over {:.1}h of wall clock",
        wall_secs as f64 / 3600.0
    );
    if wall_secs > 0 {
        let per_hour = head as f64 / (wall_secs as f64 / 3600.0);
        println!(
            "{per_hour:.0} blocks/hour -> an epoch of {EPOCH_BLOCKS} blocks lasts {:.1} days at this rate (intended: 1 day)",
            EPOCH_BLOCKS as f64 / per_hour / 24.0
        );
    }
}
