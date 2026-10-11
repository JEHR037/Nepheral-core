// SPDX-License-Identifier: BUSL-1.1
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{ApplyCtx, Pallet, PalletError};

pub const MIN_SHARES_PER_CREDIT: u64 = 100;

pub const MAX_SHARES_PER_CREDIT: u64 = 1_000_000;

/// How much recent history emission is steered by, in wall-clock time.
///
/// This used to be 1,800 blocks, on the assumption of one block per
/// two-second slot. The producer skips slots with nothing to seal, so a block
/// stands for anything from two seconds to a minute: on the testnet 1,800
/// blocks covered 3.4 hours under load and about 15 when quiet. The controller
/// still priced them as one hour, over-counted the work in them by that
/// factor, and minted 3-15 times less than its target - less still the
/// quieter the network. The window is now read from block timestamps, which
/// the header binds to the slot, so it means the same thing at any block rate.
pub const ADJUSTMENT_WINDOW_MS: u64 = 60 * 60 * 1_000;

/// How often the ratio is recomputed.
pub const ADJUSTMENT_PERIOD_MS: u64 = 10 * 60 * 1_000;

/// Below this much history the ratio is left alone: a young chain has too
/// little of it to steer emission by, and a first recompute over a few
/// seconds would swing the ratio to one of its clamps.
pub const MIN_ADJUSTMENT_SPAN_MS: u64 = 10 * 60 * 1_000;

pub const DEFAULT_TARGET_CREDITS_PER_HOUR: u64 = 1_000;

pub const DEFAULT_SHARES_PER_CREDIT: u64 = 1_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EconomyStorage {
    pub current_shares_per_credit: u64,
    pub target_credits_per_hour: u64,
    /// `(block timestamp_ms, share-equivalents)` for every block inside the
    /// window, oldest first.
    pub recent_activity: VecDeque<(u64, u64)>,
    pub last_adjustment_ms: u64,
}

impl Default for EconomyStorage {
    fn default() -> Self {
        Self {
            current_shares_per_credit: DEFAULT_SHARES_PER_CREDIT,
            target_credits_per_hour: DEFAULT_TARGET_CREDITS_PER_HOUR,
            recent_activity: VecDeque::new(),
            last_adjustment_ms: 0,
        }
    }
}

impl EconomyStorage {
    pub fn record_block_activity(&mut self, timestamp_ms: u64, block_share_equivs: u64) {
        self.recent_activity
            .push_back((timestamp_ms, block_share_equivs));
        let floor = timestamp_ms.saturating_sub(ADJUSTMENT_WINDOW_MS);
        while self
            .recent_activity
            .front()
            .is_some_and(|(at, _)| *at < floor)
        {
            self.recent_activity.pop_front();
        }
    }

    pub fn adjustment_due(&self, now_ms: u64) -> bool {
        now_ms >= self.last_adjustment_ms.saturating_add(ADJUSTMENT_PERIOD_MS)
    }

