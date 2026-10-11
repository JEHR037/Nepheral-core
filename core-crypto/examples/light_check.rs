// SPDX-License-Identifier: BUSL-1.1
//! Builds the light-sync proof for a chain file and verifies it as a phone
//! would: how many validator-set changes it carries, how big it is on the
//! wire, and whether the state it lands on matches a full replay.
//!
//! cargo run --release --example light_check -p core-crypto -- <chain.bin>

use core_crypto::light::{build_proof, verify, LightProof};
use core_crypto::ChainState;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: light_check <chain.bin>");
    let data = std::fs::read(&path).expect("read chain file");
    let body = core_crypto::chain_file::strip_header(&data).expect("not a current chain file");
    let blocks = ChainState::deserialize_blocks(body).expect("blocks decode");

    let t = std::time::Instant::now();
    let proof = build_proof(ChainState::new(), &blocks).expect("a proof (is anything finalized?)");
    let built_in = t.elapsed();
    let bytes = core_crypto::wire::encode(&proof);
    println!(
        "proof: {} handoff(s), tip at {}, {} KB on the wire, built in {:?}",
        proof.handoffs.len(),
        proof.tip.header.height,
        bytes.len() / 1024,
        built_in
    );
    for (i, h) in proof.handoffs.iter().enumerate() {
        println!(
            "  handoff {}: from {}, {} block(s), to {}",
            i + 1,
            h.base.as_ref().map(|b| b.header.height).unwrap_or(0),
            h.blocks.len(),
            h.justification.height
        );
    }

    let back: LightProof = core_crypto::wire::decode(&bytes).expect("proof decodes");
    let t = std::time::Instant::now();
    let mut light = verify(back, ChainState::new()).expect("proof verifies");
    let verified_in = t.elapsed();
    let tip = light.head_height();
    for b in blocks.iter().filter(|b| b.header.height > tip) {
        light
            .apply_block(b.clone())
            .expect("block after the tip applies");
    }
    let full = ChainState::replay_blocks(blocks);
    assert_eq!(light.head_hash(), full.head_hash(), "same head");
    assert_eq!(light.state_root(), full.state_root(), "same state");
    println!(
        "verified in {:?}; caught up to {} with the same state root as a full replay",
        verified_in,
        light.head_height()
    );
}
