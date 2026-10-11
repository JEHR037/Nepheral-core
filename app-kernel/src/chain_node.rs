// SPDX-License-Identifier: BUSL-1.1

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;

use core_crypto::chain_file::{
    strip_header as strip_chain_header, with_header as with_chain_header, CHAIN_GENERATION,
};
use core_crypto::identity::PeerId;
use mesh_network::protocol::message_types;

use crate::state::{now_ms, AppState, OpenSessionError};
use crate::wallet::{Confirm, INSUFFICIENT};

pub const PENDING_TX_CAP: usize = 4_096;
pub const PENDING_TX_PER_SIGNER_CAP: usize = 64;
pub const INFLIGHT_TX_TTL_MS: u64 = 5 * 60 * 1000;
pub const MAX_SYNC_BLOCKS: usize = 256;
pub const MAX_BLOCK_FUTURE_DRIFT_MS: u64 = 30_000;
pub const INFLIGHT_RESEND_EVERY: Duration = Duration::from_secs(15);
pub const INFLIGHT_RESEND_AFTER_MS: u64 = 20_000;
/// How often a node converts what it has earned into credits.
pub const AUTO_REDEEM_EVERY: Duration = Duration::from_secs(10 * 60);
/// Delay before the first attempt, so the chain has synced and the nonce the
/// redeem is signed with is current.
pub const AUTO_REDEEM_FIRST: Duration = Duration::from_secs(60);

/// Milliseconds until the next block slot starts, at least 50 so a loop
/// sleeping on it never spins at a boundary.
pub fn until_next_slot(now_ms: u64) -> u64 {
    use core_crypto::block::SLOT_DURATION_MS;
    (SLOT_DURATION_MS - now_ms % SLOT_DURATION_MS).max(50)
}

/// The height `ext` finalizes, if it is a `Finalize`.
pub fn finalized_height_of(ext: &core_crypto::Extrinsic) -> Option<u64> {
    use core_crypto::extrinsic::DecodedCall;
    use core_crypto::finality::FinalityCall;
    match DecodedCall::decode(ext.pallet_idx, &ext.args) {
        Ok(DecodedCall::Finality(FinalityCall::Finalize { justification })) => {
            Some(justification.height)
        }
        _ => None,
    }
}

/// Whether `ext` finalizes a height the chain has already finalized.
///
/// Such a justification can never apply. Nodes before 0.5.2 submitted one
/// from every validator that saw a quorum, and only the first to land was
/// valid; left in the inflight list or a producer's pool, each of the others
/// was rebroadcast and rejected over and over.
pub fn finalizes_no_later_than(ext: &core_crypto::Extrinsic, finalized: u64) -> bool {
    finalized_height_of(ext).is_some_and(|h| h <= finalized)
}

/// Whether to redeem automatically: on unless `NEPHERAL_AUTO_REDEEM` is 0,
/// false or off.
pub fn auto_redeem_enabled() -> bool {
    std::env::var("NEPHERAL_AUTO_REDEEM")
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off"
            )
        })
        .unwrap_or(true)
}

/// What a node should redeem now, if anything: everything it has earned,
/// provided that comes to at least one whole credit at the current ratio.
/// Asking for less mints nothing and only spends a nonce. Passing the full
/// amounts is safe - the pallet marks only what it converts as redeemed and
/// leaves the remainder for next time.
pub fn redeem_due(shares: u64, dimension: u64, shares_per_credit: u64) -> Option<(u64, u64)> {
    // The same fallback the pallet applies, so this never asks for a redeem
    // the pallet would turn down.
    let ratio = if shares_per_credit > 0 {
        shares_per_credit
    } else {
        core_crypto::pallets::contributions::SHARES_PER_CREDIT
    };
    (shares.saturating_add(dimension) / ratio >= 1).then_some((shares, dimension))
}

impl AppState {
    pub async fn next_nonce_for(&self, signer: &PeerId) -> u64 {
        let last = {
            let chain = self.chain.read().await;
            chain.system.nonces.get(signer).copied().unwrap_or(0)
        };
        let inflight_max = self
            .my_inflight_txs
            .lock()
            .await
            .iter()
            .filter(|(_, e)| e.signer == *signer && e.nonce > last)
            .map(|(_, e)| e.nonce)
            .max();
        inflight_max.unwrap_or(last) + 1
    }

    pub async fn sign_and_submit<C: serde::Serialize>(
        &self,
        pallet_idx: u8,
        call: &C,
    ) -> core_crypto::Extrinsic {
        let _guard = self.nonce_lock.lock().await;
        let me = *self.my_peer_id.read().await;
        let nonce = self.next_nonce_for(&me).await;
        let ext = core_crypto::Extrinsic::sign(&self.signing_key, pallet_idx, call, me, nonce);
        self.my_inflight_txs
            .lock()
            .await
            .push((now_ms(), ext.clone()));
        drop(_guard);
        if self.is_block_producer().await {
            self.queue_pending_tx(ext.clone()).await;
        }
        self.broadcast_chain_extrinsic(&ext).await;
        ext
    }

    pub async fn submit_signed_extrinsic(&self, ext: core_crypto::Extrinsic) {
        self.my_inflight_txs
            .lock()
            .await
            .push((now_ms(), ext.clone()));
        if self.is_block_producer().await {
            self.queue_pending_tx(ext.clone()).await;
        }
        self.broadcast_chain_extrinsic(&ext).await;
    }

    pub async fn request_redeem(&self, shares: u64, lottery_dimension: u64) {
        use core_crypto::pallets::contributions::ContributionsCall;
        if shares == 0 && lottery_dimension == 0 {
            return;
        }
        let me = *self.my_peer_id.read().await;
        let call = ContributionsCall::Redeem {
            receiver: me,
            shares,
            lottery_dimension,
        };
        self.sign_and_submit(core_crypto::extrinsic::PALLET_CONTRIBUTIONS, &call)
            .await;
        self.log(format!(
            "Redeem submitted (shares={shares}, dimension={lottery_dimension}) — pending block inclusion"
        ))
        .await;
    }

    /// Converts earned contribution into credits on a timer.
    ///
    /// Redeem used to be reachable only from a button in the desktop client,
    /// so a headless node - which is what serves the mesh - accrued lottery
    /// dimension receipt after receipt and never turned any of it into
    /// credits it could hold, spend or sell.
    pub async fn run_auto_redeem(self) {
        if self.sleep_or_shutdown(AUTO_REDEEM_FIRST).await {
            return;
        }
        loop {
            self.redeem_if_due().await;
            if self.sleep_or_shutdown(AUTO_REDEEM_EVERY).await {
                break;
            }
        }
    }

    /// One pass of the automatic redeem. Returns whether a redeem was sent.
    ///
    /// One at a time: while a redeem is in flight the chain still reports the
    /// whole amount as redeemable, and a second request for it would be
    /// rejected once the first lands.
    pub async fn redeem_if_due(&self) -> bool {
        if self.redeem_in_flight().await {
            return false;
        }
        let me = *self.my_peer_id.read().await;
        let due = {
            let chain = self.chain.read().await;
            let (shares, dim) = chain.redeemable(&me);
            redeem_due(shares, dim, chain.economy.current_shares_per_credit)
        };
        let Some((shares, dim)) = due else {
            return false;
        };
        tracing::info!(shares, dimension = dim, "Redeeming earned contribution");
        self.request_redeem(shares, dim).await;
        true
    }

    async fn redeem_in_flight(&self) -> bool {
        use core_crypto::extrinsic::DecodedCall;
        use core_crypto::pallets::contributions::ContributionsCall;
        let me = *self.my_peer_id.read().await;
        self.my_inflight_txs.lock().await.iter().any(|(_, e)| {
            e.signer == me
                && matches!(
                    DecodedCall::decode(e.pallet_idx, &e.args),
                    Ok(DecodedCall::Contributions(ContributionsCall::Redeem { .. }))
                )
        })
    }

