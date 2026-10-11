// SPDX-License-Identifier: BUSL-1.1
//! What an identity with no history can do.
//!
//! A key costs nothing to make, and presence accepts any signed HELLO. So a
//! flood of fresh keys could fill the custody slots and withhold their
//! fragments (a denial of service), or claim a model and be ranked to read
//! questions (a privacy problem: the bundle is encrypted to the ranked
//! responders). History on chain is what a flood cannot fake quickly: a node
//! is *established* once the chain has recorded it doing something - a
//! credited fragment, an answer, a share - and has *tenure* an hour after
//! that. An honest new node gets there by holding fragments, which every
//! node is asked to do.
//!
//! - Questions go only to candidates with tenure, while there are any.
//! - Custodians with no history are never more than those with one, so
//!   fresh keys alone cannot withhold enough fragments to stop a question.
//!   A quarter of the slots stay open to them, so new nodes still get the
//!   custody that establishes them.
//! - When the presence table is full, a known node pushes out a fresh one
//!   rather than being turned away.
//!
//! All of this is local policy: what an asker picks and what a node keeps
//! in memory. No other node checks it, so it needs no protocol change.

use core_crypto::identity::PeerId;
use core_crypto::ChainState;
use mesh_network::protocol::MAX_CUSTODIANS;

/// How long after its first recorded work a node can be handed questions.
pub const CANDIDATE_MIN_TENURE_MS: u64 = 60 * 60 * 1_000;

/// With no established node to compare against (a new network), how many
/// fresh custodians a question may still have.
pub const MIN_FRESH_CUSTODIANS: usize = 4;

/// Custody slots kept open to fresh nodes, so the established ones never
/// take them all and newcomers can earn their history.
pub const FRESH_CUSTODIAN_RESERVE: usize = MAX_CUSTODIANS / 4;

/// The chain has recorded this peer doing something, or it is a validator.
pub fn is_established(chain: &ChainState, pid: &PeerId) -> bool {
    chain.authority.is_authority_signer(pid) || chain.contributions.first_seen_of(pid).is_some()
}

/// Established for at least `CANDIDATE_MIN_TENURE_MS`, or a validator.
pub fn has_tenure(chain: &ChainState, pid: &PeerId, now_ms: u64) -> bool {
    chain.authority.is_authority_signer(pid)
        || chain
            .contributions
            .first_seen_of(pid)
            .is_some_and(|first| now_ms.saturating_sub(first) >= CANDIDATE_MIN_TENURE_MS)
}

/// The candidates with tenure, when there is at least one; all of them
/// otherwise, so a young network still gets answers.
pub fn prefer_tenured<T>(candidates: Vec<T>, tenured: impl Fn(&T) -> bool) -> Vec<T> {
    if candidates.iter().any(&tenured) {
        candidates.into_iter().filter(|c| tenured(c)).collect()
    } else {
        candidates
    }
}

/// Who holds a question's fragments. `ranked` (the responders, who must
/// collect) come first whatever their history; then established nodes; then
/// fresh ones, never more of them than established ones in the set. The
/// other two lists arrive shuffled and without the ranked nodes.
pub fn choose_custodians(
    ranked: &[PeerId],
    established: Vec<PeerId>,
    fresh: Vec<PeerId>,
    is_established: impl Fn(&PeerId) -> bool,
) -> Vec<PeerId> {
    let mut out: Vec<PeerId> = ranked.to_vec();
    out.truncate(MAX_CUSTODIANS);
    let reserve = fresh.len().min(FRESH_CUSTODIAN_RESERVE);
    let room = MAX_CUSTODIANS.saturating_sub(out.len() + reserve);
    out.extend(established.into_iter().take(room));
    let established_in = out.iter().filter(|p| is_established(p)).count();
    let fresh_in = out.len() - established_in;
    let fresh_cap = if established_in == 0 {
        MIN_FRESH_CUSTODIANS
    } else {
        established_in
    };
    let more = fresh_cap
        .saturating_sub(fresh_in)
        .min(MAX_CUSTODIANS.saturating_sub(out.len()));
    out.extend(fresh.into_iter().take(more));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(range: std::ops::Range<u8>) -> Vec<PeerId> {
        range.map(|n| [n; 32]).collect()
    }

    /// Ids from 100 up are established in these tests.
    fn est(p: &PeerId) -> bool {
        p[0] >= 100
    }

    #[test]
    fn fresh_custodians_never_outnumber_established_ones() {
        let out = choose_custodians(&[], ids(100..103), ids(0..50), est);
        let fresh = out.iter().filter(|p| !est(p)).count();
        assert_eq!(out.len() - fresh, 3);
        assert_eq!(fresh, 3, "as many fresh as established, no more");
    }

    #[test]
    fn a_flood_of_fresh_keys_cannot_stop_reconstruction() {
        // Shares need ceil(n/2) to rebuild; fresh keys withholding all of
        // theirs must leave that many.
        for established in 1..40u8 {
            let out = choose_custodians(&[], ids(100..100 + established), ids(0..90), est);
            let n = out.len();
            let fresh = out.iter().filter(|p| !est(p)).count();
            assert!(
                n - fresh >= n.div_ceil(2),
                "established {established}: {fresh} of {n} fresh"
            );
        }
    }

    #[test]
    fn newcomers_keep_a_share_of_the_slots_in_a_big_network() {
        let out = choose_custodians(&[], ids(100..200), ids(0..30), est);
        assert_eq!(out.len(), MAX_CUSTODIANS);
        assert_eq!(
            out.iter().filter(|p| !est(p)).count(),
            FRESH_CUSTODIAN_RESERVE
        );
    }

    #[test]
    fn a_network_with_no_history_still_gets_custodians() {
        let out = choose_custodians(&[], Vec::new(), ids(0..10), est);
        assert_eq!(out.len(), MIN_FRESH_CUSTODIANS);
    }

    #[test]
    fn ranked_responders_come_first_and_count_against_the_cap() {
        let ranked = ids(0..2);
        let out = choose_custodians(&ranked, ids(100..102), ids(2..20), est);
        assert_eq!(&out[..2], &ranked[..]);
        let fresh = out.iter().filter(|p| !est(p)).count();
        assert_eq!(
            fresh, 2,
            "the two fresh responders already fill the fresh cap"
        );
    }

    #[test]
    fn candidates_with_tenure_are_preferred_when_there_are_any() {
        let picked = prefer_tenured(vec![1, 2, 3, 4], |n| *n % 2 == 0);
        assert_eq!(picked, vec![2, 4]);
        let all = prefer_tenured(vec![1, 3], |n| *n % 2 == 0);
        assert_eq!(all, vec![1, 3], "nobody has tenure: everyone stays");
    }
}
