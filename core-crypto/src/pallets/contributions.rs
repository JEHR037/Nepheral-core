// SPDX-License-Identifier: BUSL-1.1
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::balances::BalancesStorage;
use super::{ApplyCtx, Pallet, PalletError};
use crate::identity::PeerId;

pub const SHARES_PER_CREDIT: u64 = 1_000;

/// Fragments held for answered questions per share-equivalent. Holding is
/// the most passive way to give to the network, so it earns least: at the
/// default rate of 1,000 equivalents per credit, 2,000 held fragments make
/// one N-AI. It floats with the elastic rate like everything else.
pub const CUSTODY_PER_SHARE: u64 = 2;

#[deprecated(note = "use LotteryResult.dimension via compute_dimension instead")]
pub const LOTTERY_WIN_SHARE_VALUE: u64 = 200;

pub const LEECHER_RATIO_NUM: u64 = 1;
pub const LEECHER_RATIO_DEN: u64 = 2;

pub const LEECHER_GRACE_THRESHOLD: u64 = 50;

pub const MATURITY_DIM_WEIGHT: u64 = 10;

pub const LEECHER_GRACE_WINDOW_MS: u64 = 7 * 24 * 60 * 60 * 1_000;

pub const MAX_RECEIPTS_PER_SENDER_PER_HOUR: u32 = 120;

pub const MAX_RECEIPTS_PER_PAIR_PER_HOUR: u32 = 30;
/// From rules v2, with no established custodian in a receipt: how many
/// fresh ones it may still credit.
pub const MIN_FRESH_CUSTODIANS_CREDITED: usize = 4;