    pub async fn handle_incoming_chain_tx(&self, ext: core_crypto::Extrinsic) -> bool {
        if ext.verify_signature().is_err() {
            return false;
        }
        let fresh = self
            .seen_chain_txs
            .lock()
            .await
            .insert(ext.hash_for_signing());
        if self.is_block_producer().await {
            let included = {
                let chain = self.chain.read().await;
                chain.system.nonces.get(&ext.signer).copied().unwrap_or(0) >= ext.nonce
            };
            if !included {
                self.queue_pending_tx(ext).await;
            }
        }
        fresh
    }

    pub async fn run_inflight_resend(self) {
        loop {
            if self.sleep_or_shutdown(INFLIGHT_RESEND_EVERY).await {
                break;
            }
            self.prune_inflight_txs().await;
            let now = now_ms();
            let stale: Vec<core_crypto::Extrinsic> = self
                .my_inflight_txs
                .lock()
                .await
                .iter()
                .filter(|(at, _)| now.saturating_sub(*at) >= INFLIGHT_RESEND_AFTER_MS)
                .map(|(_, e)| e.clone())
                .collect();
            if stale.is_empty() {
                continue;
            }
            let producer = self.is_block_producer().await;
            for ext in stale {
                if producer {
                    self.queue_pending_tx(ext.clone()).await;
                }
                self.broadcast_chain_extrinsic(&ext).await;
            }
        }
    }

    pub async fn broadcast_chain_extrinsic(&self, ext: &core_crypto::Extrinsic) {
        self.broadcast(message_types::CHAIN_TX, &ext.to_bytes(), None)
            .await;
    }

    pub async fn prune_pending_txs(&self) {
        let chain = self.chain.read().await;
        let finalized = chain.finalized().0;
        self.pending_txs.lock().await.retain(|e| {
            e.nonce > chain.system.nonces.get(&e.signer).copied().unwrap_or(0)
                && !finalizes_no_later_than(e, finalized)
        });
    }

    pub async fn prune_inflight_txs(&self) {
        let now = now_ms();
        let chain = self.chain.read().await;
        let finalized = chain.finalized().0;
        self.my_inflight_txs
            .lock()
            .await
            .retain(|(submitted_at, e)| {
                let last = chain.system.nonces.get(&e.signer).copied().unwrap_or(0);
                e.nonce > last
                    && now.saturating_sub(*submitted_at) < INFLIGHT_TX_TTL_MS
                    && !finalizes_no_later_than(e, finalized)
            });
    }

    pub async fn queue_pending_tx(&self, ext: core_crypto::Extrinsic) {
        let mut pending = self.pending_txs.lock().await;
        // One transaction per signer and nonce: the chain admits at most one,
        // and the producer's dry run consumes the nonce even for one that
        // fails, so a second copy would be refused as a replay. Only the
        // signer can have made either, and it reuses a nonce only after giving
        // up on the first transaction - a justification someone else already
        // landed, say - so the newer one is the one that can still apply.
        if let Some(slot) = pending
            .iter_mut()
            .find(|e| e.signer == ext.signer && e.nonce == ext.nonce)
        {
            *slot = ext;
            return;
        }
        if pending.len() >= PENDING_TX_CAP {
            return;
        }
        if pending.iter().filter(|e| e.signer == ext.signer).count() >= PENDING_TX_PER_SIGNER_CAP {
            return;
        }
        pending.push(ext);
    }

    pub(crate) async fn queue_authority_signed<C: serde::Serialize>(
        &self,
        pallet_idx: u8,
        call: &C,
    ) {
        let Some(auth_key) = self.authority_key.clone() else {
            return;
        };
        let signer: PeerId = auth_key.verifying_key().to_bytes();
        let nonce = {
            let last = self
                .chain
                .read()
                .await
                .system
                .nonces
                .get(&signer)
                .copied()
                .unwrap_or(0);
            let pending_max = self
                .pending_txs
                .lock()
                .await
                .iter()
                .filter(|e| e.signer == signer && e.nonce > last)
                .map(|e| e.nonce)
                .max();
            pending_max.unwrap_or(last) + 1
        };
        let ext = core_crypto::Extrinsic::sign(&auth_key, pallet_idx, call, signer, nonce);
        self.queue_pending_tx(ext).await;
    }

    /// Sends credits from the wallet, once its owner confirms.
    pub async fn transfer_credits(&self, receiver: PeerId, credits: u64) -> Result<(), String> {
        use core_crypto::extrinsic::PALLET_BALANCES;
        use core_crypto::pallets::balances::BalancesCall;

        self.require_unlocked().await?;
        let sender = self.my_account().await;
        let balance = self.chain.read().await.balance_of(&sender);
        if balance < credits {
            return Err(INSUFFICIENT.into());
        }
        self.confirm(Confirm::Transfer {
            to: hex::encode(receiver),
            amount: credits,
            balance,
        })
        .await?;
        let call = BalancesCall::Transfer {
            to: receiver,
            amount: credits,
        };
        self.sign_and_submit_as_wallet(PALLET_BALANCES, &call)
            .await?;
        Ok(())
    }

    /// Posts a sell listing. The credits leave the balance in the same block
    /// the listing appears, so the board never advertises what is not there.
    #[cfg(feature = "client")]
    pub async fn market_post_ask(
        &self,
        credits: u64,
        ask_sats: u64,
        ltc_address: String,
    ) -> Result<(), String> {
        use core_crypto::extrinsic::PALLET_MARKET;
        use core_crypto::pallets::market::MarketCall;

        self.require_unlocked().await?;
        let me = self.my_account().await;
        if self.chain.read().await.balance_of(&me) < credits {
            return Err(INSUFFICIENT.into());
        }
        self.confirm(Confirm::PostAsk {
            credits,
            ask_sats,
            ltc_address: ltc_address.clone(),
        })
        .await?;
        let call = MarketCall::PostAsk {
            credits,
            ask_sats,
            ltc_address,
        };
        self.sign_and_submit_as_wallet(PALLET_MARKET, &call).await?;
        Ok(())
    }

    #[cfg(feature = "client")]
    pub async fn market_post_bid(&self, credits: u64, bid_sats: u64) -> Result<(), String> {
        use core_crypto::extrinsic::PALLET_MARKET;
        use core_crypto::pallets::market::MarketCall;

        self.require_unlocked().await?;
        self.confirm(Confirm::PostBid { credits, bid_sats }).await?;
        let call = MarketCall::PostBid { credits, bid_sats };
        self.sign_and_submit_as_wallet(PALLET_MARKET, &call).await?;
        Ok(())
    }

    /// Claims, for this node's own wallet, that the Litecoin transaction
    /// `payment_ref` paid listing `ask_id`. The sale oracle then looks for it
    /// at the seller's address and releases the escrow once it confirms.
    #[cfg(feature = "client")]
    pub async fn market_claim_fill(&self, ask_id: u64, payment_ref: &str) -> Result<(), String> {
        use core_crypto::extrinsic::PALLET_MARKET;
        use core_crypto::pallets::market::MarketCall;

        self.require_unlocked().await?;
        let me = self.my_account().await;
        let payment_ref = payment_ref.trim().to_ascii_lowercase();
        claim_precheck(
            &*self.chain.read().await,
            &me,
            ask_id,
            &payment_ref,
            now_ms(),
        )?;
        self.confirm(Confirm::ClaimFill {
            ask_id,
            payment_ref: payment_ref.clone(),
        })
        .await?;
        let call = MarketCall::ClaimFill {
            ask_id,
            buyer: me,
            payment_ref,
        };
        self.sign_and_submit_as_wallet(PALLET_MARKET, &call).await?;
        Ok(())
    }

