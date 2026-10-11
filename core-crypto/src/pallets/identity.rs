// SPDX-License-Identifier: BUSL-1.1
//! Identity (pallet 4): a wallet and the nodes that work for it.
//!
//! A node has to sign all the time - greetings, lottery claims, answers - so
//! its key has to open without anyone typing a password. A key like that
//! should not also hold the funds. This pallet splits the two: a node key
//! runs unattended, a wallet key stays under the owner's password, and a
//! delegation on chain says which wallet a node works for.
//!
//! What the delegation changes:
//! - A delegated node never holds credits. Whatever lands on it - its
//!   redeemed earnings, a transfer, a refund - moves to its wallet at the end
//!   of the extrinsic that put it there (`ChainState::sweep_delegated`).
//! - A session the wallet paid for can be used by its nodes.
//! - The wallet's standing (market, governance) counts its nodes' history.
//!   A delegated node has no vote of its own, so nothing is counted twice.
//!
//! Both sides consent: the wallet signs the extrinsic and the node signs the
//! pairing, so nobody can claim a node that is not theirs, nor pin a node on
//! a wallet that did not ask for it.
//!
//! The pallet joins the state root only once it holds a delegation, so the
//! history written before it existed reconciles to the same roots.

use std::collections::BTreeMap;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::PalletError;
use crate::identity::PeerId;

/// Enough for a desktop, a laptop and a few servers; a bound so one wallet
/// cannot make standing checks walk an unbounded list.
pub const MAX_NODES_PER_WALLET: usize = 8;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct IdentityStorage {
    /// node -> the wallet it works for.
    pub wallet_of: BTreeMap<PeerId, PeerId>,
}

impl IdentityStorage {
    pub fn is_empty(&self) -> bool {
        self.wallet_of.is_empty()
    }

    /// The wallet a node works for, if it is delegated.
    pub fn wallet_of(&self, node: &PeerId) -> Option<PeerId> {
        self.wallet_of.get(node).copied()
    }

    /// The account that owns what `peer` does: its wallet when delegated,
    /// itself otherwise.
    pub fn account_of(&self, peer: &PeerId) -> PeerId {
        self.wallet_of(peer).unwrap_or(*peer)
    }

    pub fn nodes_of(&self, wallet: &PeerId) -> Vec<PeerId> {
        self.wallet_of
            .iter()
            .filter(|(_, w)| *w == wallet)
            .map(|(n, _)| *n)
            .collect()
    }

