// SPDX-License-Identifier: BUSL-1.1
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::contributions::ContributionsStorage;
use super::{ApplyCtx, PalletError};
use crate::identity::PeerId;

pub const PALLET_IDX: u8 = 8;

pub const VOTING_PERIOD_BLOCKS: u64 = 20_000;

pub const ENACTMENT_DELAY_BLOCKS: u64 = 20_000;

pub const MIN_VOTER_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1_000;

pub const QUORUM_NUM: u64 = 2;
pub const QUORUM_DEN: u64 = 3;
pub const PARTICIPATION_FLOOR_PCT: u64 = 20;

pub const MAX_OPEN_PROPOSALS: usize = 32;
pub const MAX_OPEN_PER_PROPOSER: usize = 2;
pub const MAX_DIRECT_SALE_ADDRESS_LEN: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum GovAction {
    SetSaleOracle {
        pubkey: Option<PeerId>,
    },
    SetDirectSaleWallet {
        address: Option<String>,
    },
    SetDailyMintCap {
        cap: u64,
    },
    SetChannelSessionCost {
        channel: String,
        cost: u64,
    },
    SetEmissionTarget {
        target_credits_per_hour: u64,
    },
    AddPartner {
        pubkey: [u8; 32],
        channels: Vec<String>,
        attestation_hash: [u8; 32],
    },
    RemovePartner {
        pubkey: [u8; 32],
    },
    /// After the founder resigns, the only way to schedule new chain rules.
    /// Appended last, so stored proposals keep their encoding.
    ScheduleUpgrade {
        version: u32,
        activate_at_ms: u64,
        release: [u8; 32],
    },
    CancelUpgrade,
}

impl GovAction {
    pub fn changes_membership(&self) -> bool {
        matches!(
            self,
            GovAction::AddPartner { .. } | GovAction::RemovePartner { .. }
        )
    }

    fn validate(&self) -> Result<(), PalletError> {
        let ok = match self {
            GovAction::SetSaleOracle { pubkey } => pubkey
                .map(|pk| ed25519_dalek::VerifyingKey::from_bytes(&pk).is_ok())
                .unwrap_or(true),
            GovAction::SetDirectSaleWallet { address } => address
                .as_ref()
                .map(|a| !a.is_empty() && a.len() <= MAX_DIRECT_SALE_ADDRESS_LEN)
                .unwrap_or(true),
            GovAction::SetDailyMintCap { .. } => true,
            GovAction::SetChannelSessionCost { channel, cost } => {
                !channel.is_empty()
                    && channel.len() <= crate::pallets::nai::MAX_CHANNEL_NAME_LEN
                    && *cost > 0
            }
            GovAction::SetEmissionTarget {
                target_credits_per_hour,
            } => *target_credits_per_hour > 0,
            GovAction::AddPartner {
                pubkey, channels, ..
            } => {
                ed25519_dalek::VerifyingKey::from_bytes(pubkey).is_ok()
                    && crate::pallets::authority::channels_valid(channels)
            }
            GovAction::RemovePartner { .. } => true,
            GovAction::ScheduleUpgrade { version, .. } => *version > 0,
            GovAction::CancelUpgrade => true,
        };
        if ok {
            Ok(())
        } else {
            Err(PalletError::InvalidArgs)
        }
    }