    /// Withdraws one of this node's own listings. `is_ask` picks which board.
    #[cfg(feature = "client")]
    pub async fn market_cancel(&self, id: u64, is_ask: bool) -> Result<(), String> {
        use core_crypto::extrinsic::PALLET_MARKET;
        use core_crypto::pallets::market::MarketCall;

        self.require_unlocked().await?;
        let me = self.my_account().await;
        {
            // Caught here only to give the user an immediate answer; the
            // pallet enforces ownership regardless of what is submitted.
            let chain = self.chain.read().await;
            let owned = if is_ask {
                chain.market.asks.get(&id).map(|a| a.seller) == Some(me)
            } else {
                chain.market.bids.get(&id).map(|b| b.buyer) == Some(me)
            };
            if !owned {
                return Err("that listing is not yours".into());
            }
        }
        self.confirm(Confirm::Cancel { id, is_ask }).await?;
        let call = if is_ask {
            MarketCall::CancelAsk { id }
        } else {
            MarketCall::CancelBid { id }
        };
        self.sign_and_submit_as_wallet(PALLET_MARKET, &call).await?;
        Ok(())
    }

    pub async fn open_session_onchain(
        &self,
        channel: Option<String>,
    ) -> Result<[u8; 32], OpenSessionError> {
        use core_crypto::pallets::nai::{
            NaiCall, DEFAULT_CHANNEL_SESSION_COST, MAX_CHANNEL_NAME_LEN, PRIORITY_SESSION_COST,
        };
        use rand::RngCore;

        if !self.is_unlocked().await {
            return Err(OpenSessionError::Locked);
        }
        let mut token = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut token);
        let me = self.my_account().await;

        let call = match channel {
            Some(ch) => {
                let trimmed = ch.trim().to_string();
                if trimmed.is_empty() || trimmed.len() > MAX_CHANNEL_NAME_LEN {
                    return Err(OpenSessionError::InvalidChannel);
                }
                NaiCall::OpenChannelSession {
                    channel: trimmed,
                    session_token: token,
                }
            }
            None => NaiCall::Spend {
                session_token: token,
            },
        };

        let (cost, balance) = {
            let chain = self.chain.read().await;
            let cost = match &call {
                NaiCall::OpenChannelSession { channel, .. } => chain
                    .nai
                    .top_tier_session_costs
                    .get(channel)
                    .copied()
                    .unwrap_or(DEFAULT_CHANNEL_SESSION_COST),
                _ => PRIORITY_SESSION_COST,
            };
            (cost, chain.balance_of(&me))
        };
        if balance < cost {
            return Err(OpenSessionError::InsufficientBalance);
        }
        let channel = match &call {
            NaiCall::OpenChannelSession { channel, .. } => Some(channel.clone()),
            _ => None,
        };
        if self
            .confirm(Confirm::OpenSession {
                channel,
                cost,
                balance,
            })
            .await
            .is_err()
        {
            return Err(OpenSessionError::Declined);
        }

