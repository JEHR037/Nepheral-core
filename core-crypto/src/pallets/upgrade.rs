// SPDX-License-Identifier: BUSL-1.1
//! Upgrade (pallet 11): changing the chain's rules without restarting it.
//!
//! The rules in force carry a version, starting at 1. The founder, or
//! governance once the founder has resigned, schedules the next version for
//! a wall-clock time at least `min_notice` away. At the first block whose
//! timestamp reaches it, every node switches to the new rules at once:
//! `ChainState::apply_block_checked` runs the migration and code that
//! changed behaviour asks `UpgradeStorage::active()`.
//!
//! A binary that does not implement the scheduled version stops at that
//! block with `ChainError::UpgradeRequired` instead of building on the old
//! rules, so an outdated node falls behind and says why; it never forks.
//!
//! These are versions of the chain's rules, not of the mesh protocol
//! (`mesh_network::protocol::PROTOCOL_VERSION`), which moves on its own.
//!
//! The pallet joins the state root only once something is scheduled, so the
//! history written before it existed reconciles to the same roots.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::PalletError;

/// The rules a chain starts with, before any upgrade.
pub const GENESIS_RULES_VERSION: u32 = 1;

/// The newest rules this build implements.
///
/// Version 2 (scheduled on chain once every node runs a build that has it):
/// - Self-dealing: no receipt between two nodes of the same
///   owner; the hourly receipt caps count per owner, not per node key;
///   custodians of the asker's or responder's own owner earn nothing, and
///   custodians with no history earn at most as many as those with one.
///   contributions.rs. (The declared model size and answer length were
///   already capped, at 72B and 8,192 tokens: receipts.rs.)
/// - The sale-health reason is kept while healthy too (UPGRADES §4.1).
///   nai.rs.
/// - Market asks refuse MWEB (private) payout addresses (UPGRADES §4.2).
///   market.rs.
pub const SUPPORTED_RULES_VERSION: u32 = 2;

/// The first rules version with the self-dealing bounds and the rest of
/// the list above.
pub const RULES_V2: u32 = 2;

#[cfg(test)]
thread_local! {
    /// Lets a test act as a build that implements newer rules. Per thread,
    /// so tests running in parallel do not see each other's.
    pub static SUPPORTED_OVERRIDE: std::cell::Cell<Option<u32>> =
        const { std::cell::Cell::new(None) };
}

/// The newest rules this build implements (overridable in tests).
pub fn supported_rules_version() -> u32 {
    #[cfg(test)]
    if let Some(v) = SUPPORTED_OVERRIDE.with(|c| c.get()) {
        return v;
    }
    SUPPORTED_RULES_VERSION
}

/// Oldest entries are dropped beyond this; the activation heights of recent
/// upgrades are what clients need to show.
pub const MAX_HISTORY: usize = 64;

