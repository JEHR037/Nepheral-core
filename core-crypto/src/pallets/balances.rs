// SPDX-License-Identifier: BUSL-1.1
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{ApplyCtx, Pallet, PalletError};
use crate::identity::PeerId;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BalancesStorage {
    pub balances: HashMap<PeerId, u64>,
}

impl BalancesStorage {
    pub fn balance_of(&self, account: &PeerId) -> u64 {
        self.balances.get(account).copied().unwrap_or(0)
    }

    pub fn mint(&mut self, account: PeerId, amount: u64) {
        *self.balances.entry(account).or_insert(0) =
            self.balance_of(&account).saturating_add(amount);
    }

    pub fn burn_checked(&mut self, account: &PeerId, amount: u64) -> Result<(), PalletError> {
        let current = self.balance_of(account);
        if current < amount {
            return Err(PalletError::InsufficientBalance);
        }
        self.balances.insert(*account, current - amount);
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum BalancesCall {
    Transfer {
        to: PeerId,
        amount: u64,
    },
    /// Destroys `amount` of the signer's own credits. Nobody can burn
    /// anyone else's.
    Burn {
        amount: u64,
    },
}

pub struct BalancesPallet;

impl Pallet for BalancesPallet {
    type Storage = BalancesStorage;
    type Call = BalancesCall;
    const PALLET_IDX: u8 = 1;

    fn apply(
        storage: &mut Self::Storage,
        call: Self::Call,
        ctx: &mut ApplyCtx<'_>,
    ) -> Result<(), PalletError> {
        match call {
            BalancesCall::Transfer { to, amount } => {
                if amount == 0 {
                    return Err(PalletError::InvalidArgs);
                }
                if &to == ctx.signer {
                    return Err(PalletError::InvalidArgs);
                }
                storage.burn_checked(ctx.signer, amount)?;
                storage.mint(to, amount);
                Ok(())
            }
            BalancesCall::Burn { amount } => {
                if amount == 0 {
                    return Err(PalletError::InvalidArgs);
                }
                storage.burn_checked(ctx.signer, amount)
            }
        }
    }

    fn pallet_root(storage: &Self::Storage) -> [u8; 32] {
        let mut h = Sha256::new();
        let mut balances: Vec<(&PeerId, &u64)> = storage.balances.iter().collect();
        balances.sort_by_key(|(k, _)| **k);
        for (peer, bal) in balances {
            h.update(peer);
            h.update(bal.to_le_bytes());
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
    fn transfer_moves_credits_atomically() {
        let mut storage = BalancesStorage::default();
        let alice: PeerId = [1u8; 32];
        let bob: PeerId = [2u8; 32];
        storage.mint(alice, 100);
        let author = alice;
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&alice, &author, &mut sys);

        BalancesPallet::apply(
            &mut storage,
            BalancesCall::Transfer {
                to: bob,
                amount: 30,
            },
            &mut ctx,
        )
        .unwrap();

        assert_eq!(storage.balance_of(&alice), 70);
        assert_eq!(storage.balance_of(&bob), 30);
    }

    #[test]
    fn transfer_insufficient_balance_rejects_and_does_not_mutate() {
        let mut storage = BalancesStorage::default();
        let alice: PeerId = [1u8; 32];
        let bob: PeerId = [2u8; 32];
        storage.mint(alice, 10);
        let author = alice;
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&alice, &author, &mut sys);

        let result = BalancesPallet::apply(
            &mut storage,
            BalancesCall::Transfer {
                to: bob,
                amount: 50,
            },
            &mut ctx,
        );
        assert_eq!(result, Err(PalletError::InsufficientBalance));
        assert_eq!(storage.balance_of(&alice), 10, "alice must not be debited");
        assert_eq!(storage.balance_of(&bob), 0, "bob must not be credited");
    }

    #[test]
    fn zero_amount_rejected() {
        let mut storage = BalancesStorage::default();
        let alice: PeerId = [1u8; 32];
        storage.mint(alice, 100);
        let author = alice;
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&alice, &author, &mut sys);

        let result = BalancesPallet::apply(
            &mut storage,
            BalancesCall::Transfer {
                to: [2u8; 32],
                amount: 0,
            },
            &mut ctx,
        );
        assert_eq!(result, Err(PalletError::InvalidArgs));
    }

    #[test]
    fn self_transfer_rejected() {
        let mut storage = BalancesStorage::default();
        let alice: PeerId = [1u8; 32];
        storage.mint(alice, 100);
        let author = alice;
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&alice, &author, &mut sys);

        let result = BalancesPallet::apply(
            &mut storage,
            BalancesCall::Transfer {
                to: alice,
                amount: 10,
            },
            &mut ctx,
        );
        assert_eq!(result, Err(PalletError::InvalidArgs));
    }

    #[test]
    fn pallet_root_deterministic_under_reorder() {
        let mut a = BalancesStorage::default();
        let mut b = BalancesStorage::default();
        a.balances.insert([1u8; 32], 50);
        a.balances.insert([2u8; 32], 70);
        b.balances.insert([2u8; 32], 70);
        b.balances.insert([1u8; 32], 50);
        assert_eq!(
            BalancesPallet::pallet_root(&a),
            BalancesPallet::pallet_root(&b)
        );
    }

    #[test]
    fn burn_checked_rejects_overdraft() {
        let mut storage = BalancesStorage::default();
        storage.mint([1u8; 32], 5);
        assert_eq!(
            storage.burn_checked(&[1u8; 32], 10),
            Err(PalletError::InsufficientBalance)
        );
        assert_eq!(storage.balance_of(&[1u8; 32]), 5);
    }

    #[test]
    fn a_burn_destroys_only_the_signers_own_credits() {
        let mut storage = BalancesStorage::default();
        let alice: PeerId = [1u8; 32];
        let bob: PeerId = [2u8; 32];
        storage.mint(alice, 100);
        storage.mint(bob, 50);
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&alice, &alice, &mut sys);

        BalancesPallet::apply(&mut storage, BalancesCall::Burn { amount: 60 }, &mut ctx).unwrap();
        assert_eq!(storage.balance_of(&alice), 40);
        assert_eq!(storage.balance_of(&bob), 50);

        assert!(
            BalancesPallet::apply(&mut storage, BalancesCall::Burn { amount: 41 }, &mut ctx)
                .is_err(),
            "more than the signer holds"
        );
        assert!(
            BalancesPallet::apply(&mut storage, BalancesCall::Burn { amount: 0 }, &mut ctx)
                .is_err()
        );
        assert_eq!(storage.balance_of(&alice), 40);
    }
}