        if self
            .sign_and_submit_as_wallet(core_crypto::extrinsic::PALLET_NAI, &call)
            .await
            .is_err()
        {
            return Err(OpenSessionError::Locked);
        }

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            {
                if self.chain.read().await.nai.sessions.contains_key(&token) {
                    return Ok(token);
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(OpenSessionError::NotIncluded);
            }
            let notify = self.chain_notify.clone();
            tokio::select! {
                _ = notify.notified() => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
            }
        }
    }

    async fn queue_nai_extrinsic(&self, call: core_crypto::pallets::nai::NaiCall) {
        self.queue_authority_signed(core_crypto::extrinsic::PALLET_NAI, &call)
            .await;
    }

    async fn queue_authority_extrinsic(
        &self,
        call: core_crypto::pallets::authority::AuthorityCall,
    ) {
        self.queue_authority_signed(core_crypto::extrinsic::PALLET_AUTHORITY, &call)
            .await;
    }

    /// Registers the Top-Tier inference nodes listed in
    /// `NEPHERAL_TOP_TIER_NODES` so the lottery routes channel requests to
    /// them. Format: `<hex pubkey>=<channel>[,<channel>...]`, entries
    /// separated by `;` — the pubkey is the node's mesh peer id.
    ///
    /// Registration goes through `AuthorityCall::Add`, which lists the node
    /// among the chain's authorities. Every authority is also a block producer
    /// in its own slots, signing with its mesh identity; a node that is
    /// offline only leaves its slots empty.
    ///
    /// Idempotent across restarts: a node already registered with the same
    /// channels is left alone; one registered with different channels is
    /// updated with `SetChannels`. Removing an entry from the variable does not
    /// unregister the node — submit `AuthorityCall::Remove` for that.
    pub async fn bootstrap_top_tier_nodes(&self) {
        use core_crypto::pallets::authority::AuthorityCall;

        if !self.am_i_authority() {
            return;
        }
        let Ok(raw) = std::env::var("NEPHERAL_TOP_TIER_NODES") else {
            return;
        };
        let (entries, errors) = parse_top_tier_nodes(&raw);
        for e in &errors {
            tracing::warn!(entry = %e, "NEPHERAL_TOP_TIER_NODES: entry ignored");
        }

        for (pubkey, channels) in entries {
            let fp = hex::encode(&pubkey[..8]);
            for c in &channels {
                if !API_ROUTABLE_CHANNELS.contains(&c.as_str()) {
                    tracing::warn!(
                        node_fp = %fp,
                        channel = %c,
                        routable = ?API_ROUTABLE_CHANNELS,
                        "Top-Tier channel is not one the local API routes to — nothing will \
                         request it unless a client targets it directly"
                    );
                }
            }
            let existing = self
                .chain
                .read()
                .await
                .authority
                .metadata
                .get(&pubkey)
                .map(|m| m.channels.clone());
            match existing {
                Some(current) if current == channels => {
                    tracing::info!(node_fp = %fp, ?channels, "Top-Tier node already registered — no-op");
                }
                Some(_) => {
                    tracing::info!(node_fp = %fp, ?channels, "Updating Top-Tier node channels via SetChannels");
                    self.queue_authority_extrinsic(AuthorityCall::SetChannels { pubkey, channels })
                        .await;
                }
                None => {
                    tracing::info!(node_fp = %fp, ?channels, "Registering Top-Tier node via Authority::Add");
                    self.queue_authority_extrinsic(AuthorityCall::Add {
                        pubkey,
                        channels,
                        // No attestation is checked today; zero says so
                        // rather than implying a verified measurement.
                        attestation_hash: [0u8; 32],
                    })
                    .await;
                }
            }
        }
    }

    /// Burns the founder's treasury down to `NEPHERAL_FOUNDER_KEEP` credits.
    ///
    /// An operator tool, meant to be set for one restart and removed: it
    /// burns whatever is above the target each time the node starts, so left
    /// in place it would also burn anything the founder later received.
    pub async fn bootstrap_founder_keep(&self) {
        let Ok(raw) = std::env::var("NEPHERAL_FOUNDER_KEEP") else {
            return;
        };
        match raw.trim().parse::<u64>() {
            Ok(keep) => {
                self.apply_founder_keep(keep).await;
            }
            Err(_) => {
                tracing::warn!("NEPHERAL_FOUNDER_KEEP must be a whole number of credits; ignored")
            }
        }
    }

    /// Queues a burn of the founder's balance above `keep`. Returns the amount
    /// burned; nothing from a node whose authority key is not the founder's.
    pub async fn apply_founder_keep(&self, keep: u64) -> u64 {
        use core_crypto::extrinsic::PALLET_BALANCES;
        use core_crypto::pallets::balances::BalancesCall;

        let Some(auth_key) = self.authority_key.clone() else {
            return 0;
        };
        let founder: PeerId = auth_key.verifying_key().to_bytes();
        let burn = {
            let chain = self.chain.read().await;
            if !chain.authority.is_founder(&founder) {
                tracing::warn!(
                    "NEPHERAL_FOUNDER_KEEP: this node's authority key is not the founder; ignored"
                );
                return 0;
            }
            burn_due(chain.balance_of(&founder), keep)
        };
        let Some(amount) = burn else {
            return 0;
        };
        tracing::info!(
            amount,
            keep,
            "Burning the founder treasury down to its target"
        );
        self.queue_authority_signed(PALLET_BALANCES, &BalancesCall::Burn { amount })
            .await;
        amount
    }

    /// Publishes the key allowed to mint direct sales, read from
    /// `NEPHERAL_SALE_ORACLE_PUBKEY` (hex ed25519 public key).
    ///
    /// The payment watcher runs as a separate service, outside this repository,
    /// and holds only that key, so the authority seed never has to be copied
    /// anywhere. Clearing the variable does not revoke: to revoke, submit
    /// `SetSaleOracle { pubkey: None }`.
    pub async fn bootstrap_sale_oracle(&self) {
        if !self.am_i_authority() {
            return;
        }
        let Ok(raw) = std::env::var("NEPHERAL_SALE_ORACLE_PUBKEY") else {
            tracing::info!(
                "NEPHERAL_SALE_ORACLE_PUBKEY not set — no direct-sale oracle announced this boot"
            );
            return;
        };
        let raw = raw.trim();
        if raw.is_empty() {
            return;
        }
        let pubkey: PeerId = match hex::decode(raw).ok().and_then(|b| b.try_into().ok()) {
            Some(pk) => pk,
            None => {
                tracing::warn!(
                    "NEPHERAL_SALE_ORACLE_PUBKEY is not 32 hex-encoded bytes — ignoring"
                );
                return;
            }
        };
        if self.chain.read().await.nai.sale_oracle == Some(pubkey) {
            tracing::info!(
                oracle_fp = %hex::encode(&pubkey[..8]),
                "Sale oracle already announced on chain — no-op"
            );
            return;
        }
        tracing::info!(
            oracle_fp = %hex::encode(&pubkey[..8]),
            "Announcing sale oracle via SetSaleOracle"
        );
        self.queue_nai_extrinsic(core_crypto::pallets::nai::NaiCall::SetSaleOracle {
            pubkey: Some(pubkey),
        })
        .await;
    }

    pub async fn bootstrap_direct_sale_wallet(&self) {
        if !self.am_i_authority() {
            return;
        }
        let configured = match std::env::var("NEPHERAL_LTC_DIRECT_SALE_ADDRESS") {
            Ok(s) => s.trim().to_string(),
            Err(_) => {
                tracing::info!(
                    "NEPHERAL_LTC_DIRECT_SALE_ADDRESS not set — direct sale wallet not announced this boot"
                );
                return;
            }
        };
        if configured.is_empty() {
            tracing::warn!("NEPHERAL_LTC_DIRECT_SALE_ADDRESS is empty — skipping announce");
            return;
        }
        let on_chain = self.chain.read().await.nai.direct_sale_wallet.clone();
        if on_chain.as_deref() == Some(configured.as_str()) {
            tracing::info!(
                addr = %configured,
                "Direct sale wallet already announced on chain — no-op"
            );
            return;
        }
        tracing::info!(
            addr = %configured,
            "Announcing direct sale wallet via SetDirectSaleWallet"
        );
        self.queue_nai_extrinsic(core_crypto::pallets::nai::NaiCall::SetDirectSaleWallet {
            address: Some(configured),
        })
        .await;
    }

    pub fn chain_file_path(&self) -> PathBuf {
        std::env::var("NEPHERAL_CHAIN_BIN_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| self.data_dir.join("chain.bin"))
    }

    pub async fn load_chain_from_disk(&self) {
        let path = self.chain_file_path();
        *self.chain_path.write().await = Some(path.clone());
        if !path.exists() {
            tracing::info!(path = %path.display(), "No chain file on disk — starting from genesis");
            return;
        }
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "Failed to read chain file");
                return;
            }
        };
        let blocks = match strip_chain_header(&data)
            .and_then(core_crypto::ChainState::deserialize_blocks)
        {
            Some(b) => b,
            None => {
                let quarantine = quarantine_path(&path);
                let _ = std::fs::rename(&path, &quarantine);
                tracing::warn!(
                    path = %path.display(),
                    quarantined = %quarantine.display(),
                    generation = CHAIN_GENERATION,
                    "Chain file unreadable or from another generation — moved aside, re-syncing from peers"
                );
                return;
            }
        };
        let count = blocks.len();
        // A node that started from a light proof replays its blocks on the
        // state it started from; every other node from genesis.
        let base = crate::light_sync::load_base(&path);
        let replayed = match base {
            Some(mut state) => {
                for block in blocks {
                    if let Err(e) = state.apply_block(block) {
                        tracing::warn!(error = ?e, "Stopping block replay at first failure");
                        break;
                    }
                }
                state
            }
            None => core_crypto::ChainState::replay_blocks(blocks),
        };
        let base_height = replayed.base.as_ref().map(|h| h.height).unwrap_or(0);
        let head = replayed.head_height();
        let applied = head.saturating_sub(base_height) as usize;
        if applied < count {
            tracing::warn!(
                blocks_in_file = count,
                replayed_height = head,
                "Chain replay stopped early — the rest is re-synced from peers"
            );
        }
        let persisted = if applied == count {
            (head, replayed.head_hash())
        } else {
            (0, [0u8; 32])
        };
        *self.chain.write().await = replayed;
        *self.chain_persisted.lock().await = persisted;
        tracing::info!(blocks = count, head, "Chain replayed from disk");
    }

    pub async fn persist_chain_now(&self) -> Result<(), String> {
        let Some(path) = self.chain_path.read().await.clone() else {
            return Ok(());
        };
        let mut persisted = self.chain_persisted.lock().await;
        let (data, append, head) = {
            let chain = self.chain.read().await;
            let head = (chain.head_height(), chain.head_hash());
            if head == *persisted {
                return Ok(());
            }
            let appendable = persisted.0 > 0
                && persisted.0 <= head.0
                && chain.hash_at(persisted.0) == Some(persisted.1)
                && path.exists();
            if appendable {
                (chain.serialize_blocks_after(persisted.0), true, head)
            } else {
                (with_chain_header(&chain.serialize_blocks()), false, head)
            }
        };
        tokio::task::spawn_blocking(move || {
            if append {
                append_durably(&path, &data)
            } else {
                write_atomically(&path, &data)
            }
        })
        .await
        .map_err(|e| format!("persist task: {e}"))??;
        *persisted = head;
        Ok(())
    }

    pub async fn run_chain_persist_task(self) {
        loop {
            let notified = self.chain_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.running().await {
                break;
            }
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
            if let Err(e) = self.persist_chain_now().await {
                tracing::warn!(error = %e, "Chain persist failed");
            }
        }
        let _ = self.persist_chain_now().await;
    }

    pub async fn slot_key(&self, slot: u64) -> Option<Arc<SigningKey>> {
        let primary = {
            let chain = self.chain.read().await;
            chain
                .validator_set()
                .primary_for_slot(slot)
                .map(|k| k.to_bytes())
        }?;
        if let Some(k) = &self.authority_key {
            if k.verifying_key().to_bytes() == primary {
                return Some(k.clone());
            }
        }
        if self.signing_key.verifying_key().to_bytes() == primary {
            return Some(self.signing_key.clone());
        }
        None
    }

    pub async fn is_block_producer(&self) -> bool {
        let chain = self.chain.read().await;
        let set = chain.validator_set();
        let mine = |k: &SigningKey| {
            set.authorities
                .iter()
                .any(|v| v.as_bytes() == k.verifying_key().as_bytes())
        };
        self.authority_key.as_deref().is_some_and(mine) || mine(&self.signing_key)
    }

    pub async fn requeue_extrinsics(&self, extrinsics: Vec<core_crypto::Extrinsic>) {
        if extrinsics.is_empty() {
            return;
        }
        let mut pending = self.pending_txs.lock().await;
        for ext in extrinsics {
            if pending.len() >= PENDING_TX_CAP {
                break;
            }
            if !pending
                .iter()
                .any(|e| e.signer == ext.signer && e.nonce == ext.nonce)
            {
                pending.push(ext);
            }
        }
    }

    pub async fn drain_block_candidates(&self) -> Vec<core_crypto::Extrinsic> {
        let mut pending = self.pending_txs.lock().await;
        pending.sort_by_key(|e| (e.signer, e.nonce));
        let take = pending.len().min(core_crypto::chain::MAX_BLOCK_EXTRINSICS);
        pending.drain(..take).collect()
    }

    pub async fn run_block_producer(self) {
        use core_crypto::block::{
            Block, BlockBody, BlockHeader, EARLY_BLOCK_EXT_THRESHOLD, SLOT_DURATION_MS,
        };
        use core_crypto::validators::authority_peer_id;
        const HEARTBEAT_EVERY_N_SLOTS: u64 = 30;

        if self.is_block_producer().await {
            tracing::info!("This node is in the validator set and produces blocks in its slots");
        }
        let mut last_sealed_slot: u64 = u64::MAX;
        let mut last_heartbeat_slot: u64 = u64::MAX;

        loop {
            if !self.running().await {
                break;
            }
            let now = now_ms();
            let slot = now / SLOT_DURATION_MS;
            let new_slot = slot != last_sealed_slot;
            if last_heartbeat_slot == u64::MAX {
                last_heartbeat_slot = slot;
            }
            let heartbeat_due =
                new_slot && slot.saturating_sub(last_heartbeat_slot) >= HEARTBEAT_EVERY_N_SLOTS;
            let key = if new_slot {
                self.slot_key(slot).await
            } else {
                None
            };
            let key_was_mine = key.is_some();
            if key.is_some() {
                self.queue_due_justification().await;
            }
            let ext_len = self.pending_txs.lock().await.len();
            let should_seal = key.is_some()
                && (ext_len > 0 || heartbeat_due || ext_len >= EARLY_BLOCK_EXT_THRESHOLD);
            if let (true, Some(key)) = (should_seal, key) {
                let author_peer = authority_peer_id(&key.verifying_key());
                let drained = self.drain_block_candidates().await;
                let (parent_hash, next_height, parent_ts) = {
                    let chain = self.chain.read().await;
                    (
                        chain.head_hash(),
                        chain.head_height() + 1,
                        chain.head_timestamp_ms().unwrap_or(0),
                    )
                };
                let timestamp_ms = now.max(parent_ts);
                // Never seal under the old rules once the new ones are due.
                let due = self.chain.read().await.upgrade.activation_for(timestamp_ms);
                if let core_crypto::pallets::upgrade::Activation::Unsupported { version } = due {
                    self.note_upgrade_required(version);
                    self.requeue_extrinsics(drained).await;
                    last_sealed_slot = slot;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                let block_slot = timestamp_ms / SLOT_DURATION_MS;
                if block_slot != slot {
                    self.requeue_extrinsics(drained).await;
                    last_sealed_slot = slot;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                let mut probe = self.chain.read().await.state_without_history();
                let extrinsics: Vec<core_crypto::Extrinsic> = drained
                    .into_iter()
                    .filter(|ext| {
                        match probe.apply_extrinsic(ext, next_height, timestamp_ms, &author_peer) {
                            Ok(()) => true,
                            // A justification another validator already
                            // landed; nodes before 0.5.2 still send one each.
                            Err(core_crypto::ChainError::BadJustification)
                                if finalizes_no_later_than(ext, probe.finalized().0) =>
                            {
                                tracing::debug!(signer_fp = %hex::encode(&ext.signer[..8]), nonce = ext.nonce, "Producer: stale justification dropped");
                                false
                            }
                            Err(e) => {
                                tracing::warn!(error = ?e, signer_fp = %hex::encode(&ext.signer[..8]), nonce = ext.nonce, "Producer: invalid extrinsic dropped");
                                false
                            }
                        }
                    })
                    .collect();
                let mut block = Block {
                    header: BlockHeader {
                        version: 1,
                        height: next_height,
                        slot: block_slot,
                        parent_hash,
                        state_root: [0u8; 32],
                        extrinsics_root: [0u8; 32],
                        lottery_root: [0u8; 32],
                        author: author_peer,
                        signature: [0u8; 64],
                        timestamp_ms,
                    },
                    body: BlockBody {
                        extrinsics,
                        lottery_results: Vec::new(),
                    },
                };
                block.header.extrinsics_root = block.extrinsics_root();
                block.header.lottery_root = block.lottery_root();
                let dry = {
                    let chain = self.chain.read().await;
                    let mut dry = chain.state_without_history();
                    if let Some(last) = chain.blocks.last() {
                        dry.blocks.push(last.clone());
                    }
                    dry
                };
                let mut dry = dry;
                match dry.apply_block_unverified(block.clone()) {
                    Ok(_) => block.header.state_root = dry.state_root(),
                    Err(e) => {
                        tracing::warn!(error = ?e, height = next_height, "Producer: dry-run failed — block dropped");
                        self.requeue_extrinsics(block.body.extrinsics).await;
                        last_sealed_slot = slot;
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                }
                block.header.sign(&key);
                let applied = self.chain.write().await.apply_block(block.clone());
                if let Err(e) = applied {
                    tracing::warn!(error = ?e, height = next_height, "Producer: apply failed — block dropped");
                    self.requeue_extrinsics(block.body.extrinsics).await;
                    last_sealed_slot = slot;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                if let Err(e) = self.persist_chain_now().await {
                    tracing::error!(error = %e, "Sealed block could not be persisted — not broadcasting it");
                    last_sealed_slot = slot;
                    continue;
                }
                tracing::info!(
                    height = next_height,
                    ext = block.body.extrinsics.len(),
                    "Block sealed"
                );
                self.broadcast(
                    message_types::BLOCK_BROADCAST,
                    &core_crypto::wire::encode(&block),
                    None,
                )
                .await;
                self.prune_inflight_txs().await;
                self.chain_notify.notify_waiters();
                last_sealed_slot = slot;
                last_heartbeat_slot = slot;
            }
            // In its own slot a producer polls, so a transaction arriving
            // mid-slot still makes this slot's block. Otherwise there is
            // nothing to do before the next slot: most nodes never produce,
            // and waking ten times a second for nothing costs a laptop or a
            // phone battery.
            let wait = if key_was_mine {
                Duration::from_millis(100)
            } else {
                Duration::from_millis(until_next_slot(now_ms()))
            };
            tokio::time::sleep(wait).await;
        }
    }

    pub async fn apply_remote_block(
        &self,
        block: core_crypto::Block,
    ) -> Result<(), core_crypto::ChainError> {
        if block.header.timestamp_ms > now_ms() + MAX_BLOCK_FUTURE_DRIFT_MS {
            return Err(core_crypto::ChainError::BadTimestamp);
        }
        let result = self.chain.write().await.apply_block(block);
        match &result {
            Ok(_) => {
                self.prune_pending_txs().await;
                self.prune_inflight_txs().await;
                self.chain_notify.notify_waiters();
            }
            Err(core_crypto::ChainError::UpgradeRequired { version }) => {
                self.note_upgrade_required(*version);
            }
            Err(_) => {}
        }
        result.map(|_| ())
    }

    /// Records that the chain moved to rules this build does not have, and
    /// says so once per version.
    pub fn note_upgrade_required(&self, version: u32) {
        use core_crypto::pallets::upgrade::SUPPORTED_RULES_VERSION;
        let before = self
            .upgrade_required
            .swap(version, std::sync::atomic::Ordering::Relaxed);
        if before != version {
            tracing::error!(
                version,
                supported = SUPPORTED_RULES_VERSION,
                "The network switched to chain rules this build does not implement. The node \
                 keeps serving the mesh but has stopped following the chain: install the update"
            );
        }
    }

    /// The chain rules version that stopped this node, if any.
    pub fn upgrade_required(&self) -> Option<u32> {
        match self
            .upgrade_required
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            0 => None,
            v => Some(v),
        }
    }

    pub async fn read_blocks_range(
        &self,
        from_height: u64,
        to_height: u64,
    ) -> Vec<core_crypto::Block> {
        let chain = self.chain.read().await;
        // A node that started from a light proof holds no blocks before it.
        let first = chain.blocks.first().map(|b| b.header.height).unwrap_or(1);
        let start = from_height.saturating_sub(first) as usize;
        chain
            .blocks
            .iter()
            .skip(start)
            .filter(|b| b.header.height >= from_height && b.header.height <= to_height)
            .take(MAX_SYNC_BLOCKS)
            .cloned()
            .collect()
    }

    pub async fn apply_block_batch(
        &self,
        blocks: Vec<core_crypto::Block>,
    ) -> (usize, Option<core_crypto::ChainError>) {
        let mut applied = 0usize;
        for block in blocks {
            let h = block.header.height;
            match self.apply_remote_block(block).await {
                Ok(()) => applied += 1,
                Err(core_crypto::ChainError::BadBlockHeight)
                    if h <= self.chain.read().await.head_height() => {}
                Err(e) => {
                    tracing::debug!(error = ?e, height = h, "Sync block not applied, stopping batch");
                    return (applied, Some(e));
                }
            }
        }
        (applied, None)
    }

    async fn fetch_blocks(
        conn: &quinn::Connection,
        from: u64,
    ) -> Result<Vec<core_crypto::Block>, String> {
        let (mut send, mut recv) = tokio::time::timeout(Duration::from_secs(10), conn.open_bi())
            .await
            .map_err(|_| "open timeout".to_string())?
            .map_err(|e| e.to_string())?;
        let mut req = vec![message_types::BLOCK_SYNC_REQUEST];
        req.extend_from_slice(&from.to_be_bytes());
        req.extend_from_slice(&u64::MAX.to_be_bytes());
        send.write_all(&req).await.map_err(|e| e.to_string())?;
        send.finish().map_err(|e| e.to_string())?;
        tokio::time::timeout(Duration::from_secs(60), async {
            let mut count_buf = [0u8; 4];
            recv.read_exact(&mut count_buf)
                .await
                .map_err(|e| e.to_string())?;
            let count = u32::from_be_bytes(count_buf) as usize;
            if count > MAX_SYNC_BLOCKS {
                return Err("sync response too long".to_string());
            }
            let mut blocks = Vec::with_capacity(count);
            for _ in 0..count {
                let mut len_buf = [0u8; 4];
                recv.read_exact(&mut len_buf)
                    .await
                    .map_err(|e| e.to_string())?;
                let len = u32::from_be_bytes(len_buf) as usize;
                if len > mesh_network::protocol::MAX_BLOCK_BYTES {
                    return Err("sync block too large".to_string());
                }
                let mut buf = vec![0u8; len];
                recv.read_exact(&mut buf).await.map_err(|e| e.to_string())?;
                let block: core_crypto::Block = core_crypto::wire::decode(&buf)
                    .ok_or_else(|| "sync block malformed".to_string())?;
                blocks.push(block);
            }
            Ok(blocks)
        })
        .await
        .map_err(|_| "sync timed out".to_string())?
    }

    pub async fn resolve_fork(&self, conn: &quinn::Connection) -> bool {
        let (head, head_hash) = {
            let chain = self.chain.read().await;
            (chain.head_height(), chain.head_hash())
        };
        let from = head
            .saturating_sub(core_crypto::chain::MAX_REORG_DEPTH)
            .max(1);
        let blocks = match Self::fetch_blocks(conn, from).await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!(error = %e, "Fork sync failed");
                return false;
            }
        };
        let candidate: Vec<core_crypto::Block> = {
            let chain = self.chain.read().await;
            blocks
                .into_iter()
                .skip_while(|b| chain.hash_at(b.header.height) == Some(b.header.block_hash()))
                .collect()
        };
        if candidate.is_empty() {
            return false;
        }
        let observed_final = self.observed_finality().await.0;
        let orphaned = {
            let mut chain = self.chain.write().await;
            if chain.head_hash() != head_hash {
                return false;
            }
            match chain.try_reorg(candidate, observed_final) {
                Ok(orphaned) => {
                    tracing::warn!(
                        from_height = head,
                        to_height = chain.head_height(),
                        orphaned = orphaned.len(),
                        "Switched to a longer fork"
                    );
                    orphaned
                }
                Err(e) => {
                    tracing::debug!(error = ?e, "Peer fork not adopted");
                    return false;
                }
            }
        };
        self.requeue_extrinsics(orphaned).await;
        self.prune_pending_txs().await;
        self.prune_inflight_txs().await;
        if let Err(e) = self.persist_chain_now().await {
            tracing::warn!(error = %e, "Chain persist after reorg failed");
        }
        self.chain_notify.notify_waiters();
        true
    }

    pub async fn request_block_sync(&self, conn: quinn::Connection) {
        // A light node starts from a proof (run_light_bootstrap), never by
        // fetching every block since genesis.
        if self.awaits_light_proof().await {
            return;
        }
        let state = self.clone();
        tokio::spawn(async move {
            let mut fork_attempted = false;
            loop {
                let from = state.chain.read().await.head_height() + 1;
                match Self::fetch_blocks(&conn, from).await {
                    Ok(blocks) if !blocks.is_empty() => {
                        let n = blocks.len();
                        let (applied, err) = state.apply_block_batch(blocks).await;
                        tracing::info!(applied, from, "Block sync batch applied");
                        if matches!(err, Some(core_crypto::ChainError::BadParentHash))
                            && !fork_attempted
                        {
                            fork_attempted = true;
                            if state.resolve_fork(&conn).await {
                                continue;
                            }
                            break;
                        }
                        if applied == 0 || n < MAX_SYNC_BLOCKS {
                            break;
                        }
                    }
                    Ok(_) => break,
                    Err(e) => {
                        tracing::debug!(error = %e, "Block sync failed");
                        break;
                    }
                }
            }
        });
    }

    pub async fn serve_block_sync(
        &self,
        send: &mut quinn::SendStream,
        from: u64,
        to: u64,
    ) -> Result<(), String> {
        let blocks = self.read_blocks_range(from, to).await;
        send.write_all(&(blocks.len() as u32).to_be_bytes())
            .await
            .map_err(|e| e.to_string())?;
        for block in &blocks {
            let bytes = core_crypto::wire::encode(block);
            send.write_all(&(bytes.len() as u32).to_be_bytes())
                .await
                .map_err(|e| e.to_string())?;
            send.write_all(&bytes).await.map_err(|e| e.to_string())?;
        }
        let _ = send.finish();
        Ok(())
    }

    pub async fn run_chain_status_log(self) {
        use core_crypto::pallets::upgrade::SUPPORTED_RULES_VERSION;
        let mut announced: Option<u32> = None;
        loop {
            if self.sleep_or_shutdown(Duration::from_secs(60)).await {
                break;
            }
            let chain = self.chain.read().await;
            tracing::info!(
                network = self.network.name(),
                height = chain.head_height(),
                head = %hex::encode(&chain.head_hash()[..4]),
                state_root = %hex::encode(&chain.state_root()[..4]),
                rules = chain.rules_version(),
                "chain status",
            );
            if let Some(s) = &chain.upgrade.scheduled {
                if s.version > SUPPORTED_RULES_VERSION {
                    tracing::warn!(
                        version = s.version,
                        supported = SUPPORTED_RULES_VERSION,
                        activate_at_ms = s.activate_at_ms,
                        release = %hex::encode(s.release),
                        "Chain rules upgrade scheduled that this build does not implement: \
                         update before it activates, or the node stops following the chain then"
                    );
                } else if announced != Some(s.version) {
                    announced = Some(s.version);
                    tracing::info!(
                        version = s.version,
                        activate_at_ms = s.activate_at_ms,
                        "Chain rules upgrade scheduled; this build is ready for it"
                    );
                }
            }
        }
    }
}