    pub fn recompute_ratio(&mut self, now_ms: u64) {
        let Some(&(oldest_ms, _)) = self.recent_activity.front() else {
            return;
        };
        let span_ms = now_ms.saturating_sub(oldest_ms).min(ADJUSTMENT_WINDOW_MS);
        if span_ms < MIN_ADJUSTMENT_SPAN_MS {
            return;
        }
        let total_equivs: u128 = self.recent_activity.iter().map(|(_, e)| *e as u128).sum();
        let target_per_window =
            (self.target_credits_per_hour as u128).saturating_mul(span_ms as u128) / 3_600_000;
        if target_per_window == 0 {
            return;
        }
        let new_ratio = (total_equivs / target_per_window).min(u64::MAX as u128) as u64;
        self.current_shares_per_credit =
            new_ratio.clamp(MIN_SHARES_PER_CREDIT, MAX_SHARES_PER_CREDIT);
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum EconomyCall {
    SetEmissionTarget { target_credits_per_hour: u64 },
}

pub struct EconomyPallet;

impl Pallet for EconomyPallet {
    type Storage = EconomyStorage;
    type Call = EconomyCall;
    const PALLET_IDX: u8 = 6;

    fn apply(
        storage: &mut Self::Storage,
        call: Self::Call,
        _ctx: &mut ApplyCtx<'_>,
    ) -> Result<(), PalletError> {
        match call {
            EconomyCall::SetEmissionTarget {
                target_credits_per_hour,
            } => {
                if target_credits_per_hour == 0 {
                    return Err(PalletError::InvalidArgs);
                }
                storage.target_credits_per_hour = target_credits_per_hour;
                Ok(())
            }
        }
    }

    fn pallet_root(storage: &Self::Storage) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"nepheral/economy/v2");
        h.update(storage.current_shares_per_credit.to_le_bytes());
        h.update(storage.target_credits_per_hour.to_le_bytes());
        h.update(storage.last_adjustment_ms.to_le_bytes());
        h.update((storage.recent_activity.len() as u32).to_le_bytes());
        for (at, equivs) in &storage.recent_activity {
            h.update(at.to_le_bytes());
            h.update(equivs.to_le_bytes());
        }
        h.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 60 * 60 * 1_000;
    const T0: u64 = 1_790_000_000_000;

    /// One hour of blocks every `every_ms`, each carrying `per_block`.
    fn hour_of(every_ms: u64, per_block: u64) -> EconomyStorage {
        let mut s = EconomyStorage::default();
        let mut t = T0;
        while t <= T0 + HOUR {
            s.record_block_activity(t, per_block);
            t += every_ms;
        }
        s
    }

    #[test]
    fn default_ratio_matches_legacy_constant() {
        let s = EconomyStorage::default();
        assert_eq!(s.current_shares_per_credit, DEFAULT_SHARES_PER_CREDIT);
        assert_eq!(s.target_credits_per_hour, DEFAULT_TARGET_CREDITS_PER_HOUR);
        assert!(s.recent_activity.is_empty());
    }

    #[test]
    fn the_window_is_an_hour_of_wall_clock_however_many_blocks_that_is() {
        let mut s = EconomyStorage::default();
        for i in 0..10_000u64 {
            s.record_block_activity(T0 + i * 2_000, 1);
        }
        let (oldest, _) = *s.recent_activity.front().unwrap();
        let (newest, _) = *s.recent_activity.back().unwrap();
        assert!(newest - oldest <= ADJUSTMENT_WINDOW_MS);
        assert_eq!(s.recent_activity.len(), 1_801);
    }

    #[test]
    fn target_throughput_lands_near_the_default_ratio() {
        // 1,000 credits/hour at 1,000 shares per credit is 1,000,000
        // share-equivalents an hour: 556 per two-second block.
        let mut s = hour_of(2_000, 556);
        s.recompute_ratio(T0 + HOUR);
        assert!(
            s.current_shares_per_credit
                .abs_diff(DEFAULT_SHARES_PER_CREDIT)
                <= 5,
            "got {}",
            s.current_shares_per_credit
        );
    }

    /// The regression this change exists for. The same hour of work lands in
    /// 1,801 blocks when every slot is sealed and in 61 when the producer
    /// only seals once a minute; the ratio must not care which.
    #[test]
    fn the_ratio_does_not_depend_on_how_many_blocks_carried_the_work() {
        let mut dense = hour_of(2_000, 556);
        let mut sparse = hour_of(60_000, 556 * 30);
        dense.recompute_ratio(T0 + HOUR);
        sparse.recompute_ratio(T0 + HOUR);
        let (d, p) = (
            dense.current_shares_per_credit,
            sparse.current_shares_per_credit,
        );
        assert!(
            d.abs_diff(p) * 100 <= d.max(p) * 2,
            "dense {d} vs sparse {p}"
        );
    }

    #[test]
    fn ten_times_the_work_scales_the_ratio_up() {
        let mut s = hour_of(2_000, 5_560);
        s.recompute_ratio(T0 + HOUR);
        assert!(s.current_shares_per_credit > DEFAULT_SHARES_PER_CREDIT * 8);
    }

    #[test]
    fn too_little_history_leaves_the_ratio_alone() {
        let mut s = EconomyStorage::default();
        s.record_block_activity(T0, 5_000_000);
        s.record_block_activity(T0 + 30_000, 5_000_000);
        s.recompute_ratio(T0 + 30_000);
        assert_eq!(s.current_shares_per_credit, DEFAULT_SHARES_PER_CREDIT);
    }

    #[test]
    fn recompute_ratio_clamps_to_min() {
        let mut s = hour_of(60_000, 1);
        s.recompute_ratio(T0 + HOUR);
        assert_eq!(s.current_shares_per_credit, MIN_SHARES_PER_CREDIT);
    }

    #[test]
    fn recompute_ratio_clamps_to_max() {
        let mut s = hour_of(2_000, u64::MAX / 4_000);
        s.recompute_ratio(T0 + HOUR);
        assert_eq!(s.current_shares_per_credit, MAX_SHARES_PER_CREDIT);
    }

    #[test]
    fn adjustments_are_spaced_by_wall_clock() {
        let mut s = EconomyStorage::default();
        assert!(s.adjustment_due(T0));
        s.last_adjustment_ms = T0;
        assert!(!s.adjustment_due(T0 + ADJUSTMENT_PERIOD_MS - 1));
        assert!(s.adjustment_due(T0 + ADJUSTMENT_PERIOD_MS));
    }

    #[test]
    fn set_emission_target_rejects_zero() {
        let mut s = EconomyStorage::default();
        let signer = [1u8; 32];
        let mut sys = crate::pallets::system::SystemStorage::default();
        let mut ctx = ApplyCtx {
            block_height: 1,
            timestamp_ms: 0,
            author: &signer,
            signer: &signer,
            system: &mut sys,
            current_shares_per_credit: DEFAULT_SHARES_PER_CREDIT,
            identity: &crate::pallets::NO_IDENTITY,
            rules_version: crate::pallets::upgrade::GENESIS_RULES_VERSION,
        };
        let result = EconomyPallet::apply(
            &mut s,
            EconomyCall::SetEmissionTarget {
                target_credits_per_hour: 0,
            },
            &mut ctx,
        );
        assert_eq!(result, Err(PalletError::InvalidArgs));
    }

    #[test]
    fn set_emission_target_updates_field() {
        let mut s = EconomyStorage::default();
        let signer = [1u8; 32];
        let mut sys = crate::pallets::system::SystemStorage::default();
        let mut ctx = ApplyCtx {
            block_height: 1,
            timestamp_ms: 0,
            author: &signer,
            signer: &signer,
            system: &mut sys,
            current_shares_per_credit: DEFAULT_SHARES_PER_CREDIT,
            identity: &crate::pallets::NO_IDENTITY,
            rules_version: crate::pallets::upgrade::GENESIS_RULES_VERSION,
        };
        EconomyPallet::apply(
            &mut s,
            EconomyCall::SetEmissionTarget {
                target_credits_per_hour: 5000,
            },
            &mut ctx,
        )
        .unwrap();
        assert_eq!(s.target_credits_per_hour, 5000);
    }

    #[test]
    fn pallet_root_changes_when_ratio_changes() {
        let mut s = EconomyStorage::default();
        let r0 = EconomyPallet::pallet_root(&s);
        s.current_shares_per_credit = 999;
        let r1 = EconomyPallet::pallet_root(&s);
        assert_ne!(r0, r1);
    }
}
