// SPDX-License-Identifier: BUSL-1.1
pub mod authority;
pub mod balances;
pub mod contributions;
pub mod economy;
pub mod governance;
pub mod identity;
pub mod market;
pub mod nai;
pub mod reputation;
pub mod system;
pub mod upgrade;

use serde::{de::DeserializeOwned, Serialize};

use crate::identity::PeerId;
use system::SystemStorage;

pub struct ApplyCtx<'a> {
    pub block_height: u64,
    pub timestamp_ms: u64,
    pub author: &'a PeerId,
    pub signer: &'a PeerId,
    pub system: &'a mut SystemStorage,
    pub current_shares_per_credit: u64,
    /// Which nodes work for which wallet, for the standing checks that count
    /// a wallet's nodes.
    pub identity: &'a identity::IdentityStorage,
    /// The chain rules in force (pallets/upgrade.rs). Code that changed
    /// behaviour in a version checks it.
    pub rules_version: u32,
}

/// No delegations: for applying a call outside a chain, as tests do.
pub static NO_IDENTITY: identity::IdentityStorage = identity::IdentityStorage {
    wallet_of: std::collections::BTreeMap::new(),
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PalletError {
    Unauthorized,
    NonceReplay,
    InsufficientBalance,
    InvalidArgs,
    CallDecodeFailure,
    UnknownCall,
}

impl std::fmt::Display for PalletError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl std::error::Error for PalletError {}

pub trait Pallet {
    type Storage: Default + Clone + Serialize + DeserializeOwned;

    type Call: Serialize + DeserializeOwned;

    const PALLET_IDX: u8;

    fn apply(
        storage: &mut Self::Storage,
        call: Self::Call,
        ctx: &mut ApplyCtx<'_>,
    ) -> Result<(), PalletError>;

    fn pallet_root(storage: &Self::Storage) -> [u8; 32];
}

#[cfg(test)]
mod trait_smoke_test {
    use super::*;
    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};

    #[derive(Default, Clone, Serialize, Deserialize)]
    struct NoopStorage {
        counter: u64,
    }

    #[derive(Serialize, Deserialize)]
    enum NoopCall {
        Increment,
    }

    struct NoopPallet;
    impl Pallet for NoopPallet {
        type Storage = NoopStorage;
        type Call = NoopCall;
        const PALLET_IDX: u8 = 255;

        fn apply(
            storage: &mut Self::Storage,
            call: Self::Call,
            _ctx: &mut ApplyCtx<'_>,
        ) -> Result<(), PalletError> {
            match call {
                NoopCall::Increment => {
                    storage.counter = storage.counter.saturating_add(1);
                    Ok(())
                }
            }
        }

        fn pallet_root(storage: &Self::Storage) -> [u8; 32] {
            let mut h = Sha256::new();
            h.update(storage.counter.to_le_bytes());
            h.finalize().into()
        }
    }

    #[test]
    fn noop_pallet_applies_and_changes_root() {
        let mut storage = NoopStorage::default();
        let author: PeerId = [1u8; 32];
        let signer: PeerId = [2u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ApplyCtx {
            block_height: 1,
            timestamp_ms: 1_000,
            author: &author,
            signer: &signer,
            system: &mut sys,
            current_shares_per_credit: economy::DEFAULT_SHARES_PER_CREDIT,
            identity: &crate::pallets::NO_IDENTITY,
            rules_version: crate::pallets::upgrade::GENESIS_RULES_VERSION,
        };

        let root_before = NoopPallet::pallet_root(&storage);
        NoopPallet::apply(&mut storage, NoopCall::Increment, &mut ctx).unwrap();
        let root_after = NoopPallet::pallet_root(&storage);

        assert_ne!(root_before, root_after);
        assert_eq!(storage.counter, 1);
    }
}