const MS_PER_HOUR: u64 = 3_600_000;

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct RedeemedCounts {
    pub shares: u64,
    #[serde(alias = "lotteries")]
    pub lottery_dimension: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ContributionsStorage {
    pub share_counts: HashMap<PeerId, u64>,
    pub shares_consumed: HashMap<PeerId, u64>,
    pub wins_by_peer: HashMap<PeerId, u64>,
    #[serde(default)]
    pub lottery_dimension_by_peer: HashMap<PeerId, u64>,
    pub redeemed: HashMap<PeerId, RedeemedCounts>,
    /// When each peer was first seen doing work for the network.
    ///
    /// Recorded on a peer's first share or first served request, never on
    /// account creation: an identity that has existed for a week without
    /// contributing anything has proved nothing, and minting fresh keys is
    /// free. Anything gating on "time in the network" wants this, not a
    /// registration date.
    #[serde(default)]
    pub first_seen_ms: HashMap<PeerId, u64>,
    #[serde(default)]
    pub receipt_digests: HashMap<u64, Vec<[u8; 32]>>,
    #[serde(default)]
    pub receipts_by_sender: HashMap<(u64, PeerId), u32>,
    #[serde(default)]
    pub receipts_by_pair: HashMap<(u64, PeerId, PeerId), u32>,
    /// Fragments each peer held for answered questions. Every
    /// `CUSTODY_PER_SHARE` of them add one to `share_counts`, which is what
    /// redeeming and the leecher rule read.
    #[serde(default)]
    pub custody_by_peer: HashMap<PeerId, u64>,
}

impl ContributionsStorage {
    pub fn shares_of(&self, peer: &PeerId) -> u64 {
        self.share_counts.get(peer).copied().unwrap_or(0)
    }

    pub fn consumed_of(&self, peer: &PeerId) -> u64 {
        self.shares_consumed.get(peer).copied().unwrap_or(0)
    }

    pub fn wins_of(&self, peer: &PeerId) -> u64 {
        self.wins_by_peer.get(peer).copied().unwrap_or(0)
    }

    pub fn custody_of(&self, peer: &PeerId) -> u64 {
        self.custody_by_peer.get(peer).copied().unwrap_or(0)
    }

    /// One fragment held for an answered question.
    pub fn record_custody(&mut self, peer: PeerId, now_ms: u64) {
        self.note_first_seen(peer, now_ms);
        let held = self.custody_by_peer.entry(peer).or_insert(0);
        *held += 1;
        if held.is_multiple_of(CUSTODY_PER_SHARE) {
            *self.share_counts.entry(peer).or_insert(0) += 1;
        }
    }

    pub fn redeemed_of(&self, peer: &PeerId) -> RedeemedCounts {
        self.redeemed.get(peer).copied().unwrap_or_default()
    }

    pub fn first_seen_of(&self, peer: &PeerId) -> Option<u64> {
        self.first_seen_ms.get(peer).copied()
    }

    /// Whole days between a peer's first contribution and `now_ms`.
    /// A peer that has never contributed has no tenure at all.
    pub fn days_active(&self, peer: &PeerId, now_ms: u64) -> u64 {
        match self.first_seen_of(peer) {
            Some(first) => now_ms.saturating_sub(first) / (24 * 60 * 60 * 1_000),
            None => 0,
        }
    }

    /// Stamps a peer's first contribution. Later calls are ignored, so tenure
    /// only ever counts from the earliest work recorded.
    fn note_first_seen(&mut self, peer: PeerId, now_ms: u64) {
        self.first_seen_ms.entry(peer).or_insert(now_ms);
    }

    pub fn redeemable(&self, peer: &PeerId) -> (u64, u64) {
        let r = self.redeemed_of(peer);
        let shares = self.shares_of(peer).saturating_sub(r.shares);
        let dimension = self.redeemable_lottery_dimension(peer);
        (shares, dimension)
    }

    fn prune_receipt_windows(&mut self, current_hour: u64) {
        let keep_from = current_hour.saturating_sub(1);
        self.receipt_digests.retain(|h, _| *h >= keep_from);
        self.receipts_by_sender.retain(|(h, _), _| *h >= keep_from);
        self.receipts_by_pair.retain(|(h, _, _), _| *h >= keep_from);
    }

    pub fn record_lottery_win(&mut self, winner: PeerId, dimension: u64) {
        *self.wins_by_peer.entry(winner).or_insert(0) += 1;
        *self.lottery_dimension_by_peer.entry(winner).or_insert(0) = self
            .lottery_dimension_by_peer
            .get(&winner)
            .copied()
            .unwrap_or(0)
            .saturating_add(dimension);
    }

    pub fn redeemable_lottery_dimension(&self, peer: &PeerId) -> u64 {
        let total = self
            .lottery_dimension_by_peer
            .get(peer)
            .copied()
            .unwrap_or(0);
        let already = self.redeemed_of(peer).lottery_dimension;
        total.saturating_sub(already)
    }

    /// What a peer has given the network, as the receipt model actually
    /// records it.
    ///
    /// `share_counts` alone is not it. That counter is only ever written by
    /// `RecordShare`, a founder-only call that 0.4.x used to attest custody
    /// and that nothing submits any more: 0.5.0 replaced authority-recorded
    /// shares with service receipts. Measuring "gave" by `shares_of` alone
    /// therefore reads zero for every node forever, which silently turns the
    /// leecher rule below into "anyone who ever asked a question is barred
    /// from answering". Answers are the give side receipts do record, and
    /// they are signed by the responder and countersigned by the sender, so
    /// they cannot be minted by either party alone.
    ///
    /// Shares stay in the sum so a founder-attested custody record still
    /// counts if one is ever submitted.
    pub fn contribution_of(&self, peer: &PeerId) -> u64 {
        self.shares_of(peer).saturating_add(self.wins_of(peer))
    }

    pub fn maturity_of(&self, peer: &PeerId) -> u64 {
        let shares = self.shares_of(peer);
        let dim = self
            .lottery_dimension_by_peer
            .get(peer)
            .copied()
            .unwrap_or(0);
        shares.saturating_add(dim.saturating_mul(MATURITY_DIM_WEIGHT))
    }

    pub fn is_lottery_eligible(&self, peer: &PeerId) -> bool {
        self.is_lottery_eligible_at(peer, None)
    }

    pub fn is_lottery_eligible_at(&self, peer: &PeerId, now_ms: Option<u64>) -> bool {
        let consumed = self.consumed_of(peer);
        let within_grace_window = match (now_ms, self.first_seen_of(peer)) {
            (Some(now), Some(first)) => now.saturating_sub(first) < LEECHER_GRACE_WINDOW_MS,
            _ => true,
        };
        if consumed < LEECHER_GRACE_THRESHOLD && within_grace_window {
            return true;
        }
        let given = self.contribution_of(peer);
        given.saturating_mul(LEECHER_RATIO_DEN) >= consumed.saturating_mul(LEECHER_RATIO_NUM)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ContributionsCall {
    RecordShare {
        peer: PeerId,
    },
    RecordConsumption {
        peer: PeerId,
    },
    Redeem {
        receiver: PeerId,
        shares: u64,
        lottery_dimension: u64,
    },
    SubmitReceipt {
        receipt: Box<crate::receipts::ServiceReceipt>,
    },
    /// A receipt plus the custodians the answer was rebuilt from, as the
    /// responder attested them. Appended last, so blocks written before it
    /// existed still decode.
    SubmitReceiptWithCustody {
        receipt: Box<crate::receipts::ServiceReceipt>,
        custody: crate::receipts::CustodyAttestation,
    },
}

pub struct ContributionsPallet;

/// Which named custodians a receipt credits under rules v2. Not those of the
/// asker's or the responder's own owner. Those with no history on chain at
/// most as many as those with one, so a colluding pair cannot mint a crowd
/// of fresh identities a history of their own.
fn custodians_credited_v2(
    storage: &ContributionsStorage,
    identity: &crate::pallets::identity::IdentityStorage,
    sender: PeerId,
    responder: PeerId,
    named: Vec<PeerId>,
) -> Vec<PeerId> {
    let parties = [
        identity.account_of(&sender),
        identity.account_of(&responder),
    ];
    let (established, fresh): (Vec<PeerId>, Vec<PeerId>) = named
        .into_iter()
        .filter(|p| !parties.contains(&identity.account_of(p)))
        .partition(|p| storage.first_seen_of(p).is_some());
    let fresh_cap = if established.is_empty() {
        MIN_FRESH_CUSTODIANS_CREDITED
    } else {
        established.len()
    };
    let mut out = established;
    out.extend(fresh.into_iter().take(fresh_cap));
    out
}

impl ContributionsPallet {
    pub fn apply_with_balances(
        storage: &mut ContributionsStorage,
        balances: &mut BalancesStorage,
        call: ContributionsCall,
        ctx: &mut ApplyCtx<'_>,
    ) -> Result<u64, PalletError> {
        match call {
            ContributionsCall::RecordShare { peer } => {
                storage.note_first_seen(peer, ctx.timestamp_ms);
                *storage.share_counts.entry(peer).or_insert(0) += 1;
                Ok(0)
            }
            ContributionsCall::RecordConsumption { peer } => {
                storage.note_first_seen(peer, ctx.timestamp_ms);
                *storage.shares_consumed.entry(peer).or_insert(0) += 1;
                Ok(0)
            }
            ContributionsCall::SubmitReceipt { receipt } => {
                Self::apply_receipt(storage, *receipt, ctx)?;
                Ok(0)
            }
            ContributionsCall::SubmitReceiptWithCustody { receipt, custody } => {
                custody
                    .verify(&receipt.claim)
                    .map_err(|_| PalletError::InvalidArgs)?;
                let (sender, responder) = (receipt.claim.sender_pid, receipt.claim.responder_pid);
                Self::apply_receipt(storage, *receipt, ctx)?;
                let mut seen = std::collections::HashSet::new();
                let named: Vec<PeerId> = custody
                    .custodians
                    .into_iter()
                    .filter(|p| *p != sender && *p != responder && seen.insert(*p))
                    .collect();
                let credited = if ctx.rules_version >= crate::pallets::upgrade::RULES_V2 {
                    custodians_credited_v2(storage, ctx.identity, sender, responder, named)
                } else {
                    named
                };
                for peer in credited {
                    storage.record_custody(peer, ctx.timestamp_ms);
                }
                Ok(0)
            }
            ContributionsCall::Redeem {
                receiver,
                shares,
                lottery_dimension,
            } => {
                if ctx.signer != &receiver {
                    return Err(PalletError::Unauthorized);
                }
                if shares == 0 && lottery_dimension == 0 {
                    return Err(PalletError::InvalidArgs);
                }
                let (avail_shares, avail_dim) = storage.redeemable(&receiver);
                if shares > avail_shares || lottery_dimension > avail_dim {
                    return Err(PalletError::InvalidArgs);
                }
                let divisor = if ctx.current_shares_per_credit > 0 {
                    ctx.current_shares_per_credit
                } else {
                    SHARES_PER_CREDIT
                };
                let total_share_equivs = shares.saturating_add(lottery_dimension);
                let credits = total_share_equivs / divisor;
                if credits == 0 {
                    return Err(PalletError::InvalidArgs);
                }
                let spent_equivs = credits.saturating_mul(divisor);
                let spent_shares = shares.min(spent_equivs);
                let spent_dim = spent_equivs
                    .saturating_sub(spent_shares)
                    .min(lottery_dimension);
                let counts = storage.redeemed.entry(receiver).or_default();
                counts.shares += spent_shares;
                counts.lottery_dimension += spent_dim;
                balances.mint(receiver, credits);
                Ok(credits)
            }
        }
    }

    fn apply_receipt(
        storage: &mut ContributionsStorage,
        receipt: crate::receipts::ServiceReceipt,
        ctx: &mut ApplyCtx<'_>,
    ) -> Result<(), PalletError> {
        let claim = &receipt.claim;
        if ctx.signer != &claim.sender_pid {
            return Err(PalletError::Unauthorized);
        }
        receipt.verify().map_err(|_| PalletError::InvalidArgs)?;
        let v2 = ctx.rules_version >= crate::pallets::upgrade::RULES_V2;
        // Counted per owner from v2: a wallet's nodes share one allowance,
        // and nodes of one owner cannot pay each other at all.
        let (sender_key, responder_key) = if v2 {
            (
                ctx.identity.account_of(&claim.sender_pid),
                ctx.identity.account_of(&claim.responder_pid),
            )
        } else {
            (claim.sender_pid, claim.responder_pid)
        };
        if v2 && sender_key == responder_key {
            return Err(PalletError::Unauthorized);
        }
        let block_hour = ctx.timestamp_ms / MS_PER_HOUR;
        if claim.hour_epoch > block_hour || block_hour.saturating_sub(claim.hour_epoch) > 1 {
            return Err(PalletError::InvalidArgs);
        }
        storage.prune_receipt_windows(block_hour);
        if storage
            .receipt_digests
            .values()
            .any(|v| v.contains(&claim.query_digest))
        {
            return Err(PalletError::InvalidArgs);
        }
        let sender_count = storage
            .receipts_by_sender
            .get(&(block_hour, sender_key))
            .copied()
            .unwrap_or(0);
        if sender_count >= MAX_RECEIPTS_PER_SENDER_PER_HOUR {
            return Err(PalletError::InvalidArgs);
        }
        let pair_key = (block_hour, sender_key, responder_key);
        let pair_count = storage
            .receipts_by_pair
            .get(&pair_key)
            .copied()
            .unwrap_or(0);
        if pair_count >= MAX_RECEIPTS_PER_PAIR_PER_HOUR {
            return Err(PalletError::InvalidArgs);
        }

        storage
            .receipt_digests
            .entry(block_hour)
            .or_default()
            .push(claim.query_digest);
        storage
            .receipts_by_sender
            .insert((block_hour, sender_key), sender_count + 1);
        storage.receipts_by_pair.insert(pair_key, pair_count + 1);

        storage.note_first_seen(claim.responder_pid, ctx.timestamp_ms);
        storage.note_first_seen(claim.sender_pid, ctx.timestamp_ms);
        *storage.shares_consumed.entry(claim.sender_pid).or_insert(0) += 1;
        storage.record_lottery_win(claim.responder_pid, claim.dimension());
        Ok(())
    }
}

impl Pallet for ContributionsPallet {
    type Storage = ContributionsStorage;
    type Call = ContributionsCall;
    const PALLET_IDX: u8 = 2;

    fn apply(
        storage: &mut Self::Storage,
        call: Self::Call,
        ctx: &mut ApplyCtx<'_>,
    ) -> Result<(), PalletError> {
        match call {
            ContributionsCall::RecordShare { peer } => {
                storage.note_first_seen(peer, ctx.timestamp_ms);
                *storage.share_counts.entry(peer).or_insert(0) += 1;
                Ok(())
            }
            ContributionsCall::RecordConsumption { peer } => {
                storage.note_first_seen(peer, ctx.timestamp_ms);
                *storage.shares_consumed.entry(peer).or_insert(0) += 1;
                Ok(())
            }
            ContributionsCall::Redeem { .. }
            | ContributionsCall::SubmitReceipt { .. }
            | ContributionsCall::SubmitReceiptWithCustody { .. } => Err(PalletError::InvalidArgs),
        }
    }

    fn pallet_root(storage: &Self::Storage) -> [u8; 32] {
        let mut h = Sha256::new();
        const SEP: &[u8] = b"||";
        let mut first_seen: Vec<(&PeerId, &u64)> = storage.first_seen_ms.iter().collect();
        first_seen.sort_by_key(|(k, _)| **k);
        for (peer, ms) in first_seen {
            h.update(peer);
            h.update(ms.to_le_bytes());
        }
        h.update(SEP);

        let mut shares: Vec<(&PeerId, &u64)> = storage.share_counts.iter().collect();
        shares.sort_by_key(|(k, _)| **k);
        for (peer, n) in shares {
            h.update(peer);
            h.update(n.to_le_bytes());
        }
        h.update(SEP);

        let mut consumed: Vec<(&PeerId, &u64)> = storage.shares_consumed.iter().collect();
        consumed.sort_by_key(|(k, _)| **k);
        for (peer, n) in consumed {
            h.update(peer);
            h.update(n.to_le_bytes());
        }
        h.update(SEP);

        let mut wins: Vec<(&PeerId, &u64)> = storage.wins_by_peer.iter().collect();
        wins.sort_by_key(|(k, _)| **k);
        for (peer, n) in wins {
            h.update(peer);
            h.update(n.to_le_bytes());
        }
        h.update(SEP);

        let mut dims: Vec<(&PeerId, &u64)> = storage.lottery_dimension_by_peer.iter().collect();
        dims.sort_by_key(|(k, _)| **k);
        for (peer, n) in dims {
            h.update(peer);
            h.update(n.to_le_bytes());
        }
        h.update(SEP);

        let mut redeemed: Vec<(&PeerId, &RedeemedCounts)> = storage.redeemed.iter().collect();
        redeemed.sort_by_key(|(k, _)| **k);
        for (peer, r) in redeemed {
            h.update(peer);
            h.update(r.shares.to_le_bytes());
            h.update(r.lottery_dimension.to_le_bytes());
        }
        h.update(SEP);

        let mut digests: Vec<(&u64, &Vec<[u8; 32]>)> = storage.receipt_digests.iter().collect();
        digests.sort_by_key(|(k, _)| **k);
        for (hour, list) in digests {
            h.update(hour.to_le_bytes());
            let mut sorted = list.clone();
            sorted.sort();
            for d in sorted {
                h.update(d);
            }
        }
        h.update(SEP);

        let mut by_sender: Vec<(&(u64, PeerId), &u32)> =
            storage.receipts_by_sender.iter().collect();
        by_sender.sort_by_key(|(k, _)| **k);
        for ((hour, peer), n) in by_sender {
            h.update(hour.to_le_bytes());
            h.update(peer);
            h.update(n.to_le_bytes());
        }
        h.update(SEP);

        let mut by_pair: Vec<(&(u64, PeerId, PeerId), &u32)> =
            storage.receipts_by_pair.iter().collect();
        by_pair.sort_by_key(|(k, _)| **k);
        for ((hour, a, b), n) in by_pair {
            h.update(hour.to_le_bytes());
            h.update(a);
            h.update(b);
            h.update(n.to_le_bytes());
        }

        // Joins the root with the first fragment credited, so blocks sealed
        // before custody earned keep the roots they were sealed with.
        if !storage.custody_by_peer.is_empty() {
            h.update(SEP);
            let mut custody: Vec<(&PeerId, &u64)> = storage.custody_by_peer.iter().collect();
            custody.sort_by_key(|(k, _)| **k);
            for (peer, n) in custody {
                h.update(peer);
                h.update(n.to_le_bytes());
            }
        }

        h.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pallets::system::SystemStorage;

    fn ctx_for<'a>(
        signer: &'a PeerId,
        author: &'a PeerId,
        sys: &'a mut SystemStorage,
    ) -> ApplyCtx<'a> {
        ApplyCtx {
            block_height: 1,
            timestamp_ms: 0,
            author,
            signer,
            system: sys,
            current_shares_per_credit: crate::pallets::economy::DEFAULT_SHARES_PER_CREDIT,
            identity: &crate::pallets::NO_IDENTITY,
            rules_version: crate::pallets::upgrade::GENESIS_RULES_VERSION,
        }
    }

    #[test]
    fn maturity_of_weights_dimension_above_shares() {
        let mut storage = ContributionsStorage::default();
        let peer: PeerId = [7u8; 32];
        let unknown: PeerId = [9u8; 32];
        assert_eq!(storage.maturity_of(&unknown), 0);

        storage.share_counts.insert(peer, 12);
        assert_eq!(storage.maturity_of(&peer), 12);

        storage.lottery_dimension_by_peer.insert(peer, 200);
        assert_eq!(
            storage.maturity_of(&peer),
            12 + 200 * MATURITY_DIM_WEIGHT,
            "lottery_dimension dominates over raw shares"
        );
    }

    #[test]
    fn sybil_cannot_farm_maturity_via_shares_alone() {
        let mut sybil_storage = ContributionsStorage::default();
        let sybil: PeerId = [0xAAu8; 32];
        sybil_storage.share_counts.insert(sybil, 10_000);
        let sybil_maturity = sybil_storage.maturity_of(&sybil);

        let mut honest_storage = ContributionsStorage::default();
        let honest: PeerId = [0xBBu8; 32];
        honest_storage
            .lottery_dimension_by_peer
            .insert(honest, 3_825);
        let honest_maturity = honest_storage.maturity_of(&honest);

        assert!(
            honest_maturity > sybil_maturity,
            "one honest Qwen3-72B inference (dim=3,825) must outweigh \
             10,000 farmed shares from custody-only Sybil cluster: \
             honest={honest_maturity}, sybil={sybil_maturity}"
        );

        let threshold = honest_maturity;
        assert!(
            sybil_maturity < threshold,
            "Sybil with 10k custody-only shares must NOT pass \
             min_responder_maturity threshold set at the cost of \
             one real inference"
        );

        let shares_needed_to_match = honest_maturity;
        assert_eq!(
            shares_needed_to_match,
            3_825 * MATURITY_DIM_WEIGHT,
            "matching one 72B inference via shares alone requires \
             {} custody events — economically irrational vs running \
             the model once",
            shares_needed_to_match
        );
    }

    #[test]
    fn record_share_bumps_counter() {
        let mut storage = ContributionsStorage::default();
        let peer: PeerId = [1u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&peer, &peer, &mut sys);

        ContributionsPallet::apply(
            &mut storage,
            ContributionsCall::RecordShare { peer },
            &mut ctx,
        )
        .unwrap();
        ContributionsPallet::apply(
            &mut storage,
            ContributionsCall::RecordShare { peer },
            &mut ctx,
        )
        .unwrap();
        assert_eq!(storage.shares_of(&peer), 2);
    }

    #[test]
    fn lottery_eligibility_grace_period_for_new_peer() {
        let storage = ContributionsStorage::default();
        let peer: PeerId = [1u8; 32];
        assert!(storage.is_lottery_eligible(&peer));
    }

    #[test]
    fn lottery_eligibility_grace_period_threshold() {
        let mut storage = ContributionsStorage::default();
        let peer: PeerId = [1u8; 32];

        storage
            .shares_consumed
            .insert(peer, LEECHER_GRACE_THRESHOLD - 1);
        assert!(
            storage.is_lottery_eligible(&peer),
            "below grace threshold, no custody needed"
        );

        storage
            .shares_consumed
            .insert(peer, LEECHER_GRACE_THRESHOLD);
        assert!(
            !storage.is_lottery_eligible(&peer),
            "at grace boundary with zero custody, must be flagged"
        );
    }

    #[test]
    fn lottery_eligibility_healthy_contributor() {
        let mut storage = ContributionsStorage::default();
        let peer: PeerId = [1u8; 32];
        storage.shares_consumed.insert(peer, 100);
        storage.share_counts.insert(peer, 60);
        assert!(storage.is_lottery_eligible(&peer));
    }

    #[test]
    fn lottery_eligibility_at_threshold() {
        let mut storage = ContributionsStorage::default();
        let peer: PeerId = [1u8; 32];
        storage.shares_consumed.insert(peer, 100);
        storage.share_counts.insert(peer, 50);
        assert!(storage.is_lottery_eligible(&peer));
    }

    #[test]
    fn lottery_eligibility_leecher_flagged() {
        let mut storage = ContributionsStorage::default();
        let peer: PeerId = [1u8; 32];
        storage.shares_consumed.insert(peer, 100);
        storage.share_counts.insert(peer, 49);
        assert!(!storage.is_lottery_eligible(&peer));
    }

    /// The four tests above seed `share_counts` by hand. Nothing in the node
    /// writes that counter - only the founder-only `RecordShare` does - so
    /// they exercise a path production never takes. These two go through the
    /// counters a service receipt actually moves.
    #[test]
    fn answering_keeps_a_node_eligible_once_the_grace_window_closes() {
        let mut storage = ContributionsStorage::default();
        let peer: PeerId = [1u8; 32];
        let now = 30 * 24 * 60 * 60 * 1_000u64;
        let long_ago = now - (LEECHER_GRACE_WINDOW_MS + 1);

        // Ten answers given, ten questions asked - exactly what ten receipts
        // in each direction record. No RecordShare anywhere.
        storage.first_seen_ms.insert(peer, long_ago);
        storage.shares_consumed.insert(peer, 10);
        for _ in 0..10 {
            storage.record_lottery_win(peer, 200);
        }

        assert_eq!(
            storage.shares_of(&peer),
            0,
            "no founder ever attested custody"
        );
        assert!(
            storage.is_lottery_eligible_at(&peer, Some(now)),
            "a node that answers as much as it asks must stay eligible"
        );
    }

    #[test]
    fn a_pure_consumer_is_still_barred_once_the_grace_window_closes() {
        let mut storage = ContributionsStorage::default();
        let peer: PeerId = [2u8; 32];
        let now = 30 * 24 * 60 * 60 * 1_000u64;
        storage
            .first_seen_ms
            .insert(peer, now - (LEECHER_GRACE_WINDOW_MS + 1));
        storage.shares_consumed.insert(peer, 10);

        assert!(
            !storage.is_lottery_eligible_at(&peer, Some(now)),
            "asking without ever answering is what the leecher rule is for"
        );
    }

    #[test]
    fn lottery_eligibility_recovery_path() {
        let mut storage = ContributionsStorage::default();
        let peer: PeerId = [1u8; 32];
        storage.shares_consumed.insert(peer, 100);
        storage.share_counts.insert(peer, 10);
        assert!(!storage.is_lottery_eligible(&peer));

        storage.share_counts.insert(peer, 50);
        assert!(storage.is_lottery_eligible(&peer));
    }

    #[test]
    fn record_lottery_win_bumps_wins() {
        let mut storage = ContributionsStorage::default();
        let winner: PeerId = [9u8; 32];
        storage.record_lottery_win(winner, 200);
        storage.record_lottery_win(winner, 200);
        storage.record_lottery_win(winner, 200);
        assert_eq!(storage.wins_of(&winner), 3);
        assert_eq!(storage.redeemable_lottery_dimension(&winner), 600);
    }

    #[test]
    fn redeemable_counts_lottery_wins_not_zero() {
        let mut storage = ContributionsStorage::default();
        let peer: PeerId = [1u8; 32];
        for _ in 0..250 {
            *storage.share_counts.entry(peer).or_insert(0) += 1;
        }
        storage.record_lottery_win(peer, 200);
        storage.record_lottery_win(peer, 200);
        storage.record_lottery_win(peer, 200);

        let (shares_avail, dim_avail) = storage.redeemable(&peer);
        assert_eq!(shares_avail, 250);
        assert_eq!(
            dim_avail, 600,
            "lottery wins must be redeemable as accumulated dimension"
        );
    }

    #[test]
    fn redeem_mints_credits_and_bumps_redeemed_counters() {
        let mut storage = ContributionsStorage::default();
        let mut balances = BalancesStorage::default();
        let peer: PeerId = [1u8; 32];

        for _ in 0..1000 {
            *storage.share_counts.entry(peer).or_insert(0) += 1;
        }
        storage.record_lottery_win(peer, 200);
        storage.record_lottery_win(peer, 200);

        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&peer, &peer, &mut sys);

        ContributionsPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            ContributionsCall::Redeem {
                receiver: peer,
                shares: 600,
                lottery_dimension: 400,
            },
            &mut ctx,
        )
        .unwrap();

        assert_eq!(balances.balance_of(&peer), 1);
        assert_eq!(storage.redeemed_of(&peer).shares, 600);
        assert_eq!(storage.redeemed_of(&peer).lottery_dimension, 400);
        assert_eq!(storage.redeemable(&peer), (400, 0));
    }

    #[test]
    fn redeem_dimension_combines_with_shares_at_1000_threshold() {
        let mut storage = ContributionsStorage::default();
        let mut balances = BalancesStorage::default();
        let peer: PeerId = [1u8; 32];

        for _ in 0..900 {
            *storage.share_counts.entry(peer).or_insert(0) += 1;
        }
        storage.record_lottery_win(peer, 100);

        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&peer, &peer, &mut sys);

        ContributionsPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            ContributionsCall::Redeem {
                receiver: peer,
                shares: 900,
                lottery_dimension: 100,
            },
            &mut ctx,
        )
        .unwrap();

        assert_eq!(balances.balance_of(&peer), 1);
    }

    #[test]
    fn redeem_zero_shares_small_dimension_alone_is_subcredit() {
        let mut storage = ContributionsStorage::default();
        let mut balances = BalancesStorage::default();
        let peer: PeerId = [1u8; 32];
        storage.record_lottery_win(peer, 200);
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&peer, &peer, &mut sys);

        let result = ContributionsPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            ContributionsCall::Redeem {
                receiver: peer,
                shares: 0,
                lottery_dimension: 200,
            },
            &mut ctx,
        );
        assert_eq!(result, Err(PalletError::InvalidArgs));
        assert_eq!(balances.balance_of(&peer), 0);
        assert_eq!(storage.redeemed_of(&peer), RedeemedCounts::default());
    }

    #[test]
    fn redeem_more_than_available_rejected() {
        let mut storage = ContributionsStorage::default();
        let mut balances = BalancesStorage::default();
        let peer: PeerId = [1u8; 32];
        for _ in 0..50 {
            *storage.share_counts.entry(peer).or_insert(0) += 1;
        }
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&peer, &peer, &mut sys);

        let result = ContributionsPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            ContributionsCall::Redeem {
                receiver: peer,
                shares: 100,
                lottery_dimension: 0,
            },
            &mut ctx,
        );
        assert_eq!(result, Err(PalletError::InvalidArgs));
        assert_eq!(balances.balance_of(&peer), 0);
        assert_eq!(storage.redeemed_of(&peer), RedeemedCounts::default());
    }

    #[test]
    fn redeem_zero_credits_rejected() {
        let mut storage = ContributionsStorage::default();
        let mut balances = BalancesStorage::default();
        let peer: PeerId = [1u8; 32];
        for _ in 0..500 {
            *storage.share_counts.entry(peer).or_insert(0) += 1;
        }
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&peer, &peer, &mut sys);

        let result = ContributionsPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            ContributionsCall::Redeem {
                receiver: peer,
                shares: 500,
                lottery_dimension: 0,
            },
            &mut ctx,
        );
        assert_eq!(result, Err(PalletError::InvalidArgs));
    }

    #[test]
    fn redeem_with_signer_ne_receiver_rejected_unauthorized() {
        let mut storage = ContributionsStorage::default();
        let mut balances = BalancesStorage::default();
        let victim: PeerId = [1u8; 32];
        let attacker: PeerId = [2u8; 32];
        for _ in 0..300 {
            *storage.share_counts.entry(victim).or_insert(0) += 1;
        }
        storage.record_lottery_win(victim, 200);

        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&attacker, &attacker, &mut sys);

        let result = ContributionsPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            ContributionsCall::Redeem {
                receiver: victim,
                shares: 200,
                lottery_dimension: 200,
            },
            &mut ctx,
        );
        assert_eq!(result, Err(PalletError::Unauthorized));
        assert_eq!(balances.balance_of(&victim), 0);
        assert_eq!(balances.balance_of(&attacker), 0);
        assert_eq!(storage.redeemed_of(&victim), RedeemedCounts::default());
        assert_eq!(storage.redeemable(&victim), (300, 200));
    }

    #[test]
    fn redeem_via_plain_apply_refused() {
        let mut storage = ContributionsStorage::default();
        let peer: PeerId = [1u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&peer, &peer, &mut sys);

        let result = ContributionsPallet::apply(
            &mut storage,
            ContributionsCall::Redeem {
                receiver: peer,
                shares: 100,
                lottery_dimension: 0,
            },
            &mut ctx,
        );
        assert_eq!(result, Err(PalletError::InvalidArgs));
    }

    #[test]
    fn a_redeem_converts_whole_credits_and_leaves_the_rest_redeemable() {
        // A testnet node redeeming automatically: 917 earned at
        // 100 per credit, then 458 more from a Top-Tier session.
        let mut storage = ContributionsStorage::default();
        let mut balances = BalancesStorage::default();
        let peer: PeerId = [1u8; 32];
        storage.record_lottery_win(peer, 917);
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&peer, &peer, &mut sys);
        ctx.current_shares_per_credit = 100;
        let redeem = |dim| ContributionsCall::Redeem {
            receiver: peer,
            shares: 0,
            lottery_dimension: dim,
        };

        let minted = ContributionsPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            redeem(917),
            &mut ctx,
        )
        .unwrap();
        assert_eq!(minted, 9);
        assert_eq!(storage.redeemable(&peer), (0, 17));

        // What is left is under a credit until more is earned.
        assert_eq!(
            ContributionsPallet::apply_with_balances(
                &mut storage,
                &mut balances,
                redeem(17),
                &mut ctx
            ),
            Err(PalletError::InvalidArgs)
        );

        storage.record_lottery_win(peer, 458);
        let minted = ContributionsPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            redeem(475),
            &mut ctx,
        )
        .unwrap();
        assert_eq!(minted, 4);
        assert_eq!(balances.balance_of(&peer), 13);
        assert_eq!(storage.redeemable(&peer), (0, 75));
    }
}
