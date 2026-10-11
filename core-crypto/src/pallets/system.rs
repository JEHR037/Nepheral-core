// SPDX-License-Identifier: BUSL-1.1
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{ApplyCtx, Pallet, PalletError};
use crate::identity::PeerId;

pub const MAX_NONCE_GAP: u64 = 64;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SystemStorage {
    pub nonces: HashMap<PeerId, u64>,
    pub block_height: u64,
    pub finalized_height: u64,
    pub current_slot: u64,
    #[serde(default)]
    pub finalized_hash: [u8; 32],
}

impl SystemStorage {
    pub fn verify_and_bump_nonce(
        &mut self,
        signer: &PeerId,
        nonce: u64,
    ) -> Result<(), PalletError> {
        let last = self.nonces.get(signer).copied().unwrap_or(0);
        if nonce <= last {
            return Err(PalletError::NonceReplay);
        }
        if nonce - last > MAX_NONCE_GAP {
            return Err(PalletError::InvalidArgs);
        }
        self.nonces.insert(*signer, nonce);
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum SystemCall {}

pub struct SystemPallet;

impl Pallet for SystemPallet {
    type Storage = SystemStorage;
    type Call = SystemCall;
    const PALLET_IDX: u8 = 0;

    fn apply(
        _storage: &mut Self::Storage,
        call: Self::Call,
        _ctx: &mut ApplyCtx<'_>,
    ) -> Result<(), PalletError> {
        match call {}
    }

    fn pallet_root(storage: &Self::Storage) -> [u8; 32] {
        let mut h = Sha256::new();
        const SEP: &[u8] = b"||";

        let mut nonces: Vec<(&PeerId, &u64)> = storage.nonces.iter().collect();
        nonces.sort_by_key(|(k, _)| **k);
        for (peer, n) in nonces {
            h.update(peer);
            h.update(n.to_le_bytes());
        }
        h.update(SEP);

        h.update(storage.block_height.to_le_bytes());
        h.update(storage.finalized_height.to_le_bytes());
        h.update(storage.current_slot.to_le_bytes());
        h.update(storage.finalized_hash);

        h.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_bump_accepts_strictly_increasing() {
        let mut s = SystemStorage::default();
        let signer: PeerId = [1u8; 32];
        s.verify_and_bump_nonce(&signer, 1).unwrap();
        s.verify_and_bump_nonce(&signer, 2).unwrap();
        s.verify_and_bump_nonce(&signer, 5).unwrap();
        assert_eq!(s.nonces[&signer], 5);
    }

    #[test]
    fn nonce_replay_rejected() {
        let mut s = SystemStorage::default();
        let signer: PeerId = [1u8; 32];
        s.verify_and_bump_nonce(&signer, 3).unwrap();
        assert_eq!(
            s.verify_and_bump_nonce(&signer, 3),
            Err(PalletError::NonceReplay),
            "same nonce twice must replay-error"
        );
        assert_eq!(
            s.verify_and_bump_nonce(&signer, 2),
            Err(PalletError::NonceReplay),
            "older nonce must replay-error"
        );
    }

    #[test]
    fn pallet_root_changes_on_height_advance() {
        let mut s = SystemStorage::default();
        let r0 = SystemPallet::pallet_root(&s);
        s.block_height = 1;
        let r1 = SystemPallet::pallet_root(&s);
        assert_ne!(r0, r1);
    }

    #[test]
    fn pallet_root_deterministic_under_nonce_reorder() {
        let mut a = SystemStorage::default();
        let mut b = SystemStorage::default();
        a.nonces.insert([1u8; 32], 5);
        a.nonces.insert([2u8; 32], 7);
        b.nonces.insert([2u8; 32], 7);
        b.nonces.insert([1u8; 32], 5);
        assert_eq!(SystemPallet::pallet_root(&a), SystemPallet::pallet_root(&b));
    }

    #[test]
    fn a_nonce_may_skip_ahead_up_to_the_gap() {
        // Skipping is allowed: a node whose earlier transaction was dropped
        // must not be stuck behind it forever.
        let mut s = SystemStorage::default();
        let signer: PeerId = [1u8; 32];
        s.verify_and_bump_nonce(&signer, 1).unwrap();
        s.verify_and_bump_nonce(&signer, 1 + MAX_NONCE_GAP).unwrap();
        assert_eq!(s.nonces[&signer], 1 + MAX_NONCE_GAP);
    }

    #[test]
    fn a_nonce_beyond_the_gap_is_refused_and_changes_nothing() {
        // Wall-clock milliseconds as nonces - what the sale satellite once
        // signed with - land here, which is why none of its extrinsics ever
        // reached the chain.
        let mut s = SystemStorage::default();
        let signer: PeerId = [1u8; 32];
        s.verify_and_bump_nonce(&signer, 4).unwrap();
        assert_eq!(
            s.verify_and_bump_nonce(&signer, 4 + MAX_NONCE_GAP + 1),
            Err(PalletError::InvalidArgs)
        );
        assert_eq!(
            s.verify_and_bump_nonce(&signer, 1_700_000_000_000),
            Err(PalletError::InvalidArgs)
        );
        assert_eq!(s.nonces[&signer], 4);
    }
}