    fn digest_into(&self, h: &mut Sha256) {
        h.update(crate::wire::encode(self));
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProposalStatus {
    Open,
    Enacted { at: u64 },
    Rejected { at: u64 },
    Failed { at: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Proposal {
    pub id: u64,
    pub proposer: PeerId,
    pub action: GovAction,
    pub created_at: u64,
    pub electorate_weight: u64,
    pub votes: BTreeMap<PeerId, (bool, u64)>,
    pub status: ProposalStatus,
}

impl Proposal {
    pub fn voting_closes_at(&self) -> u64 {
        self.created_at.saturating_add(VOTING_PERIOD_BLOCKS)
    }

    pub fn enactable_at(&self) -> u64 {
        self.voting_closes_at()
            .saturating_add(ENACTMENT_DELAY_BLOCKS)
    }

    pub fn tally(&self) -> (u64, u64) {
        self.votes
            .values()
            .fold((0u64, 0u64), |(aye, nay), (vote, w)| {
                if *vote {
                    (aye.saturating_add(*w), nay)
                } else {
                    (aye, nay.saturating_add(*w))
                }
            })
    }

    pub fn passes(&self) -> bool {
        let (aye, nay) = self.tally();
        let participation = aye.saturating_add(nay);
        aye > 0
            && (aye as u128) * (QUORUM_DEN as u128)
                >= (participation as u128) * (QUORUM_NUM as u128)
            && (participation as u128) * 100
                >= (self.electorate_weight as u128) * (PARTICIPATION_FLOOR_PCT as u128)
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GovernanceStorage {
    pub proposals: BTreeMap<u64, Proposal>,
    pub next_id: u64,
    pub oracle_replaced_by_vote: bool,
}

impl GovernanceStorage {
    pub fn open_proposals(&self) -> impl Iterator<Item = &Proposal> {
        self.proposals
            .values()
            .filter(|p| p.status == ProposalStatus::Open)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum GovernanceCall {
    Propose { action: GovAction },
    Vote { id: u64, aye: bool },
    Enact { id: u64 },
}

pub fn voting_weight(contributions: &ContributionsStorage, peer: &PeerId, now_ms: u64) -> u64 {
    match contributions.first_seen_of(peer) {
        Some(first) if now_ms.saturating_sub(first) >= MIN_VOTER_AGE_MS => {
            contributions.maturity_of(peer)
        }
        _ => 0,
    }
}

/// The weight of a wallet and the nodes working for it
/// (`IdentityStorage::group_of`). Each member counts only once old enough,
/// as in `electorate_weight`, so the sum never outweighs the electorate.
pub fn voting_weight_group(
    contributions: &ContributionsStorage,
    peers: &[PeerId],
    now_ms: u64,
) -> u64 {
    peers
        .iter()
        .map(|p| voting_weight(contributions, p, now_ms))
        .fold(0u64, u64::saturating_add)
}

pub fn electorate_weight(contributions: &ContributionsStorage, now_ms: u64) -> u64 {
    let mut peers: Vec<&PeerId> = contributions
        .share_counts
        .keys()
        .chain(contributions.lottery_dimension_by_peer.keys())
        .collect();
    peers.sort_unstable();
    peers.dedup();
    peers
        .into_iter()
        .map(|p| voting_weight(contributions, p, now_ms))
        .fold(0u64, |acc, w| acc.saturating_add(w))
}

pub struct GovernancePallet;

impl GovernancePallet {
    pub fn apply(
        storage: &mut GovernanceStorage,
        contributions: &ContributionsStorage,
        founder_active: bool,
        call: GovernanceCall,
        ctx: &mut ApplyCtx<'_>,
    ) -> Result<Option<(u64, GovAction)>, PalletError> {
        let signer = *ctx.signer;
        match call {
            GovernanceCall::Propose { action } => {
                action.validate()?;
                if founder_active && action.changes_membership() {
                    return Err(PalletError::Unauthorized);
                }
                let group = ctx.identity.group_of(&signer);
                if voting_weight_group(contributions, &group, ctx.timestamp_ms) == 0 {
                    return Err(PalletError::Unauthorized);
                }
                let open: Vec<&Proposal> = storage.open_proposals().collect();
                if open.len() >= MAX_OPEN_PROPOSALS
                    || open.iter().filter(|p| p.proposer == signer).count() >= MAX_OPEN_PER_PROPOSER
                {
                    return Err(PalletError::InvalidArgs);
                }
                let id = storage.next_id;
                storage.next_id = storage.next_id.saturating_add(1);
                storage.proposals.insert(
                    id,
                    Proposal {
                        id,
                        proposer: signer,
                        action,
                        created_at: ctx.block_height,
                        electorate_weight: electorate_weight(contributions, ctx.timestamp_ms),
                        votes: BTreeMap::new(),
                        status: ProposalStatus::Open,
                    },
                );
                Ok(None)
            }
            GovernanceCall::Vote { id, aye } => {
                let group = ctx.identity.group_of(&signer);
                let weight = voting_weight_group(contributions, &group, ctx.timestamp_ms);
                if weight == 0 {
                    return Err(PalletError::Unauthorized);
                }
                let Some(p) = storage.proposals.get_mut(&id) else {
                    return Err(PalletError::InvalidArgs);
                };
                if p.status != ProposalStatus::Open
                    || ctx.block_height >= p.voting_closes_at()
                    || p.votes.contains_key(&signer)
                {
                    return Err(PalletError::InvalidArgs);
                }
                p.votes.insert(signer, (aye, weight));
                Ok(None)
            }
            GovernanceCall::Enact { id } => {
                let Some(p) = storage.proposals.get_mut(&id) else {
                    return Err(PalletError::InvalidArgs);
                };
                if p.status != ProposalStatus::Open || ctx.block_height < p.enactable_at() {
                    return Err(PalletError::InvalidArgs);
                }
                if p.passes() {
                    Ok(Some((id, p.action.clone())))
                } else {
                    p.status = ProposalStatus::Rejected {
                        at: ctx.block_height,
                    };
                    Ok(None)
                }
            }
        }
    }

    pub fn finish(storage: &mut GovernanceStorage, id: u64, height: u64, enacted: bool) {
        if let Some(p) = storage.proposals.get_mut(&id) {
            if enacted && matches!(p.action, GovAction::SetSaleOracle { .. }) {
                storage.oracle_replaced_by_vote = true;
            }
            p.status = if enacted {
                ProposalStatus::Enacted { at: height }
            } else {
                ProposalStatus::Failed { at: height }
            };
        }
    }

    pub fn pallet_root(storage: &GovernanceStorage) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"nepheral/governance/v1");
        h.update(storage.next_id.to_le_bytes());
        h.update([storage.oracle_replaced_by_vote as u8]);
        for p in storage.proposals.values() {
            h.update(p.id.to_le_bytes());
            h.update(p.proposer);
            p.action.digest_into(&mut h);
            h.update(p.created_at.to_le_bytes());
            h.update(p.electorate_weight.to_le_bytes());
            h.update(crate::wire::encode(&p.status));
            h.update((p.votes.len() as u64).to_le_bytes());
            for (voter, (aye, w)) in &p.votes {
                h.update(voter);
                h.update([*aye as u8]);
                h.update(w.to_le_bytes());
            }
        }
        h.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pallets::system::SystemStorage;

    const DAY_MS: u64 = 24 * 60 * 60 * 1_000;

    fn contributions_with(peers: &[(PeerId, u64)]) -> ContributionsStorage {
        let mut c = ContributionsStorage::default();
        for (p, shares) in peers {
            c.share_counts.insert(*p, *shares);
            c.first_seen_ms.insert(*p, 0);
        }
        c
    }

    fn run(
        g: &mut GovernanceStorage,
        c: &ContributionsStorage,
        signer: PeerId,
        height: u64,
        call: GovernanceCall,
    ) -> Result<Option<(u64, GovAction)>, PalletError> {
        let mut sys = SystemStorage::default();
        let mut ctx = ApplyCtx {
            block_height: height,
            timestamp_ms: 30 * DAY_MS,
            author: &signer,
            signer: &signer,
            system: &mut sys,
            current_shares_per_credit: 1_000,
            identity: &crate::pallets::NO_IDENTITY,
            rules_version: crate::pallets::upgrade::GENESIS_RULES_VERSION,
        };
        GovernancePallet::apply(g, c, true, call, &mut ctx)
    }

    #[test]
    fn a_fresh_identity_cannot_propose_or_vote() {
        let mut c = contributions_with(&[([1u8; 32], 100)]);
        c.first_seen_ms.insert([1u8; 32], 29 * DAY_MS);
        let mut g = GovernanceStorage::default();
        let r = run(
            &mut g,
            &c,
            [1u8; 32],
            1,
            GovernanceCall::Propose {
                action: GovAction::SetDailyMintCap { cap: 1 },
            },
        );
        assert_eq!(r, Err(PalletError::Unauthorized));
        let r = run(
            &mut g,
            &c,
            [9u8; 32],
            1,
            GovernanceCall::Propose {
                action: GovAction::SetDailyMintCap { cap: 1 },
            },
        );
        assert_eq!(r, Err(PalletError::Unauthorized));
    }

    #[test]
    fn a_proposal_passes_with_quorum_only_after_the_delay() {
        let c = contributions_with(&[([1u8; 32], 60), ([2u8; 32], 30), ([3u8; 32], 10)]);
        let mut g = GovernanceStorage::default();
        let action = GovAction::SetEmissionTarget {
            target_credits_per_hour: 500,
        };
        run(
            &mut g,
            &c,
            [1u8; 32],
            10,
            GovernanceCall::Propose {
                action: action.clone(),
            },
        )
        .unwrap();
        run(
            &mut g,
            &c,
            [1u8; 32],
            11,
            GovernanceCall::Vote { id: 0, aye: true },
        )
        .unwrap();
        run(
            &mut g,
            &c,
            [2u8; 32],
            12,
            GovernanceCall::Vote { id: 0, aye: false },
        )
        .unwrap();
        assert!(
            run(
                &mut g,
                &c,
                [2u8; 32],
                13,
                GovernanceCall::Vote { id: 0, aye: true }
            )
            .is_err(),
            "one vote per voter"
        );
        let early = run(
            &mut g,
            &c,
            [3u8; 32],
            10 + VOTING_PERIOD_BLOCKS,
            GovernanceCall::Enact { id: 0 },
        );
        assert!(early.is_err(), "the enactment delay is enforced");
        assert!(
            run(
                &mut g,
                &c,
                [3u8; 32],
                10 + VOTING_PERIOD_BLOCKS,
                GovernanceCall::Vote { id: 0, aye: true }
            )
            .is_err(),
            "voting is closed after the period"
        );
        let out = run(
            &mut g,
            &c,
            [3u8; 32],
            10 + VOTING_PERIOD_BLOCKS + ENACTMENT_DELAY_BLOCKS,
            GovernanceCall::Enact { id: 0 },
        )
        .unwrap();
        assert_eq!(out, Some((0, action)), "60 of 90 is two thirds");
    }

    #[test]
    fn a_proposal_without_participation_is_rejected() {
        let c = contributions_with(&[([1u8; 32], 10), ([2u8; 32], 90)]);
        let mut g = GovernanceStorage::default();
        run(
            &mut g,
            &c,
            [1u8; 32],
            1,
            GovernanceCall::Propose {
                action: GovAction::SetDailyMintCap { cap: 1 },
            },
        )
        .unwrap();
        run(
            &mut g,
            &c,
            [1u8; 32],
            2,
            GovernanceCall::Vote { id: 0, aye: true },
        )
        .unwrap();
        let out = run(
            &mut g,
            &c,
            [2u8; 32],
            1 + VOTING_PERIOD_BLOCKS + ENACTMENT_DELAY_BLOCKS,
            GovernanceCall::Enact { id: 0 },
        )
        .unwrap();
        assert_eq!(out, None, "10% participation is below the floor");
        assert!(matches!(
            g.proposals[&0].status,
            ProposalStatus::Rejected { .. }
        ));
    }

    #[test]
    fn membership_is_the_founders_while_the_founder_is_active() {
        let c = contributions_with(&[([1u8; 32], 10)]);
        let mut g = GovernanceStorage::default();
        let r = run(
            &mut g,
            &c,
            [1u8; 32],
            1,
            GovernanceCall::Propose {
                action: GovAction::RemovePartner { pubkey: [2u8; 32] },
            },
        );
        assert_eq!(r, Err(PalletError::Unauthorized));
    }

    #[test]
    fn proposals_are_bounded_per_proposer() {
        let c = contributions_with(&[([1u8; 32], 10)]);
        let mut g = GovernanceStorage::default();
        for cap in 0..MAX_OPEN_PER_PROPOSER as u64 {
            run(
                &mut g,
                &c,
                [1u8; 32],
                1,
                GovernanceCall::Propose {
                    action: GovAction::SetDailyMintCap { cap },
                },
            )
            .unwrap();
        }
        assert!(run(
            &mut g,
            &c,
            [1u8; 32],
            1,
            GovernanceCall::Propose {
                action: GovAction::SetDailyMintCap { cap: 99 },
            },
        )
        .is_err());
    }
}
