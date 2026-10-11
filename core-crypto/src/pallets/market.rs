// SPDX-License-Identifier: BUSL-1.1
//! A notice board for NAI, with the NAI side held in escrow by the chain.
//!
//! Sellers post an ask and the credits leave their balance immediately, so a
//! listing is always covered: a buyer can see that what is advertised actually
//! exists. Buyers post a bid, which is only an advertisement — the chain
//! cannot escrow Litecoin, and pretending otherwise would be a promise it
//! could not keep.
//!
//! Settlement is deliberately one-directional. The Litecoin leg is observed
//! off-chain by the sale oracle (a separate service), which is the only key
//! that may call `Settle`. Payment goes from buyer to seller directly; the
//! network never holds, forwards or custodies Litecoin at any point.
use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::balances::BalancesStorage;
use super::{ApplyCtx, Pallet, PalletError};
use crate::identity::PeerId;

/// Caps how much of the board one account can occupy. Posting an ask costs
/// escrow, so asks are self-limiting; bids are free, hence the same cap does
/// more work there.
pub const MAX_OPEN_PER_ACCOUNT: usize = 5;

pub const MAX_LTC_ADDRESS_LEN: usize = 90;

/// A Litecoin txid is 64 hex characters; the cap leaves room without letting
/// an unbounded string into chain state.
pub const MAX_PAYMENT_REF_LEN: usize = 80;

/// How long to wait before each retry of a claim the oracle could not confirm.
///
/// A first look that finds nothing usually means the payment is real but not
/// yet deep enough, or that one explorer is lagging — both resolve with time.
/// The widening gaps give a slow confirmation room to land without holding a
/// verification slot open indefinitely.
pub const CLAIM_RETRY_BACKOFF_MS: [u64; 3] = [
    10 * 60 * 1_000, // 10 minutes
    30 * 60 * 1_000, // 30 minutes
    60 * 60 * 1_000, // 1 hour
];

/// The first look plus one per backoff step.
pub const MAX_CLAIM_ATTEMPTS: u8 = 1 + CLAIM_RETRY_BACKOFF_MS.len() as u8;

/// Both sides of a listing must be economically meaningful. One litoshi for
/// one credit is legal; zero on either side is not an offer.
pub const MIN_CREDITS: u64 = 1;
pub const MIN_SATS: u64 = 1;

/// Days of recorded contribution before an account may use the board.
///
/// Fresh keys are free to mint, so an identity with no history behind it is
/// not an identity. Tenure is counted from a peer's first contribution rather
/// than from when its key first appeared: waiting costs an attacker nothing,
/// contributing for five days does not.
pub const MIN_DAYS_ACTIVE: u64 = 5;

/// Shares contributed before an account may use the board. A node serving the
/// network casually reaches this in roughly three days, so it gates identities
/// that did nothing, not small nodes.
pub const MIN_SHARES: u64 = 2_000;

/// Lottery wins before an account may use the board. One is enough to show
/// the node actually answered the network rather than only relaying for it.
pub const MIN_WINS: u64 = 1;

/// Why an account may not trade yet. Carries the numbers so the interface can
/// tell someone how far off they are instead of only that they were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ineligible {
    TooNew { days_active: u64, required: u64 },
    TooFewShares { have: u64, required: u64 },
    TooFewWins { have: u64, required: u64 },
}

impl std::fmt::Display for Ineligible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooNew {
                days_active,
                required,
            } => write!(
                f,
                "active for {days_active} of the {required} days required to trade"
            ),
            Self::TooFewShares { have, required } => {
                write!(f, "{have} of the {required} shares required to trade")
            }
            Self::TooFewWins { have, required } => {
                write!(f, "{have} of the {required} lottery wins required to trade")
            }
        }
    }
}

/// Whether `peer` has enough history behind it to use the board.
///
/// Buying directly from the network's own sale address is deliberately not
/// gated by this: someone with no history has to be able to acquire credits
/// somehow, and that path has a counterparty who can refuse. Trading with
/// other participants is what needs the account to have a past.
pub fn check_eligible(
    contributions: &super::contributions::ContributionsStorage,
    peer: &PeerId,
    now_ms: u64,
) -> Result<(), Ineligible> {
    check_eligible_group(contributions, std::slice::from_ref(peer), now_ms)
}

