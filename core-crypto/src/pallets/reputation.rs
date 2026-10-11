// SPDX-License-Identifier: BUSL-1.1
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::contributions::ContributionsStorage;
use super::{ApplyCtx, PalletError};
use crate::identity::PeerId;

pub const PALLET_IDX: u8 = 9;

pub const RATING_WINDOW_BLOCKS: u64 = 43_200;

pub const EPOCH_BLOCKS: u64 = 43_200;

pub const EPOCHS_KEPT: u64 = 8;

pub const DISLIKE_WEIGHT: u64 = 2;

pub const MAX_RATER_WEIGHT: u64 = 32;

pub const PRIOR_WEIGHT: u64 = 20;

pub const MIN_NOTE_CHARS: usize = 8;

pub const MAX_NOTE_CHARS: usize = 280;

pub const MIN_EVIDENCE_TO_EXCLUDE: u64 = 40;

pub const EXCLUDE_BELOW_QUALITY: u64 = 25;

pub const MAX_ROUTING_WEIGHT: u8 = 100;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum DislikeReason {
    Incorrect,
    Harmful,
    OffTopic,
    Incomplete,
    Refused,
}

impl DislikeReason {
    pub const ALL: [DislikeReason; 5] = [
        DislikeReason::Incorrect,
        DislikeReason::Harmful,
        DislikeReason::OffTopic,
        DislikeReason::Incomplete,
        DislikeReason::Refused,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DislikeReason::Incorrect => "incorrect",
            DislikeReason::Harmful => "harmful",
            DislikeReason::OffTopic => "off_topic",
            DislikeReason::Incomplete => "incomplete",
            DislikeReason::Refused => "refused",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Verdict {
    Like,
    Dislike { reason: DislikeReason, note: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReputationCall {
    Rate {
        query_digest: [u8; 32],
        verdict: Verdict,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rateable {
    pub sender: PeerId,
    pub responder: PeerId,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Score {
    pub epochs: BTreeMap<u64, (u64, u64)>,
    pub reasons: [u64; 5],
    #[serde(default)]
    pub raters: BTreeMap<u64, BTreeSet<PeerId>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quality {
    pub percent: u64,
    pub likes: u64,
    pub dislikes: u64,
}

impl Quality {
    pub fn evidence(&self) -> u64 {
        self.likes.saturating_add(self.dislikes)
    }

    pub fn excluded(&self) -> bool {
        self.evidence() >= MIN_EVIDENCE_TO_EXCLUDE && self.percent < EXCLUDE_BELOW_QUALITY
    }

    pub fn routing_weight(&self) -> u8 {
        self.percent.clamp(1, MAX_ROUTING_WEIGHT as u64) as u8
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ReputationStorage {
    pub rateable: BTreeMap<[u8; 32], Rateable>,
    pub expiry: BTreeSet<(u64, [u8; 32])>,
    pub scores: BTreeMap<PeerId, Score>,
}

impl ReputationStorage {
    pub fn register(
        &mut self,
        query_digest: [u8; 32],
        sender: PeerId,
        responder: PeerId,
        height: u64,
    ) {
        let expires_at = height.saturating_add(RATING_WINDOW_BLOCKS);
        if self.rateable.contains_key(&query_digest) {
            return;
        }
        self.rateable.insert(
            query_digest,
            Rateable {
                sender,
                responder,
                expires_at,
            },
        );
        self.expiry.insert((expires_at, query_digest));
    }

    pub fn prune(&mut self, height: u64) {
        while let Some(&(at, digest)) = self.expiry.first() {
            if at > height {
                break;
            }
            self.expiry.pop_first();
            self.rateable.remove(&digest);
        }
        let current = height / EPOCH_BLOCKS;
        let oldest = current.saturating_sub(EPOCHS_KEPT - 1);
        self.scores.retain(|_, s| {
            s.epochs.retain(|e, _| *e >= oldest);
            s.raters.retain(|e, _| *e >= oldest);
            !s.epochs.is_empty()
        });
    }

    pub fn quality(&self, peer: &PeerId, height: u64) -> Quality {
        let current = height / EPOCH_BLOCKS;
        let (likes, dislikes) = self
            .scores
            .get(peer)
            .map(|s| {
                s.epochs.iter().fold((0u64, 0u64), |(l, d), (e, (el, ed))| {
                    let age = current.saturating_sub(*e);
                    if age >= EPOCHS_KEPT {
                        (l, d)
                    } else {
                        (l.saturating_add(el >> age), d.saturating_add(ed >> age))
                    }
                })
            })
            .unwrap_or((0, 0));
        let weighted_dislikes = dislikes.saturating_mul(DISLIKE_WEIGHT);
        let percent = (likes.saturating_add(PRIOR_WEIGHT) as u128 * 100
            / (likes
                .saturating_add(weighted_dislikes)
                .saturating_add(2 * PRIOR_WEIGHT)) as u128) as u64;
        Quality {
            percent,
            likes,
            dislikes,
        }
    }

    pub fn reasons_of(&self, peer: &PeerId) -> [u64; 5] {
        self.scores.get(peer).map(|s| s.reasons).unwrap_or([0; 5])
    }
}

pub fn rater_weight(contributions: &ContributionsStorage, rater: &PeerId) -> u64 {
    let maturity = contributions.maturity_of(rater);
    (maturity.isqrt()).clamp(1, MAX_RATER_WEIGHT)
}

pub struct ReputationPallet;

impl ReputationPallet {
    pub fn apply(
        storage: &mut ReputationStorage,
        contributions: &ContributionsStorage,
        call: ReputationCall,
        ctx: &mut ApplyCtx<'_>,
    ) -> Result<(), PalletError> {
        match call {
            ReputationCall::Rate {
                query_digest,
                verdict,
            } => {
                let Some(entry) = storage.rateable.get(&query_digest).copied() else {
                    return Err(PalletError::InvalidArgs);
                };
                if entry.sender != *ctx.signer {
                    return Err(PalletError::Unauthorized);
                }
                if entry.expires_at <= ctx.block_height {
                    return Err(PalletError::InvalidArgs);
                }
                if let Verdict::Dislike { note, .. } = &verdict {
                    let chars = note.trim().chars().count();
                    if !(MIN_NOTE_CHARS..=MAX_NOTE_CHARS).contains(&chars) {
                        return Err(PalletError::InvalidArgs);
                    }
                }
                let weight = rater_weight(contributions, ctx.signer);
                let epoch = ctx.block_height / EPOCH_BLOCKS;
                let score = storage.scores.entry(entry.responder).or_default();
                let counted = score.raters.entry(epoch).or_default().insert(*ctx.signer);
                let bucket = score.epochs.entry(epoch).or_insert((0, 0));
                match verdict {
                    _ if !counted => {}
                    Verdict::Like => bucket.0 = bucket.0.saturating_add(weight),
                    Verdict::Dislike { reason, .. } => {
                        bucket.1 = bucket.1.saturating_add(weight);
                        score.reasons[reason.index()] =
                            score.reasons[reason.index()].saturating_add(1);
                    }
                }
                storage.rateable.remove(&query_digest);
                storage.expiry.remove(&(entry.expires_at, query_digest));
                Ok(())
            }
        }
    }

    pub fn pallet_root(storage: &ReputationStorage) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"nepheral/reputation/v1");
        h.update((storage.rateable.len() as u64).to_le_bytes());
        for (digest, r) in &storage.rateable {
            h.update(digest);
            h.update(r.sender);
            h.update(r.responder);
            h.update(r.expires_at.to_le_bytes());
        }
        h.update((storage.scores.len() as u64).to_le_bytes());
        for (peer, score) in &storage.scores {
            h.update(peer);
            h.update((score.epochs.len() as u64).to_le_bytes());
            for (e, (l, d)) in &score.epochs {
                h.update(e.to_le_bytes());
                h.update(l.to_le_bytes());
                h.update(d.to_le_bytes());
            }
            for r in score.reasons {
                h.update(r.to_le_bytes());
            }
            for (e, raters) in &score.raters {
                h.update(e.to_le_bytes());
                for r in raters {
                    h.update(r);
                }
            }
        }
        h.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pallets::system::SystemStorage;

    fn rate(
        storage: &mut ReputationStorage,
        contributions: &ContributionsStorage,
        signer: PeerId,
        height: u64,
        digest: [u8; 32],
        verdict: Verdict,
    ) -> Result<(), PalletError> {
        let mut sys = SystemStorage::default();
        let mut ctx = ApplyCtx {
            block_height: height,
            timestamp_ms: 0,
            author: &signer,
            signer: &signer,
            system: &mut sys,
            current_shares_per_credit: 1_000,
            identity: &crate::pallets::NO_IDENTITY,
            rules_version: crate::pallets::upgrade::GENESIS_RULES_VERSION,
        };
        ReputationPallet::apply(
            storage,
            contributions,
            ReputationCall::Rate {
                query_digest: digest,
                verdict,
            },
            &mut ctx,
        )
    }

    fn dislike(note: &str) -> Verdict {
        Verdict::Dislike {
            reason: DislikeReason::Incorrect,
            note: note.to_string(),
        }
    }

    #[test]
    fn only_the_sender_of_a_receipt_can_rate_it_once() {
        let c = ContributionsStorage::default();
        let mut r = ReputationStorage::default();
        r.register([1u8; 32], [10u8; 32], [20u8; 32], 100);
        assert_eq!(
            rate(&mut r, &c, [11u8; 32], 101, [1u8; 32], Verdict::Like),
            Err(PalletError::Unauthorized)
        );
        rate(&mut r, &c, [10u8; 32], 101, [1u8; 32], Verdict::Like).unwrap();
        assert!(rate(&mut r, &c, [10u8; 32], 102, [1u8; 32], Verdict::Like).is_err());
        assert!(rate(&mut r, &c, [10u8; 32], 102, [9u8; 32], Verdict::Like).is_err());
        assert_eq!(r.quality(&[20u8; 32], 102).likes, 1);
    }

    #[test]
    fn a_dislike_needs_a_justification() {
        let c = ContributionsStorage::default();
        let mut r = ReputationStorage::default();
        r.register([1u8; 32], [10u8; 32], [20u8; 32], 0);
        assert!(rate(&mut r, &c, [10u8; 32], 1, [1u8; 32], dislike("bad")).is_err());
        rate(
            &mut r,
            &c,
            [10u8; 32],
            1,
            [1u8; 32],
            dislike("the date it gave is wrong"),
        )
        .unwrap();
        assert_eq!(r.reasons_of(&[20u8; 32])[0], 1);
    }

    #[test]
    fn ratings_expire_with_the_window() {
        let c = ContributionsStorage::default();
        let mut r = ReputationStorage::default();
        r.register([1u8; 32], [10u8; 32], [20u8; 32], 0);
        r.prune(RATING_WINDOW_BLOCKS);
        assert!(r.rateable.is_empty());
        assert!(rate(
            &mut r,
            &c,
            [10u8; 32],
            RATING_WINDOW_BLOCKS,
            [1u8; 32],
            Verdict::Like
        )
        .is_err());
    }

    #[test]
    fn dislikes_pull_quality_down_harder_than_likes_push_it_up_and_decay() {
        let mut c = ContributionsStorage::default();
        let raters: Vec<PeerId> = (10..14u8).map(|i| [i; 32]).collect();
        let mut r = ReputationStorage::default();
        for (i, rater) in raters.iter().enumerate() {
            c.share_counts.insert(*rater, 1_024);
            r.register([i as u8; 32], *rater, [20u8; 32], 0);
        }
        let verdicts = [
            Verdict::Like,
            dislike("made up a citation"),
            dislike("ignored the question"),
            dislike("answer was cut short"),
        ];
        for (i, v) in verdicts.into_iter().enumerate() {
            rate(&mut r, &c, raters[i], 1, [i as u8; 32], v).unwrap();
            if i == 1 {
                let q = r.quality(&[20u8; 32], 1);
                assert!(
                    q.percent < 50,
                    "one like and one dislike lean negative: {}",
                    q.percent
                );
            }
        }
        let bad = r.quality(&[20u8; 32], 1);
        assert!(bad.excluded(), "{bad:?}");
        let later = r.quality(&[20u8; 32], 3 * EPOCH_BLOCKS);
        assert!(later.percent > bad.percent, "old ratings fade");
        assert_eq!(
            r.quality(&[99u8; 32], 1).percent,
            50,
            "unknown nodes start neutral"
        );
    }

    #[test]
    fn one_rater_counts_once_per_responder_per_day() {
        let mut c = ContributionsStorage::default();
        let rater = [10u8; 32];
        c.share_counts.insert(rater, 1_024);
        let mut r = ReputationStorage::default();
        for i in 0..5u8 {
            r.register([i; 32], rater, [20u8; 32], u64::from(i) * 10);
        }
        for i in 0..4u8 {
            rate(&mut r, &c, rater, 1, [i; 32], Verdict::Like).unwrap();
        }
        assert_eq!(
            r.quality(&[20u8; 32], 1).likes,
            32,
            "a ring cannot pile up likes"
        );
        rate(&mut r, &c, rater, EPOCH_BLOCKS, [4u8; 32], Verdict::Like).unwrap();
        assert_eq!(r.quality(&[20u8; 32], EPOCH_BLOCKS).likes, 16 + 32);
    }
}