    /// `peer` and the nodes working for it: the accounts whose history is
    /// the wallet's standing. A delegated node's own group is empty - its
    /// standing belongs to its wallet.
    pub fn group_of(&self, peer: &PeerId) -> Vec<PeerId> {
        if self.wallet_of.contains_key(peer) {
            return Vec::new();
        }
        let mut group = vec![*peer];
        group.extend(self.nodes_of(peer));
        group
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum IdentityCall {
    /// Signed by the wallet. `node_sig` is the node's signature over
    /// `delegation_digest(wallet, node)`.
    Delegate {
        node: PeerId,
        #[serde(with = "crate::wire::bytes64")]
        node_sig: [u8; 64],
    },
    /// Ends a delegation. Signed by the wallet or by the node.
    Revoke { node: PeerId },
}

/// What a node signs to agree to work for `wallet`. Bound to the network, so
/// a pairing made on the testnet cannot be replayed on mainnet.
pub fn delegation_digest(wallet: &PeerId, node: &PeerId) -> [u8; 32] {
    crate::network::signing_digest(crate::network::domains::DELEGATION, &[wallet, node])
}

/// The node's half of a delegation.
pub fn sign_delegation(node_key: &SigningKey, wallet: &PeerId) -> [u8; 64] {
    let node = node_key.verifying_key().to_bytes();
    node_key.sign(&delegation_digest(wallet, &node)).to_bytes()
}

pub struct IdentityPallet;

impl IdentityPallet {
    pub const PALLET_IDX: u8 = 4;

    /// Applies a call from `signer`. Returns the credits a new delegation
    /// moved from the node to its wallet.
    pub fn apply(
        storage: &mut IdentityStorage,
        balances: &mut super::balances::BalancesStorage,
        call: IdentityCall,
        signer: &PeerId,
    ) -> Result<u64, PalletError> {
        match call {
            IdentityCall::Delegate { node, node_sig } => {
                let wallet = *signer;
                if node == wallet {
                    return Err(PalletError::InvalidArgs);
                }
                // One level only: a wallet is not itself a node, and a node
                // does not have nodes of its own.
                if storage.wallet_of.contains_key(&wallet)
                    || storage.wallet_of.contains_key(&node)
                    || !storage.nodes_of(&node).is_empty()
                {
                    return Err(PalletError::InvalidArgs);
                }
                if storage.nodes_of(&wallet).len() >= MAX_NODES_PER_WALLET {
                    return Err(PalletError::InvalidArgs);
                }
                let vk = VerifyingKey::from_bytes(&node).map_err(|_| PalletError::InvalidArgs)?;
                vk.verify_strict(
                    &delegation_digest(&wallet, &node),
                    &Signature::from_bytes(&node_sig),
                )
                .map_err(|_| PalletError::Unauthorized)?;
                storage.wallet_of.insert(node, wallet);
                let held = balances.balance_of(&node);
                if held > 0 {
                    balances.burn_checked(&node, held)?;
                    balances.mint(wallet, held);
                }
                Ok(held)
            }
            IdentityCall::Revoke { node } => {
                let Some(wallet) = storage.wallet_of(&node) else {
                    return Err(PalletError::InvalidArgs);
                };
                if signer != &wallet && signer != &node {
                    return Err(PalletError::Unauthorized);
                }
                storage.wallet_of.remove(&node);
                Ok(0)
            }
        }
    }

    pub fn pallet_root(storage: &IdentityStorage) -> [u8; 32] {
        let mut h = Sha256::new();
        for (node, wallet) in &storage.wallet_of {
            h.update(node);
            h.update(wallet);
        }
        h.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pallets::balances::BalancesStorage;

    fn key(seed: u8) -> (SigningKey, PeerId) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let pk = sk.verifying_key().to_bytes();
        (sk, pk)
    }

    #[test]
    fn a_node_joins_a_wallet_with_both_signatures_and_brings_its_credits() {
        let (_, wallet) = key(1);
        let (node_sk, node) = key(2);
        let mut ids = IdentityStorage::default();
        let mut bal = BalancesStorage::default();
        bal.mint(node, 40);
        let sig = sign_delegation(&node_sk, &wallet);
        let moved = IdentityPallet::apply(
            &mut ids,
            &mut bal,
            IdentityCall::Delegate {
                node,
                node_sig: sig,
            },
            &wallet,
        )
        .unwrap();
        assert_eq!(moved, 40);
        assert_eq!(bal.balance_of(&node), 0);
        assert_eq!(bal.balance_of(&wallet), 40);
        assert_eq!(ids.account_of(&node), wallet);
        assert_eq!(ids.account_of(&wallet), wallet);
        assert_eq!(ids.group_of(&wallet), vec![wallet, node]);
        assert!(ids.group_of(&node).is_empty());
    }

    #[test]
    fn nobody_can_claim_a_node_that_did_not_sign() {
        let (_, wallet) = key(1);
        let (_, node) = key(2);
        let (other_sk, _) = key(3);
        let mut ids = IdentityStorage::default();
        let mut bal = BalancesStorage::default();
        bal.mint(node, 40);
        // Signed by someone else, and a node signature for another wallet.
        for sig in [
            sign_delegation(&other_sk, &wallet),
            sign_delegation(&SigningKey::from_bytes(&[2; 32]), &[9; 32]),
        ] {
            let r = IdentityPallet::apply(
                &mut ids,
                &mut bal,
                IdentityCall::Delegate {
                    node,
                    node_sig: sig,
                },
                &wallet,
            );
            assert_eq!(r, Err(PalletError::Unauthorized));
        }
        assert_eq!(bal.balance_of(&node), 40);
        assert!(ids.is_empty());
    }

    #[test]
    fn delegations_are_one_level_deep_and_bounded() {
        let (_, wallet) = key(1);
        let (node_sk, node) = key(2);
        let mut ids = IdentityStorage::default();
        let mut bal = BalancesStorage::default();
        let delegate =
            |ids: &mut IdentityStorage, bal: &mut BalancesStorage, sk: &SigningKey, w: PeerId| {
                let n = sk.verifying_key().to_bytes();
                IdentityPallet::apply(
                    ids,
                    bal,
                    IdentityCall::Delegate {
                        node: n,
                        node_sig: sign_delegation(sk, &w),
                    },
                    &w,
                )
            };
        delegate(&mut ids, &mut bal, &node_sk, wallet).unwrap();
        // Again, to the same or another wallet.
        assert!(delegate(&mut ids, &mut bal, &node_sk, wallet).is_err());
        assert!(delegate(&mut ids, &mut bal, &node_sk, [7; 32]).is_err());
        // The node cannot take nodes, and the wallet cannot become a node.
        let (third_sk, _) = key(3);
        assert!(delegate(&mut ids, &mut bal, &third_sk, node).is_err());
        assert!(delegate(
            &mut ids,
            &mut bal,
            &SigningKey::from_bytes(&[1; 32]),
            [8; 32]
        )
        .is_err());
        // Bounded per wallet.
        for seed in 10..(10 + MAX_NODES_PER_WALLET as u8 - 1) {
            delegate(
                &mut ids,
                &mut bal,
                &SigningKey::from_bytes(&[seed; 32]),
                wallet,
            )
            .unwrap();
        }
        assert_eq!(ids.nodes_of(&wallet).len(), MAX_NODES_PER_WALLET);
        assert!(delegate(
            &mut ids,
            &mut bal,
            &SigningKey::from_bytes(&[99; 32]),
            wallet
        )
        .is_err());
    }

    #[test]
    fn either_side_can_end_it_and_nobody_else() {
        let (_, wallet) = key(1);
        let (node_sk, node) = key(2);
        let mut ids = IdentityStorage::default();
        let mut bal = BalancesStorage::default();
        for revoker in [wallet, node] {
            IdentityPallet::apply(
                &mut ids,
                &mut bal,
                IdentityCall::Delegate {
                    node,
                    node_sig: sign_delegation(&node_sk, &wallet),
                },
                &wallet,
            )
            .unwrap();
            assert_eq!(
                IdentityPallet::apply(&mut ids, &mut bal, IdentityCall::Revoke { node }, &[5; 32]),
                Err(PalletError::Unauthorized)
            );
            IdentityPallet::apply(&mut ids, &mut bal, IdentityCall::Revoke { node }, &revoker)
                .unwrap();
            assert!(ids.is_empty());
        }
    }
}
