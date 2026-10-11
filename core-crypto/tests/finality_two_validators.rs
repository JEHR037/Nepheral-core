// SPDX-License-Identifier: BUSL-1.1
//! Reproduction of what the live testnet does: exactly two validators, the
//! founder plus one node added by `Authority::Add`, both voting at a height
//! that is a multiple of FINALITY_INTERVAL.
//!
//! The four-validator end-to-end test covers threshold(4) == 3. Two
//! validators need threshold(2) == 2, i.e. unanimity, which is the shape a
//! small network actually starts in.

use core_crypto::block::{Block, BlockBody, BlockHeader, SLOT_DURATION_MS};
use core_crypto::extrinsic::{Extrinsic, PALLET_AUTHORITY, PALLET_FINALITY};
use core_crypto::finality::{threshold, FinalityCall, FinalityVote, Justification};
use core_crypto::pallets::authority::AuthorityCall;
use core_crypto::ChainState;
use ed25519_dalek::SigningKey;

fn seal(chain: &ChainState, key: &SigningKey, slot: u64, exts: Vec<Extrinsic>) -> Block {
    let author = core_crypto::validators::authority_peer_id(&key.verifying_key());
    let mut block = Block {
        header: BlockHeader {
            version: 1,
            height: chain.head_height() + 1,
            slot,
            parent_hash: chain.head_hash(),
            state_root: [0u8; 32],
            extrinsics_root: [0u8; 32],
            lottery_root: [0u8; 32],
            author,
            signature: [0u8; 64],
            timestamp_ms: slot * SLOT_DURATION_MS,
        },
        body: BlockBody {
            extrinsics: exts,
            lottery_results: Vec::new(),
        },
    };
    block.header.extrinsics_root = block.extrinsics_root();
    block.header.lottery_root = block.lottery_root();
    let mut probe = chain.state_without_history();
    if let Some(last) = chain.blocks.last() {
        probe.blocks.push(last.clone());
    }
    probe.apply_block_unverified(block.clone()).unwrap();
    block.header.state_root = probe.state_root();
    block.header.sign(key);
    block
}

#[test]
fn two_validators_finalize_a_height_on_chain() {
    let founder = SigningKey::from_bytes(&[1u8; 32]);
    let partner = SigningKey::from_bytes(&[2u8; 32]);
    let founder_pid = founder.verifying_key().to_bytes();
    let partner_pid = partner.verifying_key().to_bytes();

    let mut chain = ChainState::with_genesis_authority(founder_pid);

    // Founder admits the second validator, exactly as NEPHERAL_TOP_TIER_NODES does.
    let add = Extrinsic::sign(
        &founder,
        PALLET_AUTHORITY,
        &AuthorityCall::Add {
            pubkey: partner_pid,
            channels: vec!["chat".into()],
            attestation_hash: [0u8; 32],
        },
        founder_pid,
        1,
    );

    // Slots must map to the right primary: authorities[slot % n].
    let keys = [&founder, &partner];
    // Block 1 carries the Add. The founder is the only authority at that
    // point, so it is the primary for every slot.
    let mut slot = 1_000u64;
    let first = seal(&chain, &founder, slot, vec![add]);
    chain.apply_block(first).unwrap();
    assert_eq!(
        chain.validator_set().len(),
        2,
        "two validators after the Add"
    );

    // From here the set has two members, so the slot's primary alternates.
    while chain.head_height() < 20 {
        slot += 1;
        let idx = (slot as usize) % 2;
        let author_key = keys[idx];
        let block = seal(&chain, author_key, slot, Vec::new());
        chain.apply_block(block).unwrap();
    }

    let target = 16u64;
    let target_hash = chain.hash_at(target).expect("hash at 16");
    let set = chain.authority.validators_at(target);
    assert_eq!(set.len(), 2, "validators_at({target}) must see both");
    assert_eq!(threshold(set.len()), 2, "two validators need unanimity");

    let votes = vec![
        FinalityVote::sign(&founder, target, target_hash),
        FinalityVote::sign(&partner, target, target_hash),
    ];
    let j = Justification::from_votes(target, target_hash, &votes);
    assert_eq!(j.votes.len(), 2);
    j.verify(&set)
        .expect("justification verifies against the live set");

    // This is the step the live network never gets past.
    let finalize = Extrinsic::sign(
        &founder,
        PALLET_FINALITY,
        &FinalityCall::Finalize { justification: j },
        founder_pid,
        2,
    );
    let height = chain.head_height() + 1;
    let author = core_crypto::validators::authority_peer_id(&founder.verifying_key());
    chain
        .apply_extrinsic(&finalize, height, height * SLOT_DURATION_MS, &author)
        .expect("Finalize must apply");

    assert_eq!(chain.finalized().0, target, "on-chain finalized height");
    assert_eq!(chain.finalized().1, target_hash, "on-chain finalized hash");
}

