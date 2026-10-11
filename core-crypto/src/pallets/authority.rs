// SPDX-License-Identifier: BUSL-1.1
use std::collections::HashMap;

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{ApplyCtx, Pallet, PalletError};
use crate::identity::PeerId;
use crate::validators::{authority_peer_id, ValidatorSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorityMetadata {
    pub channels: Vec<String>,
    pub attestation_hash: [u8; 32],
    pub since_height: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AuthorityStorage {
    pub authorities: Vec<[u8; 32]>,
    pub peer_ids: Vec<PeerId>,
    pub metadata: HashMap<[u8; 32], AuthorityMetadata>,
    #[serde(default)]
    pub founder: Option<[u8; 32]>,
    #[serde(default)]
    pub dissolved_at: Option<u64>,
    #[serde(default)]
    pub equivocations: std::collections::BTreeMap<[u8; 32], std::collections::BTreeSet<u64>>,
    #[serde(default)]
    pub history: Vec<(u64, Vec<[u8; 32]>)>,
}

impl AuthorityStorage {
    pub fn with_genesis(pubkey: [u8; 32], channels: Vec<String>) -> Self {
        let mut s = Self::default();
        if pubkey != [0u8; 32] {
            if let Ok(vk) = VerifyingKey::from_bytes(&pubkey) {
                s.founder = Some(pubkey);
                s.authorities.push(pubkey);
                s.history.push((0, vec![pubkey]));
                s.peer_ids.push(authority_peer_id(&vk));
                s.metadata.insert(
                    pubkey,
                    AuthorityMetadata {
                        channels,
                        attestation_hash: [0u8; 32],
                        since_height: 0,
                    },
                );
            }
        }
        s
    }

    pub fn len(&self) -> usize {
        self.authorities.len()
    }
    pub fn is_empty(&self) -> bool {
        self.authorities.is_empty()
    }

    pub fn is_authority_signer(&self, signer: &PeerId) -> bool {
        self.authorities.iter().any(|p| p == signer)
    }

    pub fn validators_at(&self, height: u64) -> Vec<[u8; 32]> {
        self.history
            .iter()
            .rev()
            .find(|(from, _)| *from <= height)
            .map(|(_, set)| set.clone())
            .unwrap_or_else(|| self.authorities.clone())
    }

    pub fn note_membership(&mut self, height: u64) {
        if self
            .history
            .last()
            .is_some_and(|(_, set)| *set == self.authorities)
        {
            return;
        }
        if self.history.last().is_some_and(|(from, _)| *from == height) {
            self.history.pop();
        }
        self.history.push((height, self.authorities.clone()));
    }

    pub fn prune_history(&mut self, finalized: u64) {
        let keep_from = self
            .history
            .iter()
            .rposition(|(from, _)| *from <= finalized)
            .unwrap_or(0);
        self.history.drain(..keep_from);
    }

    pub fn is_founder(&self, signer: &PeerId) -> bool {
        self.dissolved_at.is_none() && self.founder.as_ref() == Some(signer)
    }

    pub fn is_dissolved(&self) -> bool {
        self.dissolved_at.is_some()
    }

    pub fn partners(&self) -> Vec<[u8; 32]> {
        self.authorities
            .iter()
            .filter(|p| self.founder.as_ref() != Some(*p))
            .copied()
            .collect()
    }

    pub fn add_member(
        &mut self,
        pubkey: [u8; 32],
        channels: Vec<String>,
        attestation_hash: [u8; 32],
        height: u64,
    ) -> Result<(), PalletError> {
        let vk = VerifyingKey::from_bytes(&pubkey).map_err(|_| PalletError::InvalidArgs)?;
        if self.is_authority_signer(&pubkey)
            || self.authorities.len() >= MAX_AUTHORITIES
            || !channels_valid(&channels)
        {
            return Err(PalletError::InvalidArgs);
        }
        self.authorities.push(pubkey);
        self.peer_ids.push(authority_peer_id(&vk));
        self.metadata.insert(
            pubkey,
            AuthorityMetadata {
                channels,
                attestation_hash,
                since_height: height,
            },
        );
        Ok(())
    }

    pub fn remove_member(&mut self, pubkey: &[u8; 32]) -> Result<(), PalletError> {
        if self.founder.as_ref() == Some(pubkey) && self.dissolved_at.is_none() {
            return Err(PalletError::Unauthorized);
        }
        let Some(idx) = self.authorities.iter().position(|p| p == pubkey) else {
            return Err(PalletError::InvalidArgs);
        };
        if self.authorities.len() == 1 {
            return Err(PalletError::InvalidArgs);
        }
        self.authorities.remove(idx);
        self.peer_ids.remove(idx);
        self.metadata.remove(pubkey);
        Ok(())
    }

    pub fn punish_equivocation(
        &mut self,
        offender: [u8; 32],
        height: u64,
    ) -> Result<(), PalletError> {
        if !self.is_authority_signer(&offender) {
            return Err(PalletError::InvalidArgs);
        }
        if !self
            .equivocations
            .entry(offender)
            .or_default()
            .insert(height)
        {
            return Err(PalletError::InvalidArgs);
        }
        let protected = self.dissolved_at.is_none() && self.founder == Some(offender);
        if !protected && self.authorities.len() > 1 {
            self.remove_member(&offender)?;
        }
        Ok(())
    }

    pub fn dissolve(&mut self, height: u64) -> Result<(), PalletError> {
        if self.dissolved_at.is_some() {
            return Err(PalletError::InvalidArgs);
        }
        self.dissolved_at = Some(height);
        self.founder = None;
        Ok(())
    }

    pub fn serves_channel(&self, pubkey: &PeerId, channel: &str) -> bool {
        match self.metadata.get(pubkey) {
            Some(m) => m.channels.iter().any(|c| c == channel),
            None => false,
        }
    }

    /// How many authorities have announced `channel`. A channel nobody serves
    /// can only ever time out, so callers use this to refuse — and, above all,
    /// to not charge for — a request that has nowhere to go.
    pub fn servers_of(&self, channel: &str) -> usize {
        self.metadata
            .values()
            .filter(|m| m.channels.iter().any(|c| c == channel))
            .count()
    }

    pub fn to_validator_set(&self) -> ValidatorSet {
        let mut vk_authorities = Vec::with_capacity(self.authorities.len());
        for raw in &self.authorities {
            match VerifyingKey::from_bytes(raw) {
                Ok(vk) => vk_authorities.push(vk),
                Err(_) => return ValidatorSet::default(),
            }
        }
        ValidatorSet {
            authorities: vk_authorities,
            peer_ids: self.peer_ids.clone(),
        }
    }
}

pub const MAX_AUTHORITIES: usize = 64;

pub const MAX_AUTHORITY_CHANNELS: usize = 16;

pub fn channels_valid(channels: &[String]) -> bool {
    channels.len() <= MAX_AUTHORITY_CHANNELS
        && channels
            .iter()
            .all(|c| !c.is_empty() && c.len() <= crate::pallets::nai::MAX_CHANNEL_NAME_LEN)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AuthorityCall {
    Add {
        pubkey: [u8; 32],
        channels: Vec<String>,
        attestation_hash: [u8; 32],
    },
    Remove {
        pubkey: [u8; 32],
    },
    RotateKey {
        old_pubkey: [u8; 32],
        new_pubkey: [u8; 32],
    },
    SetChannels {
        pubkey: [u8; 32],
        channels: Vec<String>,
    },
    Dissolve,
}

pub struct AuthorityPallet;

impl Pallet for AuthorityPallet {
    type Storage = AuthorityStorage;
    type Call = AuthorityCall;
    const PALLET_IDX: u8 = 5;

    fn apply(
        storage: &mut Self::Storage,
        call: Self::Call,
        ctx: &mut ApplyCtx<'_>,
    ) -> Result<(), PalletError> {
        let signer = *ctx.signer;
        let founder = storage.is_founder(&signer);
        match call {
            AuthorityCall::Add {
                pubkey,
                channels,
                attestation_hash,
            } => {
                if !founder {
                    return Err(PalletError::Unauthorized);
                }
                storage.add_member(pubkey, channels, attestation_hash, ctx.block_height)
            }
            AuthorityCall::Remove { pubkey } => {
                if !(founder || (pubkey == signer && storage.is_authority_signer(&signer))) {
                    return Err(PalletError::Unauthorized);
                }
                storage.remove_member(&pubkey)
            }
            AuthorityCall::SetChannels { pubkey, channels } => {
                if !(founder || pubkey == signer) {
                    return Err(PalletError::Unauthorized);
                }
                if !channels_valid(&channels) {
                    return Err(PalletError::InvalidArgs);
                }
                let Some(meta) = storage.metadata.get_mut(&pubkey) else {
                    return Err(PalletError::InvalidArgs);
                };
                meta.channels = channels;
                Ok(())
            }
            AuthorityCall::RotateKey {
                old_pubkey,
                new_pubkey,
            } => {
                if signer != old_pubkey {
                    return Err(PalletError::Unauthorized);
                }
                if old_pubkey == new_pubkey {
                    return Err(PalletError::InvalidArgs);
                }
                let new_vk =
                    VerifyingKey::from_bytes(&new_pubkey).map_err(|_| PalletError::InvalidArgs)?;
                if storage.is_authority_signer(&new_pubkey) {
                    return Err(PalletError::InvalidArgs);
                }
                let Some(idx) = storage.authorities.iter().position(|p| p == &old_pubkey) else {
                    return Err(PalletError::InvalidArgs);
                };
                storage.authorities[idx] = new_pubkey;
                storage.peer_ids[idx] = authority_peer_id(&new_vk);
                if let Some(mut meta) = storage.metadata.remove(&old_pubkey) {
                    meta.since_height = ctx.block_height;
                    storage.metadata.insert(new_pubkey, meta);
                }
                if storage.founder == Some(old_pubkey) {
                    storage.founder = Some(new_pubkey);
                }
                Ok(())
            }
            AuthorityCall::Dissolve => {
                if !founder {
                    return Err(PalletError::Unauthorized);
                }
                storage.dissolve(ctx.block_height)
            }
        }
    }

    fn pallet_root(storage: &Self::Storage) -> [u8; 32] {
        let mut h = Sha256::new();
        for pk in &storage.authorities {
            h.update(pk);
        }
        for pid in &storage.peer_ids {
            h.update(pid);
        }
        let mut keys: Vec<&[u8; 32]> = storage.metadata.keys().collect();
        keys.sort_unstable();
        for k in keys {
            h.update(k);
            let m = &storage.metadata[k];
            h.update((m.channels.len() as u32).to_le_bytes());
            for c in &m.channels {
                h.update((c.len() as u32).to_le_bytes());
                h.update(c.as_bytes());
            }
            h.update(m.attestation_hash);
            h.update(m.since_height.to_le_bytes());
        }
        match storage.founder {
            Some(f) => {
                h.update([1u8]);
                h.update(f);
            }
            None => h.update([0u8]),
        }
        match storage.dissolved_at {
            Some(at) => {
                h.update([1u8]);
                h.update(at.to_le_bytes());
            }
            None => h.update([0u8]),
        }
        for (from, set) in &storage.history {
            h.update(from.to_le_bytes());
            h.update((set.len() as u32).to_le_bytes());
            for pk in set {
                h.update(pk);
            }
        }
        for (offender, heights) in &storage.equivocations {
            h.update(offender);
            for height in heights {
                h.update(height.to_le_bytes());
            }
        }
        h.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pallets::system::SystemStorage;
    use ed25519_dalek::SigningKey;
    use rand::RngCore;

    fn rand_key() -> ([u8; 32], SigningKey) {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let sk = SigningKey::from_bytes(&bytes);
        (sk.verifying_key().to_bytes(), sk)
    }

    fn ctx<'a>(
        signer: &'a PeerId,
        author: &'a PeerId,
        height: u64,
        sys: &'a mut SystemStorage,
    ) -> ApplyCtx<'a> {
        ApplyCtx {
            block_height: height,
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
    fn genesis_seeds_first_authority() {
        let (pk, _) = rand_key();
        let storage = AuthorityStorage::with_genesis(pk, vec!["large_chat".into()]);
        assert_eq!(storage.len(), 1);
        assert!(storage.is_authority_signer(&pk));
        assert_eq!(
            storage.metadata.get(&pk).unwrap().channels,
            vec!["large_chat".to_string()]
        );
    }

    #[test]
    fn genesis_with_zero_pubkey_is_empty() {
        let storage = AuthorityStorage::with_genesis([0u8; 32], vec![]);
        assert!(storage.is_empty());
    }

    #[test]
    fn add_promotes_new_validator() {
        let (pk1, _) = rand_key();
        let (pk2, _) = rand_key();
        let mut storage = AuthorityStorage::with_genesis(pk1, vec![]);
        let mut sys = SystemStorage::default();
        let mut c = ctx(&pk1, &pk1, 10, &mut sys);

        AuthorityPallet::apply(
            &mut storage,
            AuthorityCall::Add {
                pubkey: pk2,
                channels: vec!["frontier".into()],
                attestation_hash: [7u8; 32],
            },
            &mut c,
        )
        .unwrap();

        assert_eq!(storage.len(), 2);
        assert!(storage.is_authority_signer(&pk2));
        assert_eq!(
            storage.metadata[&pk2].channels,
            vec!["frontier".to_string()]
        );
        assert_eq!(storage.metadata[&pk2].since_height, 10);
        assert_eq!(storage.metadata[&pk2].attestation_hash, [7u8; 32]);
    }

    #[test]
    fn servers_of_counts_only_authorities_announcing_the_channel() {
        let mut storage = AuthorityStorage::with_genesis([1u8; 32], vec!["chat".into()]);
        assert_eq!(storage.servers_of("chat"), 1);
        assert_eq!(storage.servers_of("frontier"), 0, "announced by nobody");

        let signer = [1u8; 32];
        let mut sys = crate::pallets::system::SystemStorage::default();
        let mut ctx = ApplyCtx {
            block_height: 1,
            timestamp_ms: 0,
            author: &signer,
            signer: &signer,
            system: &mut sys,
            current_shares_per_credit: 1_000,
            identity: &crate::pallets::NO_IDENTITY,
            rules_version: crate::pallets::upgrade::GENESIS_RULES_VERSION,
        };
        let pk2 = ed25519_dalek::SigningKey::from_bytes(&[2u8; 32])
            .verifying_key()
            .to_bytes();
        AuthorityPallet::apply(
            &mut storage,
            AuthorityCall::Add {
                pubkey: pk2,
                channels: vec!["chat".into(), "frontier".into()],
                attestation_hash: [0u8; 32],
            },
            &mut ctx,
        )
        .unwrap();
        assert_eq!(storage.servers_of("chat"), 2);
        assert_eq!(storage.servers_of("frontier"), 1);
        assert_eq!(storage.servers_of("large_chat"), 0);
    }

    #[test]
    fn add_duplicate_rejected() {
        let (pk, _) = rand_key();
        let mut storage = AuthorityStorage::with_genesis(pk, vec![]);
        let mut sys = SystemStorage::default();
        let mut c = ctx(&pk, &pk, 1, &mut sys);
        let result = AuthorityPallet::apply(
            &mut storage,
            AuthorityCall::Add {
                pubkey: pk,
                channels: vec![],
                attestation_hash: [0u8; 32],
            },
            &mut c,
        );
        assert_eq!(result, Err(PalletError::InvalidArgs));
        assert_eq!(storage.len(), 1);
    }

    #[test]
    fn remove_drops_validator_and_metadata() {
        let (pk1, _) = rand_key();
        let (pk2, _) = rand_key();
        let mut storage = AuthorityStorage::with_genesis(pk1, vec![]);
        let mut sys = SystemStorage::default();
        let mut c = ctx(&pk1, &pk1, 1, &mut sys);
        AuthorityPallet::apply(
            &mut storage,
            AuthorityCall::Add {
                pubkey: pk2,
                channels: vec!["large_chat".into()],
                attestation_hash: [0u8; 32],
            },
            &mut c,
        )
        .unwrap();
        assert_eq!(storage.len(), 2);

        AuthorityPallet::apply(&mut storage, AuthorityCall::Remove { pubkey: pk2 }, &mut c)
            .unwrap();
        assert_eq!(storage.len(), 1);
        assert!(!storage.is_authority_signer(&pk2));
        assert!(!storage.metadata.contains_key(&pk2));
    }

    #[test]
    fn remove_unknown_rejected() {
        let (pk1, _) = rand_key();
        let (pk2, _) = rand_key();
        let mut storage = AuthorityStorage::with_genesis(pk1, vec![]);
        let mut sys = SystemStorage::default();
        let mut c = ctx(&pk1, &pk1, 1, &mut sys);
        let result =
            AuthorityPallet::apply(&mut storage, AuthorityCall::Remove { pubkey: pk2 }, &mut c);
        assert_eq!(result, Err(PalletError::InvalidArgs));
    }

    #[test]
    fn set_channels_updates_metadata() {
        let (pk, _) = rand_key();
        let mut storage = AuthorityStorage::with_genesis(pk, vec!["large_chat".into()]);
        let mut sys = SystemStorage::default();
        let mut c = ctx(&pk, &pk, 1, &mut sys);
        AuthorityPallet::apply(
            &mut storage,
            AuthorityCall::SetChannels {
                pubkey: pk,
                channels: vec!["frontier".into(), "large_realtime".into()],
            },
            &mut c,
        )
        .unwrap();
        assert_eq!(
            storage.metadata[&pk].channels,
            vec!["frontier".to_string(), "large_realtime".to_string()]
        );
    }

    #[test]
    fn rotate_key_swaps_pubkey_at_same_slot_index() {
        let (pk1, _) = rand_key();
        let (pk2, _) = rand_key();
        let (pk_new, _) = rand_key();
        let mut storage = AuthorityStorage::with_genesis(pk1, vec!["large_chat".into()]);
        let mut sys = SystemStorage::default();
        let mut c = ctx(&pk1, &pk1, 1, &mut sys);
        AuthorityPallet::apply(
            &mut storage,
            AuthorityCall::Add {
                pubkey: pk2,
                channels: vec!["frontier".into()],
                attestation_hash: [9u8; 32],
            },
            &mut c,
        )
        .unwrap();
        assert_eq!(storage.authorities, vec![pk1, pk2]);

        let mut c2 = ctx(&pk1, &pk1, 42, &mut sys);
        AuthorityPallet::apply(
            &mut storage,
            AuthorityCall::RotateKey {
                old_pubkey: pk1,
                new_pubkey: pk_new,
            },
            &mut c2,
        )
        .unwrap();

        assert_eq!(
            storage.authorities,
            vec![pk_new, pk2],
            "slot index preserved"
        );
        assert!(storage.metadata.contains_key(&pk_new));
        assert!(!storage.metadata.contains_key(&pk1));
        assert_eq!(
            storage.metadata[&pk_new].channels,
            vec!["large_chat".to_string()],
            "metadata transferred"
        );
        assert_eq!(storage.metadata[&pk_new].since_height, 42);
    }

    #[test]
    fn rotate_key_refuses_when_signer_is_not_old_key() {
        let (pk1, _) = rand_key();
        let (pk_attacker, _) = rand_key();
        let (pk_new, _) = rand_key();
        let mut storage = AuthorityStorage::with_genesis(pk1, vec![]);
        let mut sys = SystemStorage::default();
        let mut c = ctx(&pk_attacker, &pk_attacker, 1, &mut sys);

        let result = AuthorityPallet::apply(
            &mut storage,
            AuthorityCall::RotateKey {
                old_pubkey: pk1,
                new_pubkey: pk_new,
            },
            &mut c,
        );
        assert_eq!(result, Err(PalletError::Unauthorized));
        assert_eq!(storage.authorities, vec![pk1], "no rotation happened");
    }

    #[test]
    fn rotate_key_refuses_when_new_already_authority() {
        let (pk1, _) = rand_key();
        let (pk2, _) = rand_key();
        let mut storage = AuthorityStorage::with_genesis(pk1, vec![]);
        let mut sys = SystemStorage::default();
        let mut c = ctx(&pk1, &pk1, 1, &mut sys);
        AuthorityPallet::apply(
            &mut storage,
            AuthorityCall::Add {
                pubkey: pk2,
                channels: vec![],
                attestation_hash: [0u8; 32],
            },
            &mut c,
        )
        .unwrap();

        let result = AuthorityPallet::apply(
            &mut storage,
            AuthorityCall::RotateKey {
                old_pubkey: pk1,
                new_pubkey: pk2,
            },
            &mut c,
        );
        assert_eq!(result, Err(PalletError::InvalidArgs));
    }

    #[test]
    fn pallet_root_changes_when_authority_added() {
        let (pk1, _) = rand_key();
        let (pk2, _) = rand_key();
        let storage1 = AuthorityStorage::with_genesis(pk1, vec![]);
        let r1 = AuthorityPallet::pallet_root(&storage1);

        let mut storage2 = storage1.clone();
        let mut sys = SystemStorage::default();
        let mut c = ctx(&pk1, &pk1, 1, &mut sys);
        AuthorityPallet::apply(
            &mut storage2,
            AuthorityCall::Add {
                pubkey: pk2,
                channels: vec![],
                attestation_hash: [0u8; 32],
            },
            &mut c,
        )
        .unwrap();
        let r2 = AuthorityPallet::pallet_root(&storage2);
        assert_ne!(r1, r2);
    }

    #[test]
    fn serves_channel_intersects_metadata_channels() {
        let (pk1, _) = rand_key();
        let (pk2, _) = rand_key();
        let mut storage =
            AuthorityStorage::with_genesis(pk1, vec!["large_chat".into(), "large_realtime".into()]);
        let mut sys = SystemStorage::default();
        let mut c = ctx(&pk1, &pk1, 1, &mut sys);
        AuthorityPallet::apply(
            &mut storage,
            AuthorityCall::Add {
                pubkey: pk2,
                channels: vec!["frontier".into()],
                attestation_hash: [0u8; 32],
            },
            &mut c,
        )
        .unwrap();

        assert!(storage.serves_channel(&pk1, "large_chat"));
        assert!(storage.serves_channel(&pk1, "large_realtime"));
        assert!(!storage.serves_channel(&pk1, "frontier"));

        assert!(storage.serves_channel(&pk2, "frontier"));
        assert!(!storage.serves_channel(&pk2, "large_chat"));

        let (stranger, _) = rand_key();
        assert!(!storage.serves_channel(&stranger, "large_chat"));
    }

    #[test]
    fn to_validator_set_roundtrip_preserves_membership() {
        let (pk, _) = rand_key();
        let storage = AuthorityStorage::with_genesis(pk, vec![]);
        let vs = storage.to_validator_set();
        assert_eq!(vs.len(), 1);
        assert!(vs.is_authority(&storage.peer_ids[0]));
    }
}