fn quarantine_path(path: &Path) -> PathBuf {
    let stamp = now_ms();
    path.with_extension(format!("corrupt-{stamp}"))
}

fn append_durably(path: &Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| format!("open for append: {e}"))?;
    f.write_all(data).map_err(|e| format!("append: {e}"))?;
    f.sync_all().map_err(|e| format!("fsync: {e}"))
}

pub(crate) fn write_atomically(path: &Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("chain path has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
    let tmp = path.with_extension("bin.tmp");
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| format!("create tmp: {e}"))?;
        f.write_all(data).map_err(|e| format!("write tmp: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync tmp: {e}"))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

const API_ROUTABLE_CHANNELS: [&str; 3] = ["chat", "large_chat", "frontier"];

/// What the chain would refuse about a buyer's claim, said before it is
/// signed: a refused extrinsic shows the buyer nothing at all, and someone
/// who has just sent money needs to know why it is not moving.
pub fn claim_precheck(
    chain: &core_crypto::ChainState,
    buyer: &PeerId,
    ask_id: u64,
    payment_ref: &str,
    now_ms: u64,
) -> Result<(), String> {
    if payment_ref.len() != 64 || !payment_ref.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("a Litecoin transaction id is 64 hexadecimal characters".into());
    }
    let ask = chain
        .market
        .asks
        .get(&ask_id)
        .ok_or("that listing is no longer open")?;
    if &ask.seller == buyer {
        return Err("you cannot buy your own listing".into());
    }
    if chain.market.fills.contains_key(payment_ref) || chain.market.claims.contains_key(payment_ref)
    {
        return Err("that payment has already been claimed".into());
    }
    if chain.market.claims.values().any(|c| c.ask_id == ask_id) {
        return Err("someone has already claimed this listing and it is being confirmed".into());
    }
    // The buyer's standing counts the nodes working for its wallet.
    let group = chain.identity.group_of(buyer);
    core_crypto::pallets::market::check_eligible_group(&chain.contributions, &group, now_ms)
        .map_err(|e| format!("not able to trade yet: {e}"))
}