/// The same bar for a wallet and the nodes working for it
/// (`IdentityStorage::group_of`): the oldest of them sets how long the
/// account has been around, and their work adds up. An empty group - a
/// delegated node, whose standing is its wallet's - does not qualify.
pub fn check_eligible_group(
    contributions: &super::contributions::ContributionsStorage,
    peers: &[PeerId],
    now_ms: u64,
) -> Result<(), Ineligible> {
    let days_active = peers
        .iter()
        .map(|p| contributions.days_active(p, now_ms))
        .max()
        .unwrap_or(0);
    if days_active < MIN_DAYS_ACTIVE {
        return Err(Ineligible::TooNew {
            days_active,
            required: MIN_DAYS_ACTIVE,
        });
    }
    let shares = peers
        .iter()
        .map(|p| contributions.maturity_of(p))
        .fold(0u64, u64::saturating_add);
    if shares < MIN_SHARES {
        return Err(Ineligible::TooFewShares {
            have: shares,
            required: MIN_SHARES,
        });
    }
    let wins = peers
        .iter()
        .map(|p| contributions.wins_of(p))
        .fold(0u64, u64::saturating_add);
    if wins < MIN_WINS {
        return Err(Ineligible::TooFewWins {
            have: wins,
            required: MIN_WINS,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ask {
    pub id: u64,
    pub seller: PeerId,
    /// Credits already deducted from the seller and held by this pallet.
    pub credits: u64,
    /// What the seller wants for the whole lot, in litoshi.
    pub ask_sats: u64,
    /// Where the buyer pays. The chain never reads this; it is shown to
    /// buyers and used by the oracle to attribute a payment.
    pub ltc_address: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bid {
    pub id: u64,
    pub buyer: PeerId,
    pub credits: u64,
    pub bid_sats: u64,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fill {
    pub ask_id: u64,
    pub seller: PeerId,
    pub buyer: PeerId,
    pub credits: u64,
    pub paid_sats: u64,
    pub settled_at_ms: u64,
}

/// A buyer's assertion that they paid for an ask, waiting on the oracle to
/// confirm it against the Litecoin chain.
///
/// The claim is what bounds the oracle: it may only confirm a payment someone
/// already named on chain, never move an escrow to a party of its own
/// choosing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Claim {
    pub ask_id: u64,
    /// Who the credits go to. The oracle checks this against the payment's
    /// OP_RETURN, so a seller claiming on a buyer's behalf cannot redirect
    /// them — only speed them up.
    pub buyer: PeerId,
    pub claimed_by: PeerId,
    pub claimed_at_ms: u64,
    /// Verifications spent so far. A claim is never taken as infallible: the
    /// payment may not be deep enough yet, or an explorer may be lagging.
    #[serde(default)]
    pub attempts: u8,
    /// Earliest the oracle may report another failure. Enforced on chain so a
    /// misbehaving oracle cannot burn through every attempt in one block and
    /// strand a payment that would have confirmed.
    #[serde(default)]
    pub next_attempt_at_ms: u64,
}

impl Claim {
    /// Attempts left before the claim is abandoned.
    pub fn attempts_remaining(&self) -> u8 {
        MAX_CLAIM_ATTEMPTS.saturating_sub(self.attempts)
    }

    /// Whether the oracle may look again yet.
    pub fn is_due(&self, now_ms: u64) -> bool {
        now_ms >= self.next_attempt_at_ms
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MarketStorage {
    pub next_id: u64,
    pub asks: BTreeMap<u64, Ask>,
    pub bids: BTreeMap<u64, Bid>,
    /// Credits this pallet is holding across all open asks. Equal to the sum
    /// of `asks[*].credits`; kept explicitly so the invariant is checkable
    /// without walking the board.
    pub escrowed: u64,
    /// Litecoin txid -> the ask it settled, so a replayed payment reference
    /// cannot drain a second listing.
    pub fills: HashMap<String, Fill>,
    /// Litecoin txid -> the claim awaiting confirmation. Keyed by payment so
    /// one payment can only ever be claimed against one ask.
    #[serde(default)]
    pub claims: BTreeMap<String, Claim>,
}

impl MarketStorage {
    pub fn open_asks_of(&self, seller: &PeerId) -> usize {
        self.asks.values().filter(|a| &a.seller == seller).count()
    }

    pub fn open_bids_of(&self, buyer: &PeerId) -> usize {
        self.bids.values().filter(|b| &b.buyer == buyer).count()
    }

    /// Total credits held in escrow, recomputed from the board. Used by tests
    /// and by anyone auditing `escrowed`.
    pub fn escrow_sum(&self) -> u64 {
        self.asks.values().map(|a| a.credits).sum()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum MarketCall {
    /// Offer `credits` for `ask_sats`. The credits are escrowed at once.
    PostAsk {
        credits: u64,
        ask_sats: u64,
        ltc_address: String,
    },
    /// Withdraw an ask; the escrow returns to the seller.
    CancelAsk {
        id: u64,
    },
    /// Advertise interest in buying. Nothing is escrowed.
    PostBid {
        credits: u64,
        bid_sats: u64,
    },
    CancelBid {
        id: u64,
    },
    /// Assert that `payment_ref` paid this ask, and that `buyer` should
    /// receive the credits. Submitted by the buyer, or by the seller on the
    /// buyer's behalf so a payment is not stranded when the buyer goes away.
    /// Confirms nothing on its own — it tells the oracle where to look.
    ClaimFill {
        ask_id: u64,
        buyer: PeerId,
        payment_ref: String,
    },
    /// Report that a claim could not be confirmed. Sale-oracle only.
    ///
    /// `permanent` is for what retrying cannot fix — the payment names a
    /// different buyer, or pays a different address — and drops the claim at
    /// once. Everything else is treated as "not yet" and schedules another
    /// look, until the attempts run out.
    FailClaim {
        payment_ref: String,
        permanent: bool,
    },
    /// Release an ask's escrow to the buyer named in the matching claim.
    /// Sale-oracle only: it asserts that the payment is real, on chain, and
    /// carries that buyer.
    Settle {
        id: u64,
        buyer: PeerId,
        payment_ref: String,
    },
}

pub struct MarketPallet;

/// An address inside Litecoin's private extension (MWEB).
pub fn is_private_ltc_address(address: &str) -> bool {
    let a = address.trim().to_ascii_lowercase();
    a.starts_with("ltcmweb1") || a.starts_with("tmweb1")
}

impl MarketPallet {
    pub fn apply_with_balances(
        storage: &mut MarketStorage,
        balances: &mut BalancesStorage,
        contributions: &super::contributions::ContributionsStorage,
        call: MarketCall,
        ctx: &mut ApplyCtx<'_>,
    ) -> Result<(), PalletError> {
        // Everything a participant initiates needs standing. `Settle` is the
        // oracle's and is checked by the chain's authority gate instead.
        let gated_peer = match &call {
            MarketCall::PostAsk { .. } | MarketCall::CancelAsk { .. } => Some(*ctx.signer),
            MarketCall::PostBid { .. } | MarketCall::CancelBid { .. } => Some(*ctx.signer),
            // The buyer is the one acquiring credits, so the buyer is who must
            // qualify — whichever party actually submits the claim.
            MarketCall::ClaimFill { buyer, .. } => Some(*buyer),
            MarketCall::FailClaim { .. } | MarketCall::Settle { .. } => None,
        };
        if let Some(peer) = gated_peer {
            // Withdrawing is always allowed: an account that fell below the
            // bar must still be able to take its own escrow back.
            let withdrawing = matches!(
                call,
                MarketCall::CancelAsk { .. } | MarketCall::CancelBid { .. }
            );
            let group = ctx.identity.group_of(&peer);
            if !withdrawing
                && check_eligible_group(contributions, &group, ctx.timestamp_ms).is_err()
            {
                return Err(PalletError::Unauthorized);
            }
        }

        match call {
            MarketCall::PostAsk {
                credits,
                ask_sats,
                ltc_address,
            } => {
                if credits < MIN_CREDITS || ask_sats < MIN_SATS {
                    return Err(PalletError::InvalidArgs);
                }
                if ltc_address.is_empty() || ltc_address.len() > MAX_LTC_ADDRESS_LEN {
                    return Err(PalletError::InvalidArgs);
                }
                // A payment to an MWEB address is invisible to every explorer,
                // so the listing could never settle. The clients refused these
                // already; from v2 it is a rule.
                if ctx.rules_version >= crate::pallets::upgrade::RULES_V2
                    && is_private_ltc_address(&ltc_address)
                {
                    return Err(PalletError::InvalidArgs);
                }
                if storage.open_asks_of(ctx.signer) >= MAX_OPEN_PER_ACCOUNT {
                    return Err(PalletError::InvalidArgs);
                }
                // Escrow first: an ask that cannot be covered is never posted.
                balances.burn_checked(ctx.signer, credits)?;
                let id = storage.next_id;
                storage.next_id = storage.next_id.saturating_add(1);
                storage.escrowed = storage.escrowed.saturating_add(credits);
                storage.asks.insert(
                    id,
                    Ask {
                        id,
                        seller: *ctx.signer,
                        credits,
                        ask_sats,
                        ltc_address,
                        created_at_ms: ctx.timestamp_ms,
                    },
                );
                Ok(())
            }

            MarketCall::CancelAsk { id } => {
                let ask = storage.asks.get(&id).ok_or(PalletError::InvalidArgs)?;
                if &ask.seller != ctx.signer {
                    return Err(PalletError::Unauthorized);
                }
                if storage.claims.values().any(|c| c.ask_id == id) {
                    return Err(PalletError::Unauthorized);
                }
                let Some(ask) = storage.asks.remove(&id) else {
                    return Err(PalletError::InvalidArgs);
                };
                storage.escrowed = storage.escrowed.saturating_sub(ask.credits);
                balances.mint(ask.seller, ask.credits);
                Ok(())
            }

            MarketCall::PostBid { credits, bid_sats } => {
                if credits < MIN_CREDITS || bid_sats < MIN_SATS {
                    return Err(PalletError::InvalidArgs);
                }
                if storage.open_bids_of(ctx.signer) >= MAX_OPEN_PER_ACCOUNT {
                    return Err(PalletError::InvalidArgs);
                }
                let id = storage.next_id;
                storage.next_id = storage.next_id.saturating_add(1);
                storage.bids.insert(
                    id,
                    Bid {
                        id,
                        buyer: *ctx.signer,
                        credits,
                        bid_sats,
                        created_at_ms: ctx.timestamp_ms,
                    },
                );
                Ok(())
            }

            MarketCall::CancelBid { id } => {
                let bid = storage.bids.get(&id).ok_or(PalletError::InvalidArgs)?;
                if &bid.buyer != ctx.signer {
                    return Err(PalletError::Unauthorized);
                }
                storage.bids.remove(&id);
                Ok(())
            }

            MarketCall::ClaimFill {
                ask_id,
                buyer,
                payment_ref,
            } => {
                if payment_ref.is_empty() || payment_ref.len() > MAX_PAYMENT_REF_LEN {
                    return Err(PalletError::InvalidArgs);
                }
                if storage.fills.contains_key(&payment_ref)
                    || storage.claims.contains_key(&payment_ref)
                {
                    return Err(PalletError::Unauthorized);
                }
                let ask = storage.asks.get(&ask_id).ok_or(PalletError::InvalidArgs)?;
                if ask.seller == buyer {
                    return Err(PalletError::InvalidArgs);
                }
                if storage.claims.values().any(|c| c.ask_id == ask_id) {
                    return Err(PalletError::Unauthorized);
                }
                // Only the two parties to the trade may raise a claim. A
                // bystander naming themselves would otherwise send the oracle
                // to check a payment that has nothing to do with them.
                if ctx.signer != &buyer && ctx.signer != &ask.seller {
                    return Err(PalletError::Unauthorized);
                }
                storage.claims.insert(
                    payment_ref,
                    Claim {
                        ask_id,
                        buyer,
                        claimed_by: *ctx.signer,
                        claimed_at_ms: ctx.timestamp_ms,
                        attempts: 0,
                        next_attempt_at_ms: ctx.timestamp_ms,
                    },
                );
                Ok(())
            }

            MarketCall::FailClaim {
                payment_ref,
                permanent,
            } => {
                let claim = storage
                    .claims
                    .get_mut(&payment_ref)
                    .ok_or(PalletError::InvalidArgs)?;

                // Abandoning a claim frees its payment reference, so whoever
                // paid can claim again once the payment settles. Dropping it
                // is never a loss of funds: the escrow stays with the seller
                // and the Litecoin stays with whoever received it.
                if permanent {
                    storage.claims.remove(&payment_ref);
                    return Ok(());
                }

                if !claim.is_due(ctx.timestamp_ms) {
                    return Err(PalletError::Unauthorized);
                }

                claim.attempts = claim.attempts.saturating_add(1);
                if claim.attempts >= MAX_CLAIM_ATTEMPTS {
                    storage.claims.remove(&payment_ref);
                    return Ok(());
                }

                let step = (claim.attempts as usize).saturating_sub(1);
                let backoff = CLAIM_RETRY_BACKOFF_MS
                    .get(step)
                    .copied()
                    .unwrap_or(*CLAIM_RETRY_BACKOFF_MS.last().expect("non-empty"));
                claim.next_attempt_at_ms = ctx.timestamp_ms.saturating_add(backoff);
                Ok(())
            }

            MarketCall::Settle {
                id,
                buyer,
                payment_ref,
            } => {
                if payment_ref.is_empty() {
                    return Err(PalletError::InvalidArgs);
                }
                // One payment settles one ask, whatever the oracle says.
                if storage.fills.contains_key(&payment_ref) {
                    return Err(PalletError::Unauthorized);
                }
                // The oracle confirms claims; it does not originate transfers.
                // Without this the oracle key alone could move any escrow to
                // any account, which is far more power than observing a
                // payment warrants.
                let claim = storage
                    .claims
                    .get(&payment_ref)
                    .ok_or(PalletError::Unauthorized)?;
                if claim.ask_id != id || claim.buyer != buyer {
                    return Err(PalletError::Unauthorized);
                }
                let ask = storage.asks.get(&id).ok_or(PalletError::InvalidArgs)?;
                if ask.seller == buyer {
                    return Err(PalletError::InvalidArgs);
                }
                let Some(ask) = storage.asks.remove(&id) else {
                    return Err(PalletError::InvalidArgs);
                };
                storage.escrowed = storage.escrowed.saturating_sub(ask.credits);
                balances.mint(buyer, ask.credits);
                storage.claims.retain(|_, c| c.ask_id != id);
                storage.fills.insert(
                    payment_ref,
                    Fill {
                        ask_id: ask.id,
                        seller: ask.seller,
                        buyer,
                        credits: ask.credits,
                        paid_sats: ask.ask_sats,
                        settled_at_ms: ctx.timestamp_ms,
                    },
                );
                Ok(())
            }
        }
    }
}

impl Pallet for MarketPallet {
    type Storage = MarketStorage;
    type Call = MarketCall;
    const PALLET_IDX: u8 = 7;

    fn apply(
        _storage: &mut Self::Storage,
        _call: Self::Call,
        _ctx: &mut ApplyCtx<'_>,
    ) -> Result<(), PalletError> {
        // Every call moves credits, so the pallet is only reachable through
        // `apply_with_balances`.
        Err(PalletError::InvalidArgs)
    }

    fn pallet_root(storage: &Self::Storage) -> [u8; 32] {
        let mut h = Sha256::new();
        const SEP: &[u8] = b"||";

        h.update(storage.next_id.to_le_bytes());
        h.update(storage.escrowed.to_le_bytes());
        h.update(SEP);

        // BTreeMap iterates in key order, so no sort is needed here.
        for (id, a) in &storage.asks {
            h.update(id.to_le_bytes());
            h.update(a.seller);
            h.update(a.credits.to_le_bytes());
            h.update(a.ask_sats.to_le_bytes());
            h.update((a.ltc_address.len() as u32).to_le_bytes());
            h.update(a.ltc_address.as_bytes());
            h.update(a.created_at_ms.to_le_bytes());
        }
        h.update(SEP);

        for (id, b) in &storage.bids {
            h.update(id.to_le_bytes());
            h.update(b.buyer);
            h.update(b.credits.to_le_bytes());
            h.update(b.bid_sats.to_le_bytes());
            h.update(b.created_at_ms.to_le_bytes());
        }
        h.update(SEP);

        for (payment_ref, c) in &storage.claims {
            h.update((payment_ref.len() as u32).to_le_bytes());
            h.update(payment_ref.as_bytes());
            h.update(c.ask_id.to_le_bytes());
            h.update(c.buyer);
            h.update(c.claimed_by);
            h.update(c.claimed_at_ms.to_le_bytes());
            h.update([c.attempts]);
            h.update(c.next_attempt_at_ms.to_le_bytes());
        }
        h.update(SEP);

        let mut fills: Vec<(&String, &Fill)> = storage.fills.iter().collect();
        fills.sort_by_key(|(k, _)| (*k).clone());
        for (payment_ref, f) in fills {
            h.update((payment_ref.len() as u32).to_le_bytes());
            h.update(payment_ref.as_bytes());
            h.update(f.ask_id.to_le_bytes());
            h.update(f.seller);
            h.update(f.buyer);
            h.update(f.credits.to_le_bytes());
            h.update(f.paid_sats.to_le_bytes());
            h.update(f.settled_at_ms.to_le_bytes());
        }

        h.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pallets::contributions::ContributionsStorage;
    use crate::pallets::system::SystemStorage;

    const SELLER: PeerId = [1u8; 32];
    const BUYER: PeerId = [2u8; 32];

    fn ctx_for<'a>(signer: &'a PeerId, sys: &'a mut SystemStorage) -> ApplyCtx<'a> {
        ApplyCtx {
            block_height: 1,
            timestamp_ms: 1_700_000_000_000,
            author: signer,
            signer,
            system: sys,
            current_shares_per_credit: 1_000,
            identity: &crate::pallets::NO_IDENTITY,
            rules_version: crate::pallets::upgrade::GENESIS_RULES_VERSION,
        }
    }

    fn funded(seller_balance: u64) -> (MarketStorage, BalancesStorage) {
        let mut b = BalancesStorage::default();
        b.mint(SELLER, seller_balance);
        (MarketStorage::default(), b)
    }

    const NOW: u64 = 1_700_000_000_000;
    const SIX_DAYS_MS: u64 = 6 * 24 * 60 * 60 * 1_000;

    /// Both parties with enough history to trade.
    fn eligible() -> ContributionsStorage {
        let mut c = ContributionsStorage::default();
        for peer in [SELLER, BUYER] {
            c.first_seen_ms.insert(peer, NOW - SIX_DAYS_MS);
            c.share_counts.insert(peer, MIN_SHARES);
            c.wins_by_peer.insert(peer, MIN_WINS);
        }
        c
    }

    fn apply(
        m: &mut MarketStorage,
        b: &mut BalancesStorage,
        call: MarketCall,
        ctx: &mut ApplyCtx<'_>,
    ) -> Result<(), PalletError> {
        MarketPallet::apply_with_balances(m, b, &eligible(), call, ctx)
    }

    fn claim_then_settle(
        m: &mut MarketStorage,
        b: &mut BalancesStorage,
        ask_id: u64,
        buyer: PeerId,
        payment_ref: &str,
    ) -> Result<(), PalletError> {
        let mut sys = SystemStorage::default();
        {
            let mut ctx = ctx_for(&buyer, &mut sys);
            apply(
                m,
                b,
                MarketCall::ClaimFill {
                    ask_id,
                    buyer,
                    payment_ref: payment_ref.into(),
                },
                &mut ctx,
            )?;
        }
        let mut ctx = ctx_for(&SELLER, &mut sys);
        apply(
            m,
            b,
            MarketCall::Settle {
                id: ask_id,
                buyer,
                payment_ref: payment_ref.into(),
            },
            &mut ctx,
        )
    }

    fn post_ask(
        m: &mut MarketStorage,
        b: &mut BalancesStorage,
        credits: u64,
        sats: u64,
    ) -> Result<(), PalletError> {
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&SELLER, &mut sys);
        apply(
            m,
            b,
            MarketCall::PostAsk {
                credits,
                ask_sats: sats,
                ltc_address: "ltc1qexampleaddress".into(),
            },
            &mut ctx,
        )
    }

    #[test]
    fn posting_an_ask_escrows_the_credits_immediately() {
        let (mut m, mut b) = funded(1_000);
        post_ask(&mut m, &mut b, 400, 8_000_000).unwrap();

        assert_eq!(b.balance_of(&SELLER), 600);
        assert_eq!(m.escrowed, 400);
        assert_eq!(m.escrow_sum(), 400);
        assert_eq!(m.asks.len(), 1);
    }

    #[test]
    fn an_ask_beyond_the_balance_is_refused_and_leaves_no_trace() {
        let (mut m, mut b) = funded(100);
        let err = post_ask(&mut m, &mut b, 400, 8_000_000).unwrap_err();

        assert_eq!(err, PalletError::InsufficientBalance);
        assert_eq!(b.balance_of(&SELLER), 100);
        assert!(m.asks.is_empty());
        assert_eq!(m.escrowed, 0);
        assert_eq!(m.next_id, 0, "a refused ask must not consume an id");
    }

    #[test]
    fn cancelling_returns_the_escrow_to_the_seller() {
        let (mut m, mut b) = funded(1_000);
        post_ask(&mut m, &mut b, 400, 8_000_000).unwrap();

        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&SELLER, &mut sys);
        apply(&mut m, &mut b, MarketCall::CancelAsk { id: 0 }, &mut ctx).unwrap();

        assert_eq!(b.balance_of(&SELLER), 1_000);
        assert_eq!(m.escrowed, 0);
        assert!(m.asks.is_empty());
    }

    #[test]
    fn only_the_seller_can_cancel_their_ask() {
        let (mut m, mut b) = funded(1_000);
        post_ask(&mut m, &mut b, 400, 8_000_000).unwrap();

        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&BUYER, &mut sys);
        let err = apply(&mut m, &mut b, MarketCall::CancelAsk { id: 0 }, &mut ctx).unwrap_err();

        assert_eq!(err, PalletError::Unauthorized);
        assert_eq!(m.escrowed, 400, "a refused cancel must not release escrow");
    }

    #[test]
    fn settling_moves_the_escrow_to_the_buyer_and_not_the_seller() {
        let (mut m, mut b) = funded(1_000);
        post_ask(&mut m, &mut b, 400, 8_000_000).unwrap();

        claim_then_settle(&mut m, &mut b, 0, BUYER, "ltc-tx-1").unwrap();

        assert_eq!(b.balance_of(&BUYER), 400);
        // The seller was paid in Litecoin, off chain; no credits come back.
        assert_eq!(b.balance_of(&SELLER), 600);
        assert_eq!(m.escrowed, 0);
        assert!(m.asks.is_empty());
        assert_eq!(m.fills.len(), 1);
        assert!(m.claims.is_empty(), "settling consumes the claim");
    }

    #[test]
    fn a_payment_reference_settles_at_most_one_ask() {
        let (mut m, mut b) = funded(1_000);
        post_ask(&mut m, &mut b, 400, 8_000_000).unwrap();
        post_ask(&mut m, &mut b, 300, 6_000_000).unwrap();

        claim_then_settle(&mut m, &mut b, 0, BUYER, "ltc-tx-1").unwrap();
        let err = claim_then_settle(&mut m, &mut b, 1, BUYER, "ltc-tx-1").unwrap_err();

        assert_eq!(err, PalletError::Unauthorized);
        assert_eq!(b.balance_of(&BUYER), 400, "the replay must not pay twice");
        assert_eq!(m.escrowed, 300, "the second ask stays escrowed");
    }

    #[test]
    fn a_seller_cannot_be_their_own_buyer() {
        let (mut m, mut b) = funded(1_000);
        post_ask(&mut m, &mut b, 400, 8_000_000).unwrap();

        let err = claim_then_settle(&mut m, &mut b, 0, SELLER, "ltc-tx-1").unwrap_err();

        assert_eq!(err, PalletError::InvalidArgs);
        assert_eq!(m.escrowed, 400);
    }

    #[test]
    fn settling_an_unknown_ask_is_refused() {
        let (mut m, mut b) = funded(1_000);
        let err = claim_then_settle(&mut m, &mut b, 99, BUYER, "ltc-tx-1").unwrap_err();

        assert_eq!(err, PalletError::InvalidArgs);
        assert!(m.fills.is_empty(), "a failed settle records no fill");
    }

    #[test]
    fn the_board_caps_how_much_one_account_can_occupy() {
        let (mut m, mut b) = funded(10_000);
        for _ in 0..MAX_OPEN_PER_ACCOUNT {
            post_ask(&mut m, &mut b, 10, 1_000).unwrap();
        }
        let err = post_ask(&mut m, &mut b, 10, 1_000).unwrap_err();

        assert_eq!(err, PalletError::InvalidArgs);
        assert_eq!(m.asks.len(), MAX_OPEN_PER_ACCOUNT);
    }

    #[test]
    fn degenerate_offers_are_refused() {
        let (mut m, mut b) = funded(1_000);
        assert_eq!(
            post_ask(&mut m, &mut b, 0, 1_000).unwrap_err(),
            PalletError::InvalidArgs
        );
        assert_eq!(
            post_ask(&mut m, &mut b, 100, 0).unwrap_err(),
            PalletError::InvalidArgs
        );

        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&SELLER, &mut sys);
        let long = "x".repeat(MAX_LTC_ADDRESS_LEN + 1);
        let err = apply(
            &mut m,
            &mut b,
            MarketCall::PostAsk {
                credits: 100,
                ask_sats: 1_000,
                ltc_address: long,
            },
            &mut ctx,
        )
        .unwrap_err();
        assert_eq!(err, PalletError::InvalidArgs);
        assert_eq!(b.balance_of(&SELLER), 1_000);
    }

    #[test]
    fn a_bid_escrows_nothing_and_can_be_withdrawn() {
        let mut m = MarketStorage::default();
        let mut b = BalancesStorage::default();
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&BUYER, &mut sys);

        apply(
            &mut m,
            &mut b,
            MarketCall::PostBid {
                credits: 500,
                bid_sats: 9_000_000,
            },
            &mut ctx,
        )
        .unwrap();
        assert_eq!(m.bids.len(), 1);
        assert_eq!(m.escrowed, 0);
        assert_eq!(b.balance_of(&BUYER), 0, "a bid costs the buyer nothing");

        apply(&mut m, &mut b, MarketCall::CancelBid { id: 0 }, &mut ctx).unwrap();
        assert!(m.bids.is_empty());
    }

    #[test]
    fn only_the_bidder_can_withdraw_their_bid() {
        let mut m = MarketStorage::default();
        let mut b = BalancesStorage::default();
        let mut sys = SystemStorage::default();
        {
            let mut ctx = ctx_for(&BUYER, &mut sys);
            apply(
                &mut m,
                &mut b,
                MarketCall::PostBid {
                    credits: 500,
                    bid_sats: 9_000_000,
                },
                &mut ctx,
            )
            .unwrap();
        }
        let mut ctx = ctx_for(&SELLER, &mut sys);
        let err = apply(&mut m, &mut b, MarketCall::CancelBid { id: 0 }, &mut ctx).unwrap_err();
        assert_eq!(err, PalletError::Unauthorized);
        assert_eq!(m.bids.len(), 1);
    }

    #[test]
    fn escrow_counter_tracks_the_board_across_a_full_lifecycle() {
        let (mut m, mut b) = funded(1_000);
        post_ask(&mut m, &mut b, 100, 1_000).unwrap();
        post_ask(&mut m, &mut b, 200, 2_000).unwrap();
        post_ask(&mut m, &mut b, 300, 3_000).unwrap();
        assert_eq!(m.escrowed, m.escrow_sum());

        let mut sys = SystemStorage::default();
        {
            let mut ctx = ctx_for(&SELLER, &mut sys);
            apply(&mut m, &mut b, MarketCall::CancelAsk { id: 1 }, &mut ctx).unwrap();
        }
        assert_eq!(m.escrowed, m.escrow_sum());

        claim_then_settle(&mut m, &mut b, 2, BUYER, "ltc-tx-9").unwrap();
        assert_eq!(m.escrowed, m.escrow_sum());
        assert_eq!(m.escrowed, 100);

        // Nothing was created or destroyed: seller spent 600, got 200 back,
        // buyer holds 300, and 100 is still escrowed.
        assert_eq!(
            b.balance_of(&SELLER) + b.balance_of(&BUYER) + m.escrowed,
            1_000
        );
    }

    // ---- standing to trade -------------------------------------------------

    fn contributions_with(days: u64, shares: u64, wins: u64) -> ContributionsStorage {
        let mut c = ContributionsStorage::default();
        c.first_seen_ms
            .insert(SELLER, NOW - days * 24 * 60 * 60 * 1_000);
        c.share_counts.insert(SELLER, shares);
        c.wins_by_peer.insert(SELLER, wins);
        c
    }

    fn try_post(c: &ContributionsStorage) -> Result<(), PalletError> {
        let mut m = MarketStorage::default();
        let mut b = BalancesStorage::default();
        b.mint(SELLER, 1_000);
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&SELLER, &mut sys);
        MarketPallet::apply_with_balances(
            &mut m,
            &mut b,
            c,
            MarketCall::PostAsk {
                credits: 100,
                ask_sats: 1_000,
                ltc_address: "ltc1qexampleaddress".into(),
            },
            &mut ctx,
        )
    }

    #[test]
    fn an_account_with_no_history_cannot_trade() {
        let err = try_post(&ContributionsStorage::default()).unwrap_err();
        assert_eq!(err, PalletError::Unauthorized);
    }

    #[test]
    fn each_threshold_is_enforced_on_its_own() {
        // Fresh keys are free, so tenure alone is not enough...
        assert!(try_post(&contributions_with(MIN_DAYS_ACTIVE, 0, MIN_WINS)).is_err());
        // ...nor is contribution without tenure...
        assert!(try_post(&contributions_with(0, MIN_SHARES, MIN_WINS)).is_err());
        // ...nor relaying without ever having answered.
        assert!(try_post(&contributions_with(MIN_DAYS_ACTIVE, MIN_SHARES, 0)).is_err());
        // All three together qualify.
        assert!(try_post(&contributions_with(MIN_DAYS_ACTIVE, MIN_SHARES, MIN_WINS)).is_ok());
    }

    #[test]
    fn tenure_counts_from_first_contribution_not_from_first_sight() {
        let mut c = contributions_with(MIN_DAYS_ACTIVE, MIN_SHARES, MIN_WINS);
        // An identity that existed but did nothing has no first contribution
        // to count from, whatever else it accumulated later.
        c.first_seen_ms.remove(&SELLER);
        assert!(try_post(&c).is_err());
    }

    #[test]
    fn the_refusal_says_how_far_off_the_account_is() {
        let c = contributions_with(2, 10, 0);
        let err = check_eligible(&c, &SELLER, NOW).unwrap_err();
        assert_eq!(
            err,
            Ineligible::TooNew {
                days_active: 2,
                required: MIN_DAYS_ACTIVE
            }
        );
        assert!(err.to_string().contains("2 of the 5 days"));
    }

    #[test]
    fn an_account_that_falls_short_can_still_withdraw_its_own_escrow() {
        // Escrow belongs to the seller; a rule about who may trade must not
        // become a rule that strands it.
        let mut m = MarketStorage::default();
        let mut b = BalancesStorage::default();
        b.mint(SELLER, 1_000);
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&SELLER, &mut sys);

        MarketPallet::apply_with_balances(
            &mut m,
            &mut b,
            &contributions_with(MIN_DAYS_ACTIVE, MIN_SHARES, MIN_WINS),
            MarketCall::PostAsk {
                credits: 400,
                ask_sats: 8_000_000,
                ltc_address: "ltc1qexampleaddress".into(),
            },
            &mut ctx,
        )
        .unwrap();

        MarketPallet::apply_with_balances(
            &mut m,
            &mut b,
            &ContributionsStorage::default(),
            MarketCall::CancelAsk { id: 0 },
            &mut ctx,
        )
        .unwrap();

        assert_eq!(b.balance_of(&SELLER), 1_000);
        assert_eq!(m.escrowed, 0);
    }

    // ---- what a claim binds ------------------------------------------------

    #[test]
    fn the_oracle_cannot_settle_what_nobody_claimed() {
        // Without this the oracle key alone could move any escrow anywhere.
        let (mut m, mut b) = funded(1_000);
        post_ask(&mut m, &mut b, 400, 8_000_000).unwrap();

        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&SELLER, &mut sys);
        let err = apply(
            &mut m,
            &mut b,
            MarketCall::Settle {
                id: 0,
                buyer: BUYER,
                payment_ref: "ltc-tx-unclaimed".into(),
            },
            &mut ctx,
        )
        .unwrap_err();

        assert_eq!(err, PalletError::Unauthorized);
        assert_eq!(m.escrowed, 400);
        assert_eq!(b.balance_of(&BUYER), 0);
    }

    #[test]
    fn the_oracle_cannot_redirect_a_claim_to_someone_else() {
        const OUTSIDER: PeerId = [9u8; 32];
        let (mut m, mut b) = funded(1_000);
        post_ask(&mut m, &mut b, 400, 8_000_000).unwrap();

        let mut sys = SystemStorage::default();
        {
            let mut ctx = ctx_for(&BUYER, &mut sys);
            apply(
                &mut m,
                &mut b,
                MarketCall::ClaimFill {
                    ask_id: 0,
                    buyer: BUYER,
                    payment_ref: "ltc-tx-1".into(),
                },
                &mut ctx,
            )
            .unwrap();
        }

        let mut ctx = ctx_for(&SELLER, &mut sys);
        let err = apply(
            &mut m,
            &mut b,
            MarketCall::Settle {
                id: 0,
                buyer: OUTSIDER,
                payment_ref: "ltc-tx-1".into(),
            },
            &mut ctx,
        )
        .unwrap_err();

        assert_eq!(err, PalletError::Unauthorized);
        assert_eq!(b.balance_of(&OUTSIDER), 0);
    }

    #[test]
    fn a_seller_may_claim_for_the_buyer() {
        // The orphaned-payment case: a buyer who pays and disappears would
        // otherwise leave the escrow stuck forever.
        let (mut m, mut b) = funded(1_000);
        post_ask(&mut m, &mut b, 400, 8_000_000).unwrap();

        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&SELLER, &mut sys);
        apply(
            &mut m,
            &mut b,
            MarketCall::ClaimFill {
                ask_id: 0,
                buyer: BUYER,
                payment_ref: "ltc-tx-1".into(),
            },
            &mut ctx,
        )
        .unwrap();

        assert_eq!(m.claims.get("ltc-tx-1").unwrap().buyer, BUYER);
        assert_eq!(m.claims.get("ltc-tx-1").unwrap().claimed_by, SELLER);
    }

    #[test]
    fn a_bystander_cannot_raise_a_claim() {
        const OUTSIDER: PeerId = [9u8; 32];
        let (mut m, mut b) = funded(1_000);
        post_ask(&mut m, &mut b, 400, 8_000_000).unwrap();

        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&OUTSIDER, &mut sys);
        let err = MarketPallet::apply_with_balances(
            &mut m,
            &mut b,
            &eligible(),
            MarketCall::ClaimFill {
                ask_id: 0,
                buyer: BUYER,
                payment_ref: "ltc-tx-1".into(),
            },
            &mut ctx,
        )
        .unwrap_err();

        assert_eq!(err, PalletError::Unauthorized);
        assert!(m.claims.is_empty());
    }

    #[test]
    fn one_payment_cannot_be_claimed_against_two_asks() {
        let (mut m, mut b) = funded(1_000);
        post_ask(&mut m, &mut b, 400, 8_000_000).unwrap();
        post_ask(&mut m, &mut b, 300, 6_000_000).unwrap();

        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&BUYER, &mut sys);
        apply(
            &mut m,
            &mut b,
            MarketCall::ClaimFill {
                ask_id: 0,
                buyer: BUYER,
                payment_ref: "ltc-tx-1".into(),
            },
            &mut ctx,
        )
        .unwrap();
        let err = apply(
            &mut m,
            &mut b,
            MarketCall::ClaimFill {
                ask_id: 1,
                buyer: BUYER,
                payment_ref: "ltc-tx-1".into(),
            },
            &mut ctx,
        )
        .unwrap_err();

        assert_eq!(err, PalletError::Unauthorized);
        assert_eq!(m.claims.len(), 1);
    }

    // ---- a claim is not taken as infallible --------------------------------

    /// Board with one ask and one open claim on it.
    fn with_open_claim() -> (MarketStorage, BalancesStorage) {
        let (mut m, mut b) = funded(1_000);
        post_ask(&mut m, &mut b, 400, 8_000_000).unwrap();
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&BUYER, &mut sys);
        apply(
            &mut m,
            &mut b,
            MarketCall::ClaimFill {
                ask_id: 0,
                buyer: BUYER,
                payment_ref: "ltc-tx-1".into(),
            },
            &mut ctx,
        )
        .unwrap();
        (m, b)
    }

    fn fail_claim_at(
        m: &mut MarketStorage,
        b: &mut BalancesStorage,
        now_ms: u64,
        permanent: bool,
    ) -> Result<(), PalletError> {
        let mut sys = SystemStorage::default();
        let mut ctx = ApplyCtx {
            block_height: 1,
            timestamp_ms: now_ms,
            author: &SELLER,
            signer: &SELLER,
            system: &mut sys,
            current_shares_per_credit: 1_000,
            identity: &crate::pallets::NO_IDENTITY,
            rules_version: crate::pallets::upgrade::GENESIS_RULES_VERSION,
        };
        apply(
            m,
            b,
            MarketCall::FailClaim {
                payment_ref: "ltc-tx-1".into(),
                permanent,
            },
            &mut ctx,
        )
    }

    #[test]
    fn a_fresh_claim_may_be_checked_at_once() {
        let (m, _) = with_open_claim();
        let claim = m.claims.get("ltc-tx-1").unwrap();
        assert_eq!(claim.attempts, 0);
        assert!(claim.is_due(NOW), "the first look should not have to wait");
        assert_eq!(claim.attempts_remaining(), MAX_CLAIM_ATTEMPTS);
    }

    #[test]
    fn a_failed_check_schedules_the_next_one_ten_minutes_out() {
        let (mut m, mut b) = with_open_claim();
        fail_claim_at(&mut m, &mut b, NOW, false).unwrap();

        let claim = m.claims.get("ltc-tx-1").unwrap();
        assert_eq!(claim.attempts, 1);
        assert_eq!(claim.next_attempt_at_ms, NOW + CLAIM_RETRY_BACKOFF_MS[0]);
        assert!(!claim.is_due(NOW), "the retry is not due immediately");
        assert!(claim.is_due(NOW + CLAIM_RETRY_BACKOFF_MS[0]));
    }

    #[test]
    fn the_oracle_cannot_burn_through_the_attempts_in_one_block() {
        // Without the schedule being enforced on chain, an oracle could fail a
        // claim four times in a row and strand a payment that was only slow.
        let (mut m, mut b) = with_open_claim();
        fail_claim_at(&mut m, &mut b, NOW, false).unwrap();

        let err = fail_claim_at(&mut m, &mut b, NOW, false).unwrap_err();
        assert_eq!(err, PalletError::Unauthorized);
        assert_eq!(m.claims.get("ltc-tx-1").unwrap().attempts, 1);
    }

    #[test]
    fn the_backoff_widens_across_the_three_retries() {
        let (mut m, mut b) = with_open_claim();
        let mut now = NOW;
        for (i, step) in CLAIM_RETRY_BACKOFF_MS.iter().enumerate() {
            fail_claim_at(&mut m, &mut b, now, false).unwrap();
            let claim = m
                .claims
                .get("ltc-tx-1")
                .unwrap_or_else(|| panic!("claim dropped early at retry {i}"));
            assert_eq!(claim.attempts as usize, i + 1);
            assert_eq!(claim.next_attempt_at_ms, now + step);
            now = claim.next_attempt_at_ms;
        }
        // 10 min, then 30, then an hour.
        assert_eq!(CLAIM_RETRY_BACKOFF_MS[0], 10 * 60 * 1_000);
        assert_eq!(CLAIM_RETRY_BACKOFF_MS[1], 30 * 60 * 1_000);
        assert_eq!(CLAIM_RETRY_BACKOFF_MS[2], 60 * 60 * 1_000);
    }

    #[test]
    fn the_claim_is_abandoned_once_the_attempts_run_out() {
        let (mut m, mut b) = with_open_claim();
        let mut now = NOW;
        for _ in 0..CLAIM_RETRY_BACKOFF_MS.len() {
            fail_claim_at(&mut m, &mut b, now, false).unwrap();
            now = m.claims.get("ltc-tx-1").unwrap().next_attempt_at_ms;
        }
        // The fourth failure is the last one.
        fail_claim_at(&mut m, &mut b, now, false).unwrap();

        assert!(m.claims.is_empty(), "an exhausted claim is dropped");
        assert_eq!(m.escrowed, 400, "the escrow stays with the seller");
        assert_eq!(b.balance_of(&BUYER), 0);
    }

    #[test]
    fn abandoning_a_claim_frees_the_payment_to_be_claimed_again() {
        // A payment claimed before it was deep enough must not be locked out
        // of the board forever.
        let (mut m, mut b) = with_open_claim();
        fail_claim_at(&mut m, &mut b, NOW, true).unwrap();
        assert!(m.claims.is_empty());

        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&BUYER, &mut sys);
        apply(
            &mut m,
            &mut b,
            MarketCall::ClaimFill {
                ask_id: 0,
                buyer: BUYER,
                payment_ref: "ltc-tx-1".into(),
            },
            &mut ctx,
        )
        .unwrap();
        assert_eq!(m.claims.len(), 1);
    }

    #[test]
    fn a_permanent_failure_does_not_wait_for_the_schedule() {
        // A payment naming a different buyer will never become valid, so there
        // is nothing to wait for.
        let (mut m, mut b) = with_open_claim();
        fail_claim_at(&mut m, &mut b, NOW, false).unwrap();
        fail_claim_at(&mut m, &mut b, NOW, true).unwrap();
        assert!(m.claims.is_empty());
    }

    #[test]
    fn failing_an_unknown_claim_is_refused() {
        let (mut m, mut b) = funded(1_000);
        let err = fail_claim_at(&mut m, &mut b, NOW, false).unwrap_err();
        assert_eq!(err, PalletError::InvalidArgs);
    }

    #[test]
    fn a_retried_claim_can_still_settle() {
        let (mut m, mut b) = with_open_claim();
        fail_claim_at(&mut m, &mut b, NOW, false).unwrap();

        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&SELLER, &mut sys);
        apply(
            &mut m,
            &mut b,
            MarketCall::Settle {
                id: 0,
                buyer: BUYER,
                payment_ref: "ltc-tx-1".into(),
            },
            &mut ctx,
        )
        .unwrap();

        assert_eq!(b.balance_of(&BUYER), 400);
        assert!(m.claims.is_empty());
    }

    #[test]
    fn pallet_root_changes_when_the_board_changes() {
        let (mut m, mut b) = funded(1_000);
        let r0 = MarketPallet::pallet_root(&m);
        post_ask(&mut m, &mut b, 100, 1_000).unwrap();
        let r1 = MarketPallet::pallet_root(&m);
        assert_ne!(r0, r1);
    }

    #[test]
    fn direct_apply_is_not_a_usable_entry_point() {
        let mut m = MarketStorage::default();
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_for(&SELLER, &mut sys);
        let err =
            MarketPallet::apply(&mut m, MarketCall::CancelAsk { id: 0 }, &mut ctx).unwrap_err();
        assert_eq!(err, PalletError::InvalidArgs);
    }

    #[test]
    fn tenure_counts_whole_days_from_the_first_contribution() {
        const DAY: u64 = 24 * 60 * 60 * 1_000;
        let now = 1_700_000_000_000;
        let mut c = ContributionsStorage::default();
        c.share_counts.insert(SELLER, MIN_SHARES);
        c.wins_by_peer.insert(SELLER, MIN_WINS);

        c.first_seen_ms
            .insert(SELLER, now - MIN_DAYS_ACTIVE * DAY + 1);
        assert_eq!(
            check_eligible(&c, &SELLER, now),
            Err(Ineligible::TooNew {
                days_active: MIN_DAYS_ACTIVE - 1,
                required: MIN_DAYS_ACTIVE
            })
        );
        c.first_seen_ms.insert(SELLER, now - MIN_DAYS_ACTIVE * DAY);
        assert_eq!(check_eligible(&c, &SELLER, now), Ok(()));
    }

    #[test]
    fn an_account_that_only_asks_never_qualifies_for_the_board() {
        // A desktop that consumes and never wins the lottery cannot buy on
        // the board however long it has been around: the direct sale is its
        // only way to get credits.
        let now = 1_700_000_000_000;
        let mut c = ContributionsStorage::default();
        c.first_seen_ms
            .insert(BUYER, now - 365 * 24 * 60 * 60 * 1_000);
        c.shares_consumed.insert(BUYER, 10_000);
        assert!(check_eligible(&c, &BUYER, now).is_err());

        c.share_counts.insert(BUYER, MIN_SHARES);
        assert_eq!(
            check_eligible(&c, &BUYER, now),
            Err(Ineligible::TooFewWins {
                have: 0,
                required: MIN_WINS
            })
        );
    }
}
