// SPDX-License-Identifier: BUSL-1.1
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::balances::BalancesStorage;
use super::{ApplyCtx, Pallet, PalletError};
use crate::identity::PeerId;

pub const NAI_CREDITS_PER_LTC: u64 = 5_000;

pub const MAX_DIRECT_SALE_CREDITS: u64 = 5_000;

pub const DEFAULT_DAILY_MINT_CAP: u64 = 50_000;

pub const DAILY_WINDOW_MS: u64 = 24 * 60 * 60 * 1_000;

pub const PARAM_TIMELOCK_BLOCKS: u64 = 302_400;

pub const MAX_DIRECT_SALE_WALLET_LEN: usize = 90;

/// Cap on the operator-facing reason attached to a sale outage.
pub const MAX_SALE_HEALTH_REASON_LEN: usize = 160;

pub const PRIORITY_SESSION_COST: u64 = 10;

pub const DEFAULT_CHANNEL_SESSION_COST: u64 = PRIORITY_SESSION_COST;

pub const INFERENCES_PER_SESSION: u8 = 10;

pub const SESSION_TTL_MS: u64 = 60 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NaiSession {
    pub owner: PeerId,
    pub expires_at_ms: u64,
    pub inferences_remaining: u8,
    #[serde(default)]
    pub channel: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectSaleRecord {
    pub receiver: PeerId,
    pub credits: u64,
    pub recorded_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ParamChange {
    DailyMintCap(u64),
    ChannelSessionCost { channel: String, cost: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingParamChange {
    pub effective_at_height: u64,
    pub change: ParamChange,
}

fn sale_healthy_by_default() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NaiStorage {
    pub sessions: HashMap<[u8; 32], NaiSession>,
    pub direct_sales: HashMap<String, DirectSaleRecord>,
    #[serde(default)]
    pub direct_sale_wallet: Option<String>,
    /// Whether the off-chain payment watcher can currently credit payments.
    ///
    /// The watcher runs beside the anchor and can lose the sources it needs to
    /// confirm a payment safely. It keeps running when that happens rather
    /// than exiting — a watcher that exits stops noticing payments at all —
    /// but it stops crediting, and payments made meanwhile sit unprocessed
    /// until it recovers. This is how that reaches a buyer's client before
    /// they send money, instead of after.
    #[serde(default = "sale_healthy_by_default")]
    pub direct_sale_healthy: bool,
    /// Plain-language cause, shown to buyers while sales are paused.
    #[serde(default)]
    pub direct_sale_health_reason: Option<String>,
    #[serde(default)]
    pub top_tier_session_costs: HashMap<String, u64>,
    pub total_minted: u64,
    pub total_spent: u64,
    #[serde(default)]
    pub daily_mint_window_start_ms: u64,
    #[serde(default)]
    pub daily_minted: u64,
    #[serde(default)]
    pub daily_mint_cap: u64,
    #[serde(default)]
    pub pending_changes: Vec<PendingParamChange>,
    /// Narrow, revocable key allowed to submit `DirectSale` without being a
    /// block-producing authority. Lets the off-chain payment watcher run as a
    /// separate process holding only this key.
    #[serde(default)]
    pub sale_oracle: Option<PeerId>,
}

impl Default for NaiStorage {
    fn default() -> Self {
        Self {
            sessions: HashMap::new(),
            direct_sales: HashMap::new(),
            direct_sale_wallet: None,
            direct_sale_healthy: true,
            direct_sale_health_reason: None,
            top_tier_session_costs: HashMap::new(),
            total_minted: 0,
            total_spent: 0,
            daily_mint_window_start_ms: 0,
            daily_minted: 0,
            daily_mint_cap: 0,
            pending_changes: Vec::new(),
            sale_oracle: None,
        }
    }
}

impl NaiStorage {
    pub fn session(&self, token: &[u8; 32]) -> Option<&NaiSession> {
        self.sessions.get(token)
    }

    pub fn session_expired(&self, token: &[u8; 32], now_ms: u64) -> bool {
        match self.session(token) {
            Some(s) => now_ms >= s.expires_at_ms || s.inferences_remaining == 0,
            None => true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum NaiCall {
    /// Authority-only mint against a confirmed off-chain payment.
    /// `payment_ref` is an opaque, currency-agnostic identifier used only
    /// for idempotency (see `direct_sales`), never parsed by this pallet.
    DirectSale {
        receiver: PeerId,
        credits: u64,
        payment_ref: String,
    },
    Spend {
        session_token: [u8; 32],
    },
    UseSession {
        session_token: [u8; 32],
    },
    SetDirectSaleWallet {
        address: Option<String>,
    },
    OpenChannelSession {
        channel: String,
        session_token: [u8; 32],
    },
    SetChannelSessionCost {
        channel: String,
        cost: u64,
    },
    SetDailyMintCap {
        cap: u64,
    },
    /// Authority-only. Designates (or clears, with `None`) the key permitted
    /// to submit `DirectSale`. See `NaiStorage::sale_oracle`.
    SetSaleOracle {
        pubkey: Option<PeerId>,
    },
    /// Reports whether the payment watcher can credit payments right now.
    /// Submitted by the sale oracle on a change of state, never per tick.
    SetSaleHealth {
        healthy: bool,
        reason: Option<String>,
    },
}

pub const MAX_CHANNEL_NAME_LEN: usize = 32;

pub struct NaiPallet;

impl NaiPallet {
    pub fn drain_due_changes(storage: &mut NaiStorage, current_height: u64) {
        let due: Vec<ParamChange> = {
            let mut still_pending = Vec::new();
            let mut ready = Vec::new();
            for pc in storage.pending_changes.drain(..) {
                if pc.effective_at_height <= current_height {
                    ready.push(pc.change);
                } else {
                    still_pending.push(pc);
                }
            }
            storage.pending_changes = still_pending;
            ready
        };
        for change in due {
            match change {
                ParamChange::DailyMintCap(cap) => {
                    if cap > 0 {
                        storage.daily_mint_cap = cap;
                    }
                }
                ParamChange::ChannelSessionCost { channel, cost } => {
                    if cost == 0 {
                        storage.top_tier_session_costs.remove(&channel);
                    } else {
                        storage.top_tier_session_costs.insert(channel, cost);
                    }
                }
            }
        }
    }

    pub fn apply_with_balances(
        storage: &mut NaiStorage,
        balances: &mut BalancesStorage,
        call: NaiCall,
        ctx: &mut ApplyCtx<'_>,
    ) -> Result<(), PalletError> {
        match call {
            NaiCall::DirectSale {
                receiver,
                credits,
                payment_ref,
            } => {
                if credits == 0 || payment_ref.is_empty() {
                    return Err(PalletError::InvalidArgs);
                }
                if credits > MAX_DIRECT_SALE_CREDITS {
                    return Err(PalletError::InvalidArgs);
                }
                if storage.direct_sales.contains_key(&payment_ref) {
                    return Err(PalletError::Unauthorized);
                }
                let cap = if storage.daily_mint_cap == 0 {
                    DEFAULT_DAILY_MINT_CAP
                } else {
                    storage.daily_mint_cap
                };
                if ctx.timestamp_ms
                    >= storage
                        .daily_mint_window_start_ms
                        .saturating_add(DAILY_WINDOW_MS)
                {
                    storage.daily_mint_window_start_ms = ctx.timestamp_ms;
                    storage.daily_minted = 0;
                }
                if storage.daily_minted.saturating_add(credits) > cap {
                    return Err(PalletError::InvalidArgs);
                }
                balances.mint(receiver, credits);
                storage.total_minted = storage.total_minted.saturating_add(credits);
                storage.daily_minted = storage.daily_minted.saturating_add(credits);
                storage.direct_sales.insert(
                    payment_ref,
                    DirectSaleRecord {
                        receiver,
                        credits,
                        recorded_at_ms: ctx.timestamp_ms,
                    },
                );
                Ok(())
            }
            NaiCall::SetDirectSaleWallet { address } => {
                if let Some(addr) = &address {
                    if addr.is_empty() || addr.len() > MAX_DIRECT_SALE_WALLET_LEN {
                        return Err(PalletError::InvalidArgs);
                    }
                }
                storage.direct_sale_wallet = address;
                Ok(())
            }
            NaiCall::Spend { session_token } => {
                if storage.sessions.contains_key(&session_token) {
                    return Err(PalletError::InvalidArgs);
                }
                balances.burn_checked(ctx.signer, PRIORITY_SESSION_COST)?;
                storage.total_spent = storage.total_spent.saturating_add(PRIORITY_SESSION_COST);
                storage.sessions.insert(
                    session_token,
                    NaiSession {
                        owner: *ctx.signer,
                        expires_at_ms: ctx.timestamp_ms.saturating_add(SESSION_TTL_MS),
                        inferences_remaining: INFERENCES_PER_SESSION,
                        channel: None,
                    },
                );
                Ok(())
            }
            NaiCall::UseSession { session_token } => {
                let Some(session) = storage.sessions.get_mut(&session_token) else {
                    return Err(PalletError::InvalidArgs);
                };
                if session.owner != *ctx.signer {
                    return Err(PalletError::Unauthorized);
                }
                if ctx.timestamp_ms >= session.expires_at_ms || session.inferences_remaining == 0 {
                    return Err(PalletError::InvalidArgs);
                }
                session.inferences_remaining -= 1;
                Ok(())
            }
            NaiCall::OpenChannelSession {
                channel,
                session_token,
            } => {
                if channel.is_empty() || channel.len() > MAX_CHANNEL_NAME_LEN {
                    return Err(PalletError::InvalidArgs);
                }
                if storage.sessions.contains_key(&session_token) {
                    return Err(PalletError::InvalidArgs);
                }
                // Channels without an authority-set cost fall back to the
                // default so a fresh network (empty top_tier_session_costs,
                // 7-day timelock on SetChannelSessionCost) can still open
                // sessions from day one.
                let cost = storage
                    .top_tier_session_costs
                    .get(&channel)
                    .copied()
                    .unwrap_or(DEFAULT_CHANNEL_SESSION_COST);
                balances.burn_checked(ctx.signer, cost)?;
                storage.total_spent = storage.total_spent.saturating_add(cost);
                storage.sessions.insert(
                    session_token,
                    NaiSession {
                        owner: *ctx.signer,
                        expires_at_ms: ctx.timestamp_ms.saturating_add(SESSION_TTL_MS),
                        inferences_remaining: INFERENCES_PER_SESSION,
                        channel: Some(channel),
                    },
                );
                Ok(())
            }
            NaiCall::SetChannelSessionCost { channel, cost } => {
                if channel.is_empty() || channel.len() > MAX_CHANNEL_NAME_LEN {
                    return Err(PalletError::InvalidArgs);
                }
                let effective_at = ctx.block_height.saturating_add(PARAM_TIMELOCK_BLOCKS);
                storage.pending_changes.push(PendingParamChange {
                    effective_at_height: effective_at,
                    change: ParamChange::ChannelSessionCost { channel, cost },
                });
                Ok(())
            }
            NaiCall::SetSaleOracle { pubkey } => {
                storage.sale_oracle = pubkey;
                Ok(())
            }
            NaiCall::SetSaleHealth { healthy, reason } => {
                if let Some(r) = &reason {
                    if r.len() > MAX_SALE_HEALTH_REASON_LEN {
                        return Err(PalletError::InvalidArgs);
                    }
                }
                storage.direct_sale_healthy = healthy;
                // Before v2 a recovery cleared the cause. From v2 the oracle
                // states the reason either way, so a caveat that holds while
                // healthy (payments checked by a single validator, say) is on
                // chain rather than a fixed notice in the client.
                storage.direct_sale_health_reason =
                    if healthy && ctx.rules_version < crate::pallets::upgrade::RULES_V2 {
                        None
                    } else {
                        reason
                    };
                Ok(())
            }
            NaiCall::SetDailyMintCap { cap } => {
                if cap == 0 {
                    return Err(PalletError::InvalidArgs);
                }
                let effective_at = ctx.block_height.saturating_add(PARAM_TIMELOCK_BLOCKS);
                storage.pending_changes.push(PendingParamChange {
                    effective_at_height: effective_at,
                    change: ParamChange::DailyMintCap(cap),
                });
                Ok(())
            }
        }
    }
}

impl Pallet for NaiPallet {
    type Storage = NaiStorage;
    type Call = NaiCall;
    const PALLET_IDX: u8 = 3;

    fn apply(
        _storage: &mut Self::Storage,
        _call: Self::Call,
        _ctx: &mut ApplyCtx<'_>,
    ) -> Result<(), PalletError> {
        Err(PalletError::InvalidArgs)
    }

    fn pallet_root(storage: &Self::Storage) -> [u8; 32] {
        let mut h = Sha256::new();
        const SEP: &[u8] = b"||";

        let mut sessions: Vec<(&[u8; 32], &NaiSession)> = storage.sessions.iter().collect();
        sessions.sort_by_key(|(k, _)| **k);
        for (token, s) in sessions {
            h.update(token);
            h.update(s.owner);
            h.update(s.expires_at_ms.to_le_bytes());
            h.update([s.inferences_remaining]);
            match &s.channel {
                Some(c) => {
                    h.update([1u8]);
                    h.update((c.len() as u32).to_le_bytes());
                    h.update(c.as_bytes());
                }
                None => {
                    h.update([0u8]);
                }
            }
        }
        h.update(SEP);

        let mut sales: Vec<(&String, &DirectSaleRecord)> = storage.direct_sales.iter().collect();
        sales.sort_by_key(|(k, _)| (*k).clone());
        for (ltc_id, rec) in sales {
            h.update(ltc_id.as_bytes());
            h.update(rec.receiver);
            h.update(rec.credits.to_le_bytes());
            h.update(rec.recorded_at_ms.to_le_bytes());
        }
        h.update(SEP);

        h.update(storage.total_minted.to_le_bytes());
        h.update(storage.total_spent.to_le_bytes());
        h.update(storage.daily_mint_window_start_ms.to_le_bytes());
        h.update(storage.daily_minted.to_le_bytes());
        h.update(storage.daily_mint_cap.to_le_bytes());
        h.update(SEP);

        h.update([u8::from(storage.direct_sale_healthy)]);
        match &storage.direct_sale_health_reason {
            Some(r) => {
                h.update([1u8]);
                h.update((r.len() as u32).to_le_bytes());
                h.update(r.as_bytes());
            }
            None => {
                h.update([0u8]);
            }
        }
        h.update(SEP);

        match &storage.sale_oracle {
            Some(pk) => {
                h.update([1u8]);
                h.update(pk);
            }
            None => {
                h.update([0u8]);
            }
        }
        h.update(SEP);

        match &storage.direct_sale_wallet {
            Some(addr) => {
                h.update([1u8]);
                h.update((addr.len() as u32).to_le_bytes());
                h.update(addr.as_bytes());
            }
            None => {
                h.update([0u8]);
            }
        }
        h.update(SEP);

        let mut costs: Vec<(&String, &u64)> = storage.top_tier_session_costs.iter().collect();
        costs.sort_by_key(|(k, _)| (*k).clone());
        for (channel, cost) in costs {
            h.update((channel.len() as u32).to_le_bytes());
            h.update(channel.as_bytes());
            h.update(cost.to_le_bytes());
        }
        h.update(SEP);

        for pc in &storage.pending_changes {
            h.update(pc.effective_at_height.to_le_bytes());
            match &pc.change {
                ParamChange::DailyMintCap(cap) => {
                    h.update([1u8]);
                    h.update(cap.to_le_bytes());
                }
                ParamChange::ChannelSessionCost { channel, cost } => {
                    h.update([2u8]);
                    h.update((channel.len() as u32).to_le_bytes());
                    h.update(channel.as_bytes());
                    h.update(cost.to_le_bytes());
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

    fn ctx_at<'a>(
        signer: &'a PeerId,
        author: &'a PeerId,
        now_ms: u64,
        sys: &'a mut SystemStorage,
    ) -> ApplyCtx<'a> {
        ApplyCtx {
            block_height: 1,
            timestamp_ms: now_ms,
            author,
            signer,
            system: sys,
            current_shares_per_credit: crate::pallets::economy::DEFAULT_SHARES_PER_CREDIT,
            identity: &crate::pallets::NO_IDENTITY,
            rules_version: crate::pallets::upgrade::GENESIS_RULES_VERSION,
        }
    }

    #[test]
    fn direct_sale_mints_and_records() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let receiver: PeerId = [9u8; 32];
        let authority: PeerId = [1u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&authority, &authority, 1_700_000_000_000, &mut sys);

        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::DirectSale {
                receiver,
                credits: 100,
                payment_ref: "abc123".to_string(),
            },
            &mut ctx,
        )
        .unwrap();
        assert_eq!(balances.balance_of(&receiver), 100);
        assert_eq!(storage.total_minted, 100);
        assert!(storage.direct_sales.contains_key("abc123"));
    }

    #[test]
    fn direct_sale_double_mint_rejected() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let receiver: PeerId = [9u8; 32];
        let authority: PeerId = [1u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&authority, &authority, 0, &mut sys);

        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::DirectSale {
                receiver,
                credits: 100,
                payment_ref: "abc123".to_string(),
            },
            &mut ctx,
        )
        .unwrap();

        let second = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::DirectSale {
                receiver,
                credits: 100,
                payment_ref: "abc123".to_string(),
            },
            &mut ctx,
        );
        assert_eq!(second, Err(PalletError::Unauthorized));
        assert_eq!(balances.balance_of(&receiver), 100, "no double mint");
        assert_eq!(storage.total_minted, 100);
    }

    #[test]
    fn spend_opens_session_and_burns_credits() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let peer: PeerId = [7u8; 32];
        balances.mint(peer, 50);
        let mut sys = SystemStorage::default();
        let now = 1_700_000_000_000_u64;
        let mut ctx = ctx_at(&peer, &peer, now, &mut sys);
        let token = [42u8; 32];

        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::Spend {
                session_token: token,
            },
            &mut ctx,
        )
        .unwrap();
        assert_eq!(balances.balance_of(&peer), 50 - PRIORITY_SESSION_COST);
        assert_eq!(storage.total_spent, PRIORITY_SESSION_COST);
        let s = storage.session(&token).unwrap();
        assert_eq!(s.owner, peer);
        assert_eq!(s.inferences_remaining, INFERENCES_PER_SESSION);
        assert_eq!(s.expires_at_ms, now + SESSION_TTL_MS);
    }

    #[test]
    fn spend_with_insufficient_balance_rejects_and_no_session_opens() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let peer: PeerId = [7u8; 32];
        balances.mint(peer, PRIORITY_SESSION_COST - 1);
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&peer, &peer, 0, &mut sys);
        let token = [42u8; 32];

        let result = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::Spend {
                session_token: token,
            },
            &mut ctx,
        );
        assert_eq!(result, Err(PalletError::InsufficientBalance));
        assert!(storage.session(&token).is_none(), "session must not open");
        assert_eq!(storage.total_spent, 0);
    }

    #[test]
    fn spend_token_reuse_rejected() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let peer: PeerId = [7u8; 32];
        balances.mint(peer, 100);
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&peer, &peer, 0, &mut sys);
        let token = [42u8; 32];

        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::Spend {
                session_token: token,
            },
            &mut ctx,
        )
        .unwrap();
        let balance_after_first = balances.balance_of(&peer);

        let second = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::Spend {
                session_token: token,
            },
            &mut ctx,
        );
        assert_eq!(second, Err(PalletError::InvalidArgs));
        assert_eq!(
            balances.balance_of(&peer),
            balance_after_first,
            "no double burn"
        );
    }

    #[test]
    fn session_expired_after_ttl() {
        let mut storage = NaiStorage::default();
        let token = [42u8; 32];
        storage.sessions.insert(
            token,
            NaiSession {
                owner: [7u8; 32],
                expires_at_ms: 1000,
                inferences_remaining: 5,
                channel: None,
            },
        );
        assert!(!storage.session_expired(&token, 999));
        assert!(storage.session_expired(&token, 1000));
        assert!(storage.session_expired(&token, 5_000));
    }

    #[test]
    fn session_expired_when_inferences_exhausted() {
        let mut storage = NaiStorage::default();
        let token = [42u8; 32];
        storage.sessions.insert(
            token,
            NaiSession {
                owner: [7u8; 32],
                expires_at_ms: u64::MAX,
                inferences_remaining: 0,
                channel: None,
            },
        );
        assert!(storage.session_expired(&token, 0));
    }

    #[test]
    fn direct_sale_credits_above_cap_rejected() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let authority: PeerId = [1u8; 32];
        let receiver: PeerId = [9u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&authority, &authority, 0, &mut sys);

        let result = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::DirectSale {
                receiver,
                credits: MAX_DIRECT_SALE_CREDITS + 1,
                payment_ref: "biggie".to_string(),
            },
            &mut ctx,
        );
        assert_eq!(result, Err(PalletError::InvalidArgs));
        assert_eq!(balances.balance_of(&receiver), 0);
        assert_eq!(storage.total_minted, 0);
        assert!(
            !storage.direct_sales.contains_key("biggie"),
            "rejected sale must NOT consume the tx_id slot — \
             a corrected resubmission of the same payment_ref at a \
             legal amount should still go through"
        );
    }

    #[test]
    fn daily_mint_cap_blocks_overshoot() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let authority: PeerId = [1u8; 32];
        let receiver: PeerId = [9u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&authority, &authority, 1_000, &mut sys);

        for i in 0..(DEFAULT_DAILY_MINT_CAP / MAX_DIRECT_SALE_CREDITS) {
            NaiPallet::apply_with_balances(
                &mut storage,
                &mut balances,
                NaiCall::DirectSale {
                    receiver,
                    credits: MAX_DIRECT_SALE_CREDITS,
                    payment_ref: format!("tx_{i}"),
                },
                &mut ctx,
            )
            .unwrap();
        }
        assert_eq!(storage.daily_minted, DEFAULT_DAILY_MINT_CAP);

        let overshoot = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::DirectSale {
                receiver,
                credits: MAX_DIRECT_SALE_CREDITS,
                payment_ref: "overshoot".into(),
            },
            &mut ctx,
        );
        assert_eq!(overshoot, Err(PalletError::InvalidArgs));
        assert_eq!(
            storage.daily_minted, DEFAULT_DAILY_MINT_CAP,
            "rejected mint must not bump daily_minted"
        );
    }

    #[test]
    fn daily_mint_window_resets_after_24h() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let authority: PeerId = [1u8; 32];
        let receiver: PeerId = [9u8; 32];
        let mut sys = SystemStorage::default();

        let mut ctx1 = ctx_at(&authority, &authority, 1_000, &mut sys);
        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::DirectSale {
                receiver,
                credits: MAX_DIRECT_SALE_CREDITS,
                payment_ref: "day1".into(),
            },
            &mut ctx1,
        )
        .unwrap();
        assert_eq!(storage.daily_minted, MAX_DIRECT_SALE_CREDITS);

        let mut sys2 = SystemStorage::default();
        let mut ctx2 = ctx_at(
            &authority,
            &authority,
            1_000 + DAILY_WINDOW_MS + 1,
            &mut sys2,
        );
        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::DirectSale {
                receiver,
                credits: MAX_DIRECT_SALE_CREDITS,
                payment_ref: "day2".into(),
            },
            &mut ctx2,
        )
        .unwrap();
        assert_eq!(
            storage.daily_minted, MAX_DIRECT_SALE_CREDITS,
            "window reset wiped previous day"
        );
    }

    #[test]
    fn set_daily_mint_cap_enqueues_then_applies_after_timelock() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let authority: PeerId = [1u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&authority, &authority, 0, &mut sys);

        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::SetDailyMintCap { cap: 200_000 },
            &mut ctx,
        )
        .unwrap();
        assert_eq!(
            storage.daily_mint_cap, 0,
            "timelock: cap must not change at submission time"
        );
        assert_eq!(storage.pending_changes.len(), 1);

        NaiPallet::drain_due_changes(&mut storage, PARAM_TIMELOCK_BLOCKS);
        assert_eq!(storage.daily_mint_cap, 0, "still pending one block early");

        NaiPallet::drain_due_changes(&mut storage, PARAM_TIMELOCK_BLOCKS + 1);
        assert_eq!(
            storage.daily_mint_cap, 200_000,
            "applied at maturity height"
        );
        assert!(storage.pending_changes.is_empty());

        let rejected = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::SetDailyMintCap { cap: 0 },
            &mut ctx,
        );
        assert_eq!(rejected, Err(PalletError::InvalidArgs));
    }

    #[test]
    fn direct_sale_at_cap_accepted() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let authority: PeerId = [1u8; 32];
        let receiver: PeerId = [9u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&authority, &authority, 0, &mut sys);

        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::DirectSale {
                receiver,
                credits: MAX_DIRECT_SALE_CREDITS,
                payment_ref: "exact_cap".to_string(),
            },
            &mut ctx,
        )
        .unwrap();
        assert_eq!(balances.balance_of(&receiver), MAX_DIRECT_SALE_CREDITS);
        assert_eq!(storage.total_minted, MAX_DIRECT_SALE_CREDITS);
    }

    #[test]
    fn direct_sale_fractional_ltc_accepted() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let authority: PeerId = [1u8; 32];
        let receiver: PeerId = [9u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&authority, &authority, 0, &mut sys);

        let half_quarter = NAI_CREDITS_PER_LTC / 4;
        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::DirectSale {
                receiver,
                credits: half_quarter,
                payment_ref: "quarter_ltc".to_string(),
            },
            &mut ctx,
        )
        .unwrap();
        assert_eq!(balances.balance_of(&receiver), half_quarter);
        assert_eq!(storage.total_minted, half_quarter);
    }

    #[test]
    fn set_direct_sale_wallet_persists() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let authority: PeerId = [1u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&authority, &authority, 0, &mut sys);
        assert!(storage.direct_sale_wallet.is_none(), "default is None");

        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::SetDirectSaleWallet {
                address: Some("ltc1qexampleaddressstring".to_string()),
            },
            &mut ctx,
        )
        .unwrap();
        assert_eq!(
            storage.direct_sale_wallet.as_deref(),
            Some("ltc1qexampleaddressstring")
        );

        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::SetDirectSaleWallet { address: None },
            &mut ctx,
        )
        .unwrap();
        assert!(storage.direct_sale_wallet.is_none());
    }

    #[test]
    fn set_direct_sale_wallet_rejects_empty_string_and_oversize() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let authority: PeerId = [1u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&authority, &authority, 0, &mut sys);

        let empty = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::SetDirectSaleWallet {
                address: Some(String::new()),
            },
            &mut ctx,
        );
        assert_eq!(empty, Err(PalletError::InvalidArgs));

        let oversize = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::SetDirectSaleWallet {
                address: Some("a".repeat(MAX_DIRECT_SALE_WALLET_LEN + 1)),
            },
            &mut ctx,
        );
        assert_eq!(oversize, Err(PalletError::InvalidArgs));

        assert!(storage.direct_sale_wallet.is_none());
    }

    #[test]
    fn pallet_root_changes_when_direct_sale_wallet_changes() {
        let mut storage = NaiStorage::default();
        let root_none = NaiPallet::pallet_root(&storage);
        storage.direct_sale_wallet = Some("ltc1qabc".to_string());
        let root_set = NaiPallet::pallet_root(&storage);
        assert_ne!(root_none, root_set);

        let mut alt = storage.clone();
        alt.direct_sale_wallet = Some("ltc1qdef".to_string());
        let root_alt = NaiPallet::pallet_root(&alt);
        assert_ne!(root_set, root_alt);
    }

    #[test]
    fn set_channel_session_cost_timelocked_then_inserts_and_clears() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let authority: PeerId = [1u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&authority, &authority, 0, &mut sys);

        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::SetChannelSessionCost {
                channel: "large_chat".into(),
                cost: 10,
            },
            &mut ctx,
        )
        .unwrap();
        assert!(
            !storage.top_tier_session_costs.contains_key("large_chat"),
            "timelock: cost change pending, not yet applied"
        );
        NaiPallet::drain_due_changes(&mut storage, PARAM_TIMELOCK_BLOCKS + 1);
        assert_eq!(
            storage.top_tier_session_costs.get("large_chat").copied(),
            Some(10)
        );

        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::SetChannelSessionCost {
                channel: "large_chat".into(),
                cost: 0,
            },
            &mut ctx,
        )
        .unwrap();
        NaiPallet::drain_due_changes(&mut storage, PARAM_TIMELOCK_BLOCKS + 1);
        assert!(!storage.top_tier_session_costs.contains_key("large_chat"));
    }

    #[test]
    fn set_channel_session_cost_rejects_invalid_channel() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let authority: PeerId = [1u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&authority, &authority, 0, &mut sys);

        let empty = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::SetChannelSessionCost {
                channel: String::new(),
                cost: 10,
            },
            &mut ctx,
        );
        assert_eq!(empty, Err(PalletError::InvalidArgs));

        let oversize = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::SetChannelSessionCost {
                channel: "x".repeat(MAX_CHANNEL_NAME_LEN + 1),
                cost: 10,
            },
            &mut ctx,
        );
        assert_eq!(oversize, Err(PalletError::InvalidArgs));
    }

    #[test]
    fn open_channel_session_burns_per_channel_cost() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let peer: PeerId = [7u8; 32];
        balances.mint(peer, 100);

        storage.top_tier_session_costs.insert("frontier".into(), 50);

        let mut sys2 = SystemStorage::default();
        let mut ctx_peer = ctx_at(&peer, &peer, 1_700_000_000_000, &mut sys2);
        let token = [42u8; 32];
        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::OpenChannelSession {
                channel: "frontier".into(),
                session_token: token,
            },
            &mut ctx_peer,
        )
        .unwrap();
        assert_eq!(balances.balance_of(&peer), 50);
        assert_eq!(storage.total_spent, 50);
        let s = storage.session(&token).unwrap();
        assert_eq!(s.owner, peer);
        assert_eq!(s.channel.as_deref(), Some("frontier"));
        assert_eq!(s.inferences_remaining, INFERENCES_PER_SESSION);
    }

    #[test]
    fn open_channel_session_unconfigured_channel_uses_default_cost() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let peer: PeerId = [7u8; 32];
        balances.mint(peer, 100);
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&peer, &peer, 0, &mut sys);

        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::OpenChannelSession {
                channel: "chat".into(),
                session_token: [1u8; 32],
            },
            &mut ctx,
        )
        .unwrap();
        assert_eq!(
            balances.balance_of(&peer),
            100 - DEFAULT_CHANNEL_SESSION_COST
        );
        let s = storage.session(&[1u8; 32]).unwrap();
        assert_eq!(s.channel.as_deref(), Some("chat"));
    }

    #[test]
    fn use_session_decrements_and_exhausts() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let peer: PeerId = [7u8; 32];
        balances.mint(peer, 100);
        let mut sys = SystemStorage::default();
        let now = 1_700_000_000_000_u64;
        let mut ctx = ctx_at(&peer, &peer, now, &mut sys);
        let token = [42u8; 32];

        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::Spend {
                session_token: token,
            },
            &mut ctx,
        )
        .unwrap();

        for used in 1..=INFERENCES_PER_SESSION {
            NaiPallet::apply_with_balances(
                &mut storage,
                &mut balances,
                NaiCall::UseSession {
                    session_token: token,
                },
                &mut ctx,
            )
            .unwrap();
            assert_eq!(
                storage.session(&token).unwrap().inferences_remaining,
                INFERENCES_PER_SESSION - used
            );
        }
        assert!(storage.session_expired(&token, now), "exhausted = expired");

        let overdraw = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::UseSession {
                session_token: token,
            },
            &mut ctx,
        );
        assert_eq!(overdraw, Err(PalletError::InvalidArgs));
    }

    #[test]
    fn use_session_rejects_non_owner() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let owner: PeerId = [7u8; 32];
        let thief: PeerId = [8u8; 32];
        balances.mint(owner, 100);
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&owner, &owner, 0, &mut sys);
        let token = [42u8; 32];

        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::Spend {
                session_token: token,
            },
            &mut ctx,
        )
        .unwrap();

        let mut sys2 = SystemStorage::default();
        let mut ctx_thief = ctx_at(&thief, &thief, 0, &mut sys2);
        let result = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::UseSession {
                session_token: token,
            },
            &mut ctx_thief,
        );
        assert_eq!(result, Err(PalletError::Unauthorized));
        assert_eq!(
            storage.session(&token).unwrap().inferences_remaining,
            INFERENCES_PER_SESSION,
            "non-owner must not consume inferences"
        );
    }

    #[test]
    fn use_session_rejects_after_ttl() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let peer: PeerId = [7u8; 32];
        balances.mint(peer, 100);
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&peer, &peer, 0, &mut sys);
        let token = [42u8; 32];

        NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::Spend {
                session_token: token,
            },
            &mut ctx,
        )
        .unwrap();

        let mut sys2 = SystemStorage::default();
        let mut ctx_late = ctx_at(&peer, &peer, SESSION_TTL_MS + 1, &mut sys2);
        let result = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::UseSession {
                session_token: token,
            },
            &mut ctx_late,
        );
        assert_eq!(result, Err(PalletError::InvalidArgs));
    }

    #[test]
    fn open_channel_session_insufficient_balance_rejected() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let peer: PeerId = [7u8; 32];
        balances.mint(peer, 5);
        storage
            .top_tier_session_costs
            .insert("large_chat".into(), 10);

        let mut sys2 = SystemStorage::default();
        let mut ctx_peer = ctx_at(&peer, &peer, 0, &mut sys2);
        let result = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::OpenChannelSession {
                channel: "large_chat".into(),
                session_token: [9u8; 32],
            },
            &mut ctx_peer,
        );
        assert_eq!(result, Err(PalletError::InsufficientBalance));
        assert_eq!(balances.balance_of(&peer), 5);
        assert!(storage.session(&[9u8; 32]).is_none());
    }

    #[test]
    fn direct_sale_zero_credits_rejected() {
        let mut storage = NaiStorage::default();
        let mut balances = BalancesStorage::default();
        let authority: PeerId = [1u8; 32];
        let mut sys = SystemStorage::default();
        let mut ctx = ctx_at(&authority, &authority, 0, &mut sys);

        let result = NaiPallet::apply_with_balances(
            &mut storage,
            &mut balances,
            NaiCall::DirectSale {
                receiver: [9u8; 32],
                credits: 0,
                payment_ref: "abc".to_string(),
            },
            &mut ctx,
        );
        assert_eq!(result, Err(PalletError::InvalidArgs));
    }
}
