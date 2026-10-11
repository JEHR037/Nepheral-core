// SPDX-License-Identifier: BUSL-1.1
use sha2::{Digest, Sha256};

use crate::identity::PeerId;

pub const PI_BITS: u64 = 0x243F_6A88_85A3_08D3;

fn lottery_score(peer: &PeerId, hour_epoch: u64, msg_len: u64, n_engines_constant: u64) -> u64 {
    let mut h = Sha256::new();
    h.update(peer);
    h.update(hour_epoch.to_le_bytes());
    h.update(msg_len.to_le_bytes());
    h.update(PI_BITS.to_le_bytes());
    h.update(n_engines_constant.to_le_bytes());
    let digest = h.finalize();
    let mut b = [0u8; 8];
    b.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(b)
}

pub fn lottery_select_top_n(
    peers: &[PeerId],
    hour_epoch: u64,
    msg_len: u64,
    n_engines_constant: u64,
    n: usize,
) -> Vec<&PeerId> {
    if peers.is_empty() || n == 0 {
        return Vec::new();
    }
    let mut scored: Vec<(u64, &PeerId)> = peers
        .iter()
        .map(|p| (lottery_score(p, hour_epoch, msg_len, n_engines_constant), p))
        .collect();
    scored.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored.into_iter().take(n).map(|(_, p)| p).collect()
}

pub fn lottery_select(
    peers: &[PeerId],
    hour_epoch: u64,
    msg_len: u64,
    n_engines_constant: u64,
) -> Option<&PeerId> {
    lottery_select_top_n(peers, hour_epoch, msg_len, n_engines_constant, 1)
        .into_iter()
        .next()
}

pub fn rank_candidates_weighted<'a>(
    peers: &'a [PeerId],
    weights: &[u8],
    seed: &[u8; 32],
    n: usize,
) -> Vec<&'a PeerId> {
    if peers.is_empty() || n == 0 || weights.len() != peers.len() {
        return Vec::new();
    }
    let mut scored: Vec<([u8; 32], &PeerId)> = peers
        .iter()
        .zip(weights)
        .map(|(p, w)| {
            let best = (0..(*w).max(1))
                .map(|ticket| {
                    let mut h = Sha256::new();
                    h.update(b"nepheral/lottery-rank/v3");
                    h.update(seed);
                    h.update(p);
                    h.update([ticket]);
                    let out: [u8; 32] = h.finalize().into();
                    out
                })
                .min()
                .unwrap_or([0xFF; 32]);
            (best, p)
        })
        .collect();
    scored.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored.dedup_by(|a, b| a.1 == b.1);
    scored.into_iter().take(n).map(|(_, p)| p).collect()
}

pub fn current_hour_epoch() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 3600)
        .unwrap_or(0)
}

pub const DIMENSION_BASE: u64 = 25;

const fn ilog2(x: u64) -> u64 {
    if x <= 1 {
        0
    } else {
        63 - x.leading_zeros() as u64
    }
}

pub fn compute_dimension(model_params_b: u64, output_tokens: u64, gen_time_ms: u64) -> u64 {
    let token_units = (output_tokens / 100).max(1);
    let token_log = (ilog2(1 + token_units) + 1).max(1);
    let model_factor = model_params_b.max(1);
    let time_log = ilog2(1 + gen_time_ms / 100);
    let time_bonus = (time_log / 2).min(4);
    let core = DIMENSION_BASE
        .saturating_mul(token_log)
        .saturating_mul(model_factor);
    core.saturating_add(time_bonus)
}

pub const VRF_SEED_DOMAIN: &[u8] = b"nepheral-vrf-lottery-v1";

pub fn vrf_seed(message_id: u64, sender_pid: &PeerId, hour_epoch: u64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(VRF_SEED_DOMAIN);
    h.update(message_id.to_le_bytes());
    h.update(sender_pid);
    h.update(hour_epoch.to_le_bytes());
    h.finalize().into()
}

fn vrf_message(seed: &[u8; 32]) -> [u8; 32] {
    crate::network::signing_digest(crate::network::domains::VRF, &[seed])
}