/// How much of `balance` to burn to leave `keep`, if any.
fn burn_due(balance: u64, keep: u64) -> Option<u64> {
    let excess = balance.saturating_sub(keep);
    (excess > 0).then_some(excess)
}

fn parse_top_tier_nodes(raw: &str) -> (Vec<(PeerId, Vec<String>)>, Vec<String>) {
    use core_crypto::pallets::nai::MAX_CHANNEL_NAME_LEN;

    let mut entries: Vec<(PeerId, Vec<String>)> = Vec::new();
    let mut errors = Vec::new();

    for entry in raw.split(';').map(str::trim).filter(|e| !e.is_empty()) {
        let Some((key_hex, chans)) = entry.split_once('=') else {
            errors.push(format!(
                "{entry}: expected <hex pubkey>=<channel>[,<channel>]"
            ));
            continue;
        };
        let pubkey: PeerId = match hex::decode(key_hex.trim())
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
        {
            Some(pk) => pk,
            None => {
                errors.push(format!("{entry}: pubkey is not 32 hex-encoded bytes"));
                continue;
            }
        };
        if ed25519_dalek::VerifyingKey::from_bytes(&pubkey).is_err() {
            errors.push(format!("{entry}: pubkey is not a valid ed25519 key"));
            continue;
        }
        let mut channels: Vec<String> = Vec::new();
        let mut bad_channel = None;
        for c in chans.split(',').map(str::trim).filter(|c| !c.is_empty()) {
            let ok = c.len() <= MAX_CHANNEL_NAME_LEN
                && c.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            if !ok {
                bad_channel = Some(c.to_string());
                break;
            }
            if !channels.iter().any(|x| x == c) {
                channels.push(c.to_string());
            }
        }
        if let Some(c) = bad_channel {
            errors.push(format!(
                "{entry}: channel '{c}' must be 1..={MAX_CHANNEL_NAME_LEN} of [a-z0-9_]"
            ));
            continue;
        }
        if channels.is_empty() {
            errors.push(format!("{entry}: no channels listed"));
            continue;
        }
        if entries.iter().any(|(pk, _)| *pk == pubkey) {
            errors.push(format!("{entry}: node listed twice; first entry kept"));
            continue;
        }
        entries.push((pubkey, channels));
    }
    (entries, errors)
}