/// The block producer never validates against the full chain: it dry-runs
/// pending extrinsics against `state_without_history()`, which drops `blocks`
/// and keeps only `recent_hashes`. That is the path the live network takes.
#[test]
fn a_justification_survives_the_producers_dry_run_probe() {
    let founder = SigningKey::from_bytes(&[1u8; 32]);
    let partner = SigningKey::from_bytes(&[2u8; 32]);
    let founder_pid = founder.verifying_key().to_bytes();
    let partner_pid = partner.verifying_key().to_bytes();

    let mut chain = ChainState::with_genesis_authority(founder_pid);
    let add = Extrinsic::sign(
        &founder,
        PALLET_AUTHORITY,
        &AuthorityCall::Add {
            pubkey: partner_pid,
            channels: vec!["chat".into()],
            attestation_hash: [0u8; 32],
        },
        founder_pid,
        1,
    );
    let keys = [&founder, &partner];
    let mut slot = 1_000u64;
    let first = seal(&chain, &founder, slot, vec![add]);
    chain.apply_block(first).unwrap();
    while chain.head_height() < 20 {
        slot += 1;
        let block = seal(&chain, keys[(slot as usize) % 2], slot, Vec::new());
        chain.apply_block(block).unwrap();
    }

    let target = 16u64;
    let target_hash = chain.hash_at(target).unwrap();
    let set = chain.authority.validators_at(target);
    let votes = vec![
        FinalityVote::sign(&founder, target, target_hash),
        FinalityVote::sign(&partner, target, target_hash),
    ];
    let j = Justification::from_votes(target, target_hash, &votes);
    j.verify(&set).expect("verifies against the live set");

    let finalize = Extrinsic::sign(
        &founder,
        PALLET_FINALITY,
        &FinalityCall::Finalize { justification: j },
        founder_pid,
        2,
    );

    // Exactly what run_block_producer does before putting an extrinsic in a block.
    let mut probe = chain.state_without_history();
    let height = chain.head_height() + 1;
    let author = core_crypto::validators::authority_peer_id(&founder.verifying_key());
    probe
        .apply_extrinsic(&finalize, height, height * SLOT_DURATION_MS, &author)
        .expect("Finalize must survive the producer probe");
    assert_eq!(probe.finalized().0, target);
}

/// What the live testnet does at every finalized height: each validator that
/// sees the quorum submits its own copy of the justification, and both land
/// in the same block. The first finalizes; the second is stale by the time it
/// is applied and must be refused rather than finalize anything twice.
#[test]
fn both_validators_submitting_the_same_justification_finalize_once() {
    let founder = SigningKey::from_bytes(&[1u8; 32]);
    let partner = SigningKey::from_bytes(&[2u8; 32]);
    let founder_pid = founder.verifying_key().to_bytes();
    let partner_pid = partner.verifying_key().to_bytes();

    let mut chain = ChainState::with_genesis_authority(founder_pid);
    let add = Extrinsic::sign(
        &founder,
        PALLET_AUTHORITY,
        &AuthorityCall::Add {
            pubkey: partner_pid,
            channels: vec!["chat".into()],
            attestation_hash: [0u8; 32],
        },
        founder_pid,
        1,
    );
    let keys = [&founder, &partner];
    let mut slot = 1_000u64;
    let first = seal(&chain, &founder, slot, vec![add]);
    chain.apply_block(first).unwrap();
    while chain.head_height() < 20 {
        slot += 1;
        let block = seal(&chain, keys[(slot as usize) % 2], slot, Vec::new());
        chain.apply_block(block).unwrap();
    }

    let target = 16u64;
    let hash = chain.hash_at(target).unwrap();
    let votes = vec![
        FinalityVote::sign(&founder, target, hash),
        FinalityVote::sign(&partner, target, hash),
    ];
    let j = Justification::from_votes(target, hash, &votes);
    let from_partner = Extrinsic::sign(
        &partner,
        PALLET_FINALITY,
        &FinalityCall::Finalize {
            justification: j.clone(),
        },
        partner_pid,
        1,
    );
    let from_founder = Extrinsic::sign(
        &founder,
        PALLET_FINALITY,
        &FinalityCall::Finalize { justification: j },
        founder_pid,
        2,
    );

    let height = chain.head_height() + 1;
    let author = core_crypto::validators::authority_peer_id(&partner.verifying_key());
    let ts = (slot + 1) * SLOT_DURATION_MS;
    chain
        .apply_extrinsic(&from_partner, height, ts, &author)
        .expect("the first copy finalizes");
    assert_eq!(chain.finalized().0, target);
    assert!(
        matches!(
            chain.apply_extrinsic(&from_founder, height, ts, &author),
            Err(core_crypto::ChainError::BadJustification)
        ),
        "the second copy is stale"
    );
    assert_eq!(chain.finalized(), (target, hash));
}