/// The least time between scheduling an upgrade and its activation, so
/// every operator has the chance to update first. Wall-clock time, not
/// blocks: the producer skips empty slots, so a block count is anything from
/// two seconds to a minute per block.
pub fn min_notice_ms(network: crate::network::Network) -> u64 {
    match network {
        crate::network::Network::Testnet => 10 * 60 * 1_000,
        crate::network::Network::Mainnet => 7 * 24 * 60 * 60 * 1_000,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledUpgrade {
    pub version: u32,
    /// The rules switch at the first block whose timestamp is at or after
    /// this.
    pub activate_at_ms: u64,
    /// Hash of the release that implements `version` (a signed release
    /// manifest), so operators can check what they install.
    pub release: [u8; 32],
    pub scheduled_at_height: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradeStorage {
    /// The version whose rules are in force. 0 reads as
    /// `GENESIS_RULES_VERSION`, so a chain that never upgraded has default
    /// storage.
    pub active_version: u32,
    pub scheduled: Option<ScheduledUpgrade>,
    /// (version, height it activated at), oldest first.
    pub history: Vec<(u32, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpgradeCall {
    Schedule {
        version: u32,
        activate_at_ms: u64,
        release: [u8; 32],
    },
    Cancel,
}

/// What the block about to be applied has to do before its extrinsics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    None,
    /// Switch to `version` now.
    Activate {
        version: u32,
    },
    /// The scheduled version is due and this build does not implement it.
    Unsupported {
        version: u32,
    },
}

impl UpgradeStorage {
    pub fn is_default(&self) -> bool {
        self.active_version == 0 && self.scheduled.is_none() && self.history.is_empty()
    }

    /// The version whose rules are in force.
    pub fn active(&self) -> u32 {
        self.active_version.max(GENESIS_RULES_VERSION)
    }

    /// Whether a scheduled upgrade needs a newer build than this one.
    pub fn scheduled_unsupported(&self) -> Option<&ScheduledUpgrade> {
        self.scheduled
            .as_ref()
            .filter(|s| s.version > supported_rules_version())
    }

    pub fn activation_for(&self, timestamp_ms: u64) -> Activation {
        Self::activation_with(
            self.scheduled.as_ref(),
            timestamp_ms,
            supported_rules_version(),
        )
    }

    fn activation_with(
        scheduled: Option<&ScheduledUpgrade>,
        timestamp_ms: u64,
        supported: u32,
    ) -> Activation {
        match scheduled {
            Some(s) if timestamp_ms >= s.activate_at_ms => {
                if s.version > supported {
                    Activation::Unsupported { version: s.version }
                } else {
                    Activation::Activate { version: s.version }
                }
            }
            _ => Activation::None,
        }
    }

    /// Makes `version` the rules in force from `height`.
    pub fn activate(&mut self, version: u32, height: u64) {
        self.active_version = version;
        self.scheduled = None;
        self.history.push((version, height));
        if self.history.len() > MAX_HISTORY {
            let excess = self.history.len() - MAX_HISTORY;
            self.history.drain(..excess);
        }
    }

    /// Schedules `version`. The caller has checked who may.
    pub fn schedule(
        &mut self,
        version: u32,
        activate_at_ms: u64,
        release: [u8; 32],
        now_ms: u64,
        height: u64,
        min_notice_ms: u64,
    ) -> Result<(), PalletError> {
        if version <= self.active() {
            return Err(PalletError::InvalidArgs);
        }
        // A newer schedule replaces an older one; an equal or older one
        // would silently undo it.
        if self
            .scheduled
            .as_ref()
            .is_some_and(|s| version <= s.version)
        {
            return Err(PalletError::InvalidArgs);
        }
        if activate_at_ms < now_ms.saturating_add(min_notice_ms) {
            return Err(PalletError::InvalidArgs);
        }
        self.scheduled = Some(ScheduledUpgrade {
            version,
            activate_at_ms,
            release,
            scheduled_at_height: height,
        });
        Ok(())
    }

    /// Withdraws the scheduled upgrade. Only before it activates; once it
    /// has, it is the rules in force and there is nothing to cancel.
    pub fn cancel(&mut self) -> Result<(), PalletError> {
        if self.scheduled.take().is_none() {
            return Err(PalletError::InvalidArgs);
        }
        Ok(())
    }

    pub fn pallet_root(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"nepheral/upgrade/v1");
        h.update(crate::wire::encode(self));
        h.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTICE: u64 = 600_000;

    fn scheduled(version: u32, at: u64) -> UpgradeStorage {
        let mut s = UpgradeStorage::default();
        s.schedule(version, at, [7u8; 32], 0, 10, NOTICE).unwrap();
        s
    }

    #[test]
    fn a_chain_that_never_upgraded_runs_genesis_rules() {
        let s = UpgradeStorage::default();
        assert!(s.is_default());
        assert_eq!(s.active(), GENESIS_RULES_VERSION);
    }

    #[test]
    fn scheduling_needs_a_newer_version_and_enough_notice() {
        let mut s = UpgradeStorage::default();
        assert_eq!(
            s.schedule(1, NOTICE, [0u8; 32], 0, 1, NOTICE),
            Err(PalletError::InvalidArgs),
            "the version in force"
        );
        assert_eq!(
            s.schedule(2, NOTICE - 1, [0u8; 32], 0, 1, NOTICE),
            Err(PalletError::InvalidArgs),
            "too little notice"
        );
        assert!(s.schedule(2, NOTICE, [0u8; 32], 0, 1, NOTICE).is_ok());
        assert_eq!(
            s.schedule(2, 2 * NOTICE, [0u8; 32], 0, 1, NOTICE),
            Err(PalletError::InvalidArgs),
            "the same version again"
        );
        assert!(
            s.schedule(3, 2 * NOTICE, [0u8; 32], 0, 1, NOTICE).is_ok(),
            "a newer one replaces it"
        );
        assert_eq!(s.scheduled.as_ref().unwrap().version, 3);
    }

    #[test]
    fn activation_waits_for_the_scheduled_time() {
        let s = scheduled(2, 1_000_000);
        let pending = s.scheduled.as_ref();
        assert_eq!(
            UpgradeStorage::activation_with(pending, 999_999, 2),
            Activation::None
        );
        assert_eq!(
            UpgradeStorage::activation_with(pending, 1_000_000, 2),
            Activation::Activate { version: 2 }
        );
        assert_eq!(
            UpgradeStorage::activation_with(pending, 1_000_000, 1),
            Activation::Unsupported { version: 2 }
        );
    }

    #[test]
    fn activating_records_the_height_and_clears_the_schedule() {
        let mut s = scheduled(2, 1_000_000);
        s.activate(2, 500);
        assert_eq!(s.active(), 2);
        assert!(s.scheduled.is_none());
        assert_eq!(s.history, vec![(2, 500)]);
        assert_eq!(
            s.cancel(),
            Err(PalletError::InvalidArgs),
            "nothing left to cancel"
        );
    }

    #[test]
    fn cancelling_returns_to_default_storage() {
        let mut s = scheduled(2, 1_000_000);
        assert!(!s.is_default());
        s.cancel().unwrap();
        assert!(s.is_default());
    }
}