#[cfg(test)]
mod top_tier_env_tests {
    use super::*;

    fn key_hex(seed: u8) -> String {
        hex::encode(
            ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .to_bytes(),
        )
    }

    #[test]
    fn one_node_one_channel() {
        let (entries, errors) = parse_top_tier_nodes(&format!("{}=chat", key_hex(1)));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, vec!["chat".to_string()]);
    }

    #[test]
    fn several_nodes_several_channels_with_whitespace() {
        let raw = format!(
            " {} = chat , large_chat ; {}=frontier ;",
            key_hex(1),
            key_hex(2)
        );
        let (entries, errors) = parse_top_tier_nodes(&raw);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].1,
            vec!["chat".to_string(), "large_chat".to_string()]
        );
        assert_eq!(entries[1].1, vec!["frontier".to_string()]);
    }

    #[test]
    fn a_bad_entry_does_not_discard_the_good_ones() {
        let raw = format!("nothex=chat;{}=chat", key_hex(3));
        let (entries, errors) = parse_top_tier_nodes(&raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn malformed_entries_are_rejected_with_a_reason() {
        let k = key_hex(4);
        for raw in [
            k.clone(),                    // no '='
            format!("{k}="),              // no channels
            format!("{k}=Chat"),          // uppercase
            format!("{k}=chat-large"),    // hyphen
            format!("{}=chat", &k[..62]), // short key
        ] {
            let (entries, errors) = parse_top_tier_nodes(&raw);
            assert!(entries.is_empty(), "{raw} should be rejected");
            assert_eq!(errors.len(), 1, "{raw} should report one error");
        }
    }

    #[test]
    fn duplicate_channels_collapse_and_duplicate_nodes_keep_the_first() {
        let k = key_hex(5);
        let (entries, errors) = parse_top_tier_nodes(&format!("{k}=chat,chat;{k}=frontier"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, vec!["chat".to_string()]);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn the_routable_channels_match_what_the_api_requests() {
        // If this list and the API's channel names drift, a registered node
        // would be warned about as unroutable while serving fine, or the
        // reverse. The API maps chat, large_chat and frontier.
        assert_eq!(API_ROUTABLE_CHANNELS, ["chat", "large_chat", "frontier"]);
    }
}

#[cfg(test)]
mod claim_precheck_tests {
    use super::claim_precheck;
    use core_crypto::pallets::market::{Ask, Claim, MIN_DAYS_ACTIVE, MIN_SHARES, MIN_WINS};
    use core_crypto::ChainState;

    const NOW: u64 = 1_790_000_000_000;
    const SELLER: [u8; 32] = [1u8; 32];
    const BUYER: [u8; 32] = [2u8; 32];
    const TXID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    fn board() -> ChainState {
        let mut s = ChainState::default();
        s.market.asks.insert(
            0,
            Ask {
                id: 0,
                seller: SELLER,
                credits: 400,
                ask_sats: 8_000_000,
                ltc_address: "ltc1qseller".into(),
                created_at_ms: NOW,
            },
        );
        s.contributions
            .first_seen_ms
            .insert(BUYER, NOW - (MIN_DAYS_ACTIVE + 1) * 24 * 60 * 60 * 1_000);
        s.contributions.share_counts.insert(BUYER, MIN_SHARES);
        s.contributions.wins_by_peer.insert(BUYER, MIN_WINS);
        s
    }

    #[test]
    fn a_buyer_with_standing_may_claim_an_open_listing() {
        assert_eq!(claim_precheck(&board(), &BUYER, 0, TXID, NOW), Ok(()));
    }

    #[test]
    fn what_the_chain_would_refuse_is_said_first() {
        let s = board();
        assert!(claim_precheck(&s, &BUYER, 0, "not-a-txid", NOW).is_err());
        assert!(
            claim_precheck(&s, &BUYER, 9, TXID, NOW).is_err(),
            "no such listing"
        );
        assert!(
            claim_precheck(&s, &SELLER, 0, TXID, NOW).is_err(),
            "own listing"
        );
        let newcomer = [3u8; 32];
        let why = claim_precheck(&s, &newcomer, 0, TXID, NOW).unwrap_err();
        assert!(why.contains("not able to trade yet"), "{why}");
    }

    #[test]
    fn a_listing_or_payment_already_claimed_is_refused() {
        let mut s = board();
        s.market.claims.insert(
            TXID.into(),
            Claim {
                ask_id: 0,
                buyer: BUYER,
                claimed_by: BUYER,
                claimed_at_ms: NOW,
                attempts: 0,
                next_attempt_at_ms: NOW,
            },
        );
        assert!(
            claim_precheck(&s, &BUYER, 0, TXID, NOW).is_err(),
            "same payment"
        );
        let other = "ff".repeat(32);
        assert!(
            claim_precheck(&s, &BUYER, 0, &other, NOW).is_err(),
            "same listing"
        );
    }
}

#[cfg(test)]
mod founder_keep_tests {
    use super::burn_due;

    #[test]
    fn only_the_excess_over_the_target_is_burned() {
        assert_eq!(burn_due(996_016, 100_000), Some(896_016));
        assert_eq!(burn_due(100_000, 100_000), None);
        assert_eq!(burn_due(5, 100_000), None, "never below what is there");
    }
}

#[cfg(test)]
mod pending_pool_tests {
    use super::*;
    use core_crypto::extrinsic::PALLET_BALANCES;
    use core_crypto::pallets::balances::BalancesCall;

    fn state() -> AppState {
        let key = ed25519_dalek::SigningKey::from_bytes(&[3u8; 32]);
        let identity =
            core_crypto::NodeIdentity::from_mnemonic(&core_crypto::NodeMnemonic::generate())
                .unwrap();
        AppState::new(
            "127.0.0.1:0".parse().unwrap(),
            Vec::new(),
            key,
            crate::boot::tls_identity(&identity),
        )
    }

    fn transfer(amount: u64, nonce: u64) -> core_crypto::Extrinsic {
        let key = ed25519_dalek::SigningKey::from_bytes(&[4u8; 32]);
        core_crypto::Extrinsic::sign(
            &key,
            PALLET_BALANCES,
            &BalancesCall::Transfer {
                to: [9u8; 32],
                amount,
            },
            key.verifying_key().to_bytes(),
            nonce,
        )
    }

    #[tokio::test]
    async fn a_reused_nonce_replaces_what_was_pending_under_it() {
        let st = state();
        st.queue_pending_tx(transfer(1, 7)).await;
        st.queue_pending_tx(transfer(2, 7)).await;
        let pending = st.pending_txs.lock().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].hash_for_signing(),
            transfer(2, 7).hash_for_signing()
        );
    }

    #[tokio::test]
    async fn a_reused_nonce_is_still_news_to_gossip() {
        // The node that saw the first transaction under a nonce must still
        // pass on a different one under the same nonce.
        let st = state();
        assert!(st.handle_incoming_chain_tx(transfer(1, 7)).await);
        assert!(
            !st.handle_incoming_chain_tx(transfer(1, 7)).await,
            "an echo is not"
        );
        assert!(st.handle_incoming_chain_tx(transfer(2, 7)).await);
    }
}

#[cfg(test)]
mod slot_wait_tests {
    use super::until_next_slot;
    use core_crypto::block::SLOT_DURATION_MS;

    #[test]
    fn a_node_sleeps_to_the_next_slot_boundary() {
        assert_eq!(until_next_slot(10 * SLOT_DURATION_MS), SLOT_DURATION_MS);
        assert_eq!(
            until_next_slot(10 * SLOT_DURATION_MS + 500),
            SLOT_DURATION_MS - 500
        );
        assert_eq!(
            until_next_slot(11 * SLOT_DURATION_MS - 10),
            50,
            "never a busy spin"
        );
    }
}

#[cfg(test)]
mod stale_finalize_tests {
    use super::finalizes_no_later_than;
    use core_crypto::extrinsic::{PALLET_BALANCES, PALLET_FINALITY};
    use core_crypto::finality::{FinalityCall, FinalityVote, Justification};

    fn finalize_at(height: u64) -> core_crypto::Extrinsic {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let vote = FinalityVote::sign(&key, height, [1u8; 32]);
        let justification = Justification::from_votes(height, [1u8; 32], &[vote]);
        core_crypto::Extrinsic::sign(
            &key,
            PALLET_FINALITY,
            &FinalityCall::Finalize { justification },
            key.verifying_key().to_bytes(),
            1,
        )
    }

    #[test]
    fn a_justification_the_chain_has_passed_is_stale() {
        assert!(finalizes_no_later_than(&finalize_at(16), 16));
        assert!(finalizes_no_later_than(&finalize_at(16), 32));
    }

    #[test]
    fn a_justification_ahead_of_the_chain_is_kept() {
        assert!(!finalizes_no_later_than(&finalize_at(32), 16));
    }

    #[test]
    fn other_calls_are_never_treated_as_stale_justifications() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let transfer = core_crypto::Extrinsic::sign(
            &key,
            PALLET_BALANCES,
            &core_crypto::pallets::balances::BalancesCall::Transfer {
                to: [2u8; 32],
                amount: 1,
            },
            key.verifying_key().to_bytes(),
            1,
        );
        assert!(!finalizes_no_later_than(&transfer, u64::MAX));
    }
}

#[cfg(test)]
mod auto_redeem_tests {
    use super::redeem_due;

    #[test]
    fn nothing_is_redeemed_below_one_whole_credit() {
        assert_eq!(redeem_due(0, 0, 100), None);
        assert_eq!(redeem_due(40, 59, 100), None);
    }

    #[test]
    fn everything_earned_is_asked_for_once_it_makes_a_credit() {
        assert_eq!(redeem_due(0, 917, 100), Some((0, 917)));
        assert_eq!(redeem_due(60, 40, 100), Some((60, 40)));
    }

    #[test]
    fn a_zero_ratio_falls_back_the_way_the_pallet_does() {
        use core_crypto::pallets::contributions::SHARES_PER_CREDIT;
        assert_eq!(redeem_due(0, SHARES_PER_CREDIT - 1, 0), None);
        assert_eq!(
            redeem_due(0, SHARES_PER_CREDIT, 0),
            Some((0, SHARES_PER_CREDIT))
        );
    }
}