pub fn vrf_evaluate(
    signing_key: &ed25519_dalek::SigningKey,
    seed: &[u8; 32],
) -> ([u8; 32], [u8; 64]) {
    use ed25519_dalek::Signer;
    let signature = signing_key.sign(&vrf_message(seed));
    let sig_bytes = signature.to_bytes();
    let mut h = Sha256::new();
    h.update(VRF_SEED_DOMAIN);
    h.update(b"output");
    h.update(sig_bytes);
    let output: [u8; 32] = h.finalize().into();
    (output, sig_bytes)
}

pub fn vrf_verify(
    pubkey: &ed25519_dalek::VerifyingKey,
    seed: &[u8; 32],
    proof: &[u8; 64],
) -> Option<[u8; 32]> {
    let sig = ed25519_dalek::Signature::from_bytes(proof);
    pubkey.verify_strict(&vrf_message(seed), &sig).ok()?;
    let mut h = Sha256::new();
    h.update(VRF_SEED_DOMAIN);
    h.update(b"output");
    h.update(proof);
    Some(h.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(b: u8) -> PeerId {
        let mut id = [0u8; 32];
        id[0] = b;
        id
    }

    fn rand_keypair() -> (
        ed25519_dalek::SigningKey,
        ed25519_dalek::VerifyingKey,
        PeerId,
    ) {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let sk = ed25519_dalek::SigningKey::from_bytes(&bytes);
        let vk = sk.verifying_key();
        let pid = vk.to_bytes();
        (sk, vk, pid)
    }

    #[test]
    fn vrf_evaluate_is_deterministic() {
        let (sk, _, _) = rand_keypair();
        let seed = vrf_seed(42, &[1u8; 32], 7);
        let (out1, _) = vrf_evaluate(&sk, &seed);
        let (out2, _) = vrf_evaluate(&sk, &seed);
        assert_eq!(out1, out2);
    }

    #[test]
    fn vrf_verify_accepts_honest_proof() {
        let (sk, vk, _) = rand_keypair();
        let seed = vrf_seed(99, &[3u8; 32], 0);
        let (declared, proof) = vrf_evaluate(&sk, &seed);
        let recovered = vrf_verify(&vk, &seed, &proof).expect("honest proof verifies");
        assert_eq!(declared, recovered);
    }

    #[test]
    fn vrf_verify_rejects_wrong_seed() {
        let (sk, vk, _) = rand_keypair();
        let seed = vrf_seed(99, &[3u8; 32], 0);
        let (_, proof) = vrf_evaluate(&sk, &seed);
        let other_seed = vrf_seed(100, &[3u8; 32], 0);
        assert!(vrf_verify(&vk, &other_seed, &proof).is_none());
    }

    #[test]
    fn vrf_verify_rejects_wrong_pubkey() {
        let (sk, _, _) = rand_keypair();
        let (_, vk_other, _) = rand_keypair();
        let seed = vrf_seed(99, &[3u8; 32], 0);
        let (_, proof) = vrf_evaluate(&sk, &seed);
        assert!(vrf_verify(&vk_other, &seed, &proof).is_none());
    }

    #[test]
    fn a_vrf_proof_is_never_a_valid_extrinsic_signature() {
        use crate::extrinsic::{Extrinsic, PALLET_BALANCES};
        use crate::pallets::balances::BalancesCall;
        let (victim_sk, _, victim_pid) = rand_keypair();
        let call = BalancesCall::Transfer {
            to: [0xEE; 32],
            amount: 500,
        };
        let mut forged = Extrinsic::sign(&victim_sk, PALLET_BALANCES, &call, victim_pid, 1);
        let seed = forged.hash_for_signing();
        let (_, proof) = vrf_evaluate(&victim_sk, &seed);
        forged.signature = proof;
        assert!(forged.verify_signature().is_err());
    }

    #[test]
    fn rank_candidates_is_order_independent_and_seeded() {
        let peers = vec![pid(1), pid(2), pid(3), pid(4)];
        let mut reversed = peers.clone();
        reversed.reverse();
        let w = vec![10u8; peers.len()];
        let a = rank_candidates_weighted(&peers, &w, &[1u8; 32], 3);
        let b = rank_candidates_weighted(&reversed, &w, &[1u8; 32], 3);
        assert_eq!(a, b);
        let spread: std::collections::HashSet<PeerId> = (0..64u8)
            .map(|i| *rank_candidates_weighted(&peers, &w, &[i; 32], 1)[0])
            .collect();
        assert!(spread.len() > 1);
    }

    #[test]
    fn vrf_attacker_cannot_predict_other_candidates_output() {
        let (sk_alice, vk_alice, _pid_alice) = rand_keypair();
        let (sk_bob, vk_bob, _pid_bob) = rand_keypair();
        let seed = vrf_seed(7, &[9u8; 32], 0);
        let (alice_out, _) = vrf_evaluate(&sk_alice, &seed);
        let (bob_out, _) = vrf_evaluate(&sk_bob, &seed);
        assert_ne!(
            alice_out, bob_out,
            "different keys must produce different outputs for the same seed"
        );
        let _ = (vk_alice, vk_bob);
    }

    #[test]
    fn vrf_seed_changes_with_message_id() {
        let s1 = vrf_seed(1, &[0u8; 32], 0);
        let s2 = vrf_seed(2, &[0u8; 32], 0);
        assert_ne!(s1, s2);
    }

    #[test]
    fn empty_peers_returns_none() {
        assert!(lottery_select(&[], 0, 0, 0).is_none());
    }

    #[test]
    fn dimension_baseline_qwen4b_lands_near_legacy_constant() {
        let d = compute_dimension(4, 500, 10_000);
        assert!(
            (150..=400).contains(&d),
            "Qwen-4B 500-token baseline = {d} not in [150, 400]"
        );
    }

    #[test]
    fn dimension_72b_dominates_4b() {
        let d4 = compute_dimension(4, 500, 1_000);
        let d72 = compute_dimension(72, 500, 1_000);
        assert!(d72 >= d4 * 10, "d72={d72} d4={d4}");
    }

    #[test]
    fn dimension_logarithmic_in_output_tokens() {
        let d_short = compute_dimension(72, 500, 1_000);
        let d_4x = compute_dimension(72, 2000, 1_000);
        assert!(d_4x < d_short * 4, "4× tokens should not be ≥4× dimension");
        assert!(d_4x >= d_short, "more tokens should not decrease dimension");
    }

    #[test]
    fn dimension_monotonic_in_model_size() {
        let a = compute_dimension(4, 500, 1_000);
        let b = compute_dimension(32, 500, 1_000);
        let c = compute_dimension(72, 500, 1_000);
        let d = compute_dimension(405, 500, 1_000);
        assert!(a < b && b < c && c < d, "got: {a} {b} {c} {d}");
    }

    #[test]
    fn dimension_time_bonus_bounded() {
        let frontier_fast = compute_dimension(405, 500, 100);
        let small_slow = compute_dimension(4, 500, 60_000);
        assert!(frontier_fast > small_slow * 10);
    }

    #[test]
    fn dimension_handles_zero_inputs_gracefully() {
        let d = compute_dimension(4, 0, 0);
        assert!(d > 0);
    }

    #[test]
    fn single_peer_always_wins() {
        let peers = [pid(1)];
        let winner = lottery_select(&peers, 12345, 42, 3).unwrap();
        assert_eq!(*winner, pid(1));
    }

    #[test]
    fn deterministic_for_same_inputs() {
        let peers = [pid(1), pid(2), pid(3), pid(4)];
        let a = lottery_select(&peers, 100, 50, 5).unwrap();
        let b = lottery_select(&peers, 100, 50, 5).unwrap();
        assert_eq!(*a, *b);
    }

    #[test]
    fn order_of_input_does_not_matter() {
        let peers_asc = [pid(1), pid(2), pid(3), pid(4)];
        let peers_desc = [pid(4), pid(3), pid(2), pid(1)];
        let a = lottery_select(&peers_asc, 100, 50, 5).unwrap();
        let b = lottery_select(&peers_desc, 100, 50, 5).unwrap();
        assert_eq!(*a, *b);
    }

    #[test]
    fn different_msg_len_can_change_winner() {
        let peers = [pid(1), pid(2), pid(3), pid(4), pid(5)];
        let mut winners = std::collections::HashSet::new();
        for msg_len in 0..200u64 {
            winners.insert(*lottery_select(&peers, 100, msg_len, 5).unwrap());
        }
        assert!(winners.len() >= 2, "lottery should vary with msg_len");
    }

    #[test]
    fn top_n_empty_peers_returns_empty() {
        assert!(lottery_select_top_n(&[], 0, 0, 0, 3).is_empty());
    }

    #[test]
    fn top_n_zero_returns_empty() {
        let peers = [pid(1), pid(2), pid(3)];
        assert!(lottery_select_top_n(&peers, 0, 0, 0, 0).is_empty());
    }

    #[test]
    fn top_n_clamps_to_peer_count() {
        let peers = [pid(1), pid(2)];
        let result = lottery_select_top_n(&peers, 100, 50, 5, 10);
        assert_eq!(result.len(), 2, "n > peers.len() must clamp, not panic");
    }

    #[test]
    fn top_n_first_matches_lottery_select() {
        let peers = [pid(1), pid(2), pid(3), pid(4), pid(5)];
        for msg_len in 0..50u64 {
            let solo = lottery_select(&peers, 100, msg_len, 5).unwrap();
            let top = lottery_select_top_n(&peers, 100, msg_len, 5, 3);
            assert_eq!(*solo, *top[0], "top_n[0] must equal lottery_select");
        }
    }

    #[test]
    fn top_n_returns_distinct_peers() {
        let peers = [pid(1), pid(2), pid(3), pid(4), pid(5)];
        let top = lottery_select_top_n(&peers, 100, 42, 5, 3);
        assert_eq!(top.len(), 3);
        let unique: std::collections::HashSet<_> = top.iter().collect();
        assert_eq!(unique.len(), 3, "ranks must be distinct peers");
    }

    #[test]
    fn top_n_deterministic_across_input_reorder() {
        let asc = [pid(1), pid(2), pid(3), pid(4), pid(5)];
        let desc = [pid(5), pid(4), pid(3), pid(2), pid(1)];
        let a: Vec<PeerId> = lottery_select_top_n(&asc, 100, 50, 5, 3)
            .into_iter()
            .copied()
            .collect();
        let b: Vec<PeerId> = lottery_select_top_n(&desc, 100, 50, 5, 3)
            .into_iter()
            .copied()
            .collect();
        assert_eq!(a, b, "input order must not affect ranking");
    }

    #[test]
    fn top_n_load_distributes_over_msg_lengths() {
        let peers = [pid(1), pid(2), pid(3), pid(4), pid(5)];
        let mut primary_winners = std::collections::HashSet::new();
        for msg_len in 0..1000u64 {
            let top = lottery_select_top_n(&peers, 100, msg_len, 5, 1);
            primary_winners.insert(*top[0]);
        }
        assert_eq!(
            primary_winners.len(),
            peers.len(),
            "every peer must win primary at least once across 1000 messages"
        );
    }

    #[test]
    fn weighted_ranking_favours_higher_weights_proportionally() {
        let peers: Vec<PeerId> = vec![[1u8; 32], [2u8; 32]];
        let weights = [90u8, 10u8];
        let mut first = 0usize;
        for i in 0..2_000u32 {
            let mut seed = [0u8; 32];
            seed[..4].copy_from_slice(&i.to_le_bytes());
            if *rank_candidates_weighted(&peers, &weights, &seed, 1)[0] == [1u8; 32] {
                first += 1;
            }
        }
        assert!((1_700..1_900).contains(&first), "{first}");
        assert!(rank_candidates_weighted(&peers, &[1u8], &[0u8; 32], 1).is_empty());
    }
}
