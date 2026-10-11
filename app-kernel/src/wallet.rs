// SPDX-License-Identifier: BUSL-1.1
//! The wallet, apart from the node that works for it.
//!
//! A node signs all the time - greetings, lottery claims, answers, receipts,
//! its own redeems - so its key opens without anyone typing a password. The
//! wallet holds the funds, so its key is in memory only while its owner has
//! unlocked it, and leaves memory when it is locked again: by hand, or after
//! a stretch of inactivity. The chain links the two (pallet 4, Identity):
//! what the node earns lands in the wallet, and the node may use the
//! sessions the wallet paid for.
//!
//! Locked, the node keeps serving the network, and the local API keeps
//! answering whoever presents its token - checked against a hash, so the
//! token is not on disk. Everything a person does in the app - chatting,
//! spending, trading, voting - waits for the unlock.

use std::sync::atomic::Ordering;

use core_crypto::identity::PeerId;
use core_crypto::pallets::identity::{sign_delegation, IdentityCall};
use ed25519_dalek::SigningKey;

use crate::state::{now_ms, AppState};

/// Returned by everything that needs the wallet while it is locked. The
/// front-ends match on the prefix.
pub const LOCKED: &str = "locked: unlock the wallet first";

/// Returned when the owner does not confirm. The front-ends match on it.
pub const DECLINED: &str = "declined: not confirmed";

pub const INSUFFICIENT: &str = "insufficient balance";

/// How long the wallet stays open without activity, unless configured.
pub const DEFAULT_AUTO_LOCK_MINUTES: u64 = 15;

/// What the wallet's owner confirms before it signs. The desktop shows it in
/// a native dialog, which the page cannot answer on the owner's behalf; a
/// headless node asks at its command line before calling in.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Confirm {
    Transfer {
        to: String,
        amount: u64,
        balance: u64,
    },
    OpenSession {
        channel: Option<String>,
        cost: u64,
        balance: u64,
    },
    PostAsk {
        credits: u64,
        ask_sats: u64,
        ltc_address: String,
    },
    PostBid {
        credits: u64,
        bid_sats: u64,
    },
    ClaimFill {
        ask_id: u64,
        payment_ref: String,
    },
    Cancel {
        id: u64,
        is_ask: bool,
    },
    Propose {
        action: String,
    },
    Vote {
        id: u64,
        aye: bool,
    },
    Enact {
        id: u64,
    },
}

/// Asks the owner; resolves to whether they confirmed.
pub type ConfirmHook = std::sync::Arc<
    dyn Fn(Confirm) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send
        + Sync,
>;

/// The wallet's key, in memory while unlocked. `SigningKey` wipes itself
/// when dropped.
pub struct WalletKey {
    signing_key: SigningKey,
    pub peer_id: PeerId,
}

impl WalletKey {
    /// A key for one purpose of this wallet (e.g. "conversations"): the
    /// wallet's secret is never used directly, and keys for different
    /// purposes are unrelated.
    pub fn derive_key(&self, purpose: &str) -> zeroize::Zeroizing<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"nepheral/");
        h.update(purpose.as_bytes());
        h.update(b"/v1\0");
        h.update(self.signing_key.to_bytes());
        zeroize::Zeroizing::new(h.finalize().into())
    }
}

/// A new random token for the local API. It lives in the vault with the
/// wallet, so the API answers only while the wallet is unlocked.
pub fn new_api_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// What the API checks a presented token against. A token is 192 random
/// bits, so a plain hash cannot be reversed or guessed.
pub fn api_token_hash(token: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::new()
        .chain_update(b"nepheral/api-token/v1")
        .chain_update(token.trim().as_bytes())
        .finalize()
        .into()
}

/// The API token's hash from config.json. A config from before the vault
/// kept the token itself: it is hashed, and the plain copy removed.
pub fn load_api_token_hash() -> Option<[u8; 32]> {
    let cfg = crate::nepheral_config::load_config()?;
    if let Some(plain) = cfg
        .local_api_server_token
        .as_deref()
        .filter(|t| t.trim().len() >= 32)
    {
        let hash = api_token_hash(plain);
        if store_api_token_hash(&hash).is_ok() {
            tracing::info!("API token in config.json replaced by its hash");
        }
        return Some(hash);
    }
    let raw = hex::decode(cfg.local_api_server_token_hash?.trim()).ok()?;
    <[u8; 32]>::try_from(raw).ok()
}

/// Writes the token's hash to config.json and drops any plain copy.
pub fn store_api_token_hash(hash: &[u8; 32]) -> Result<(), String> {
    let hex_hash = hex::encode(hash);
    crate::nepheral_config::update(move |c| {
        c.local_api_server_token_hash = Some(hex_hash);
        c.local_api_server_token = None;
    })
    .map(|_| ())
}

impl WalletKey {
    pub fn from_mnemonic(mnemonic: &core_crypto::NodeMnemonic) -> Self {
        let signing_key = SigningKey::from_bytes(&mnemonic.to_seed());
        let peer_id = signing_key.verifying_key().to_bytes();
        Self {
            signing_key,
            peer_id,
        }
    }

    /// A wallet whose key is already in hand - a node that is its own
    /// wallet, as installs from before the split were.
    pub fn from_signing_key(signing_key: SigningKey) -> Self {
        let peer_id = signing_key.verifying_key().to_bytes();
        Self {
            signing_key,
            peer_id,
        }
    }

    /// The address of the wallet a phrase opens, without keeping its key.
    pub fn address_of(mnemonic: &core_crypto::NodeMnemonic) -> PeerId {
        Self::from_mnemonic(mnemonic).peer_id
    }
}

/// Where a delegation stands, for the front-ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delegation {
    /// The node is its own wallet (an install from before the split).
    SameKey,
    /// On chain: the node works for this wallet.
    Linked,
    /// Submitted, not yet in a block.
    Pending,
    /// Needs the wallet unlocked to be signed.
    Unsigned,
    /// The chain says the node works for a different wallet.
    Elsewhere,
}

impl AppState {
    /// Records activity, which keeps the wallet from locking itself.
    pub fn touch(&self) {
        self.last_activity_ms.store(now_ms(), Ordering::Relaxed);
    }

    pub async fn is_unlocked(&self) -> bool {
        self.wallet_key.read().await.is_some()
    }

    pub async fn require_unlocked(&self) -> Result<(), String> {
        if self.is_unlocked().await {
            self.touch();
            Ok(())
        } else {
            Err(LOCKED.into())
        }
    }

    /// Who confirms the wallet's actions; `None` lets them through, for a
    /// caller that has asked already (the headless command line).
    pub fn set_confirm_hook(&self, hook: Option<ConfirmHook>) {
        *self.confirm_hook.write().expect("confirm hook lock") = hook;
    }

    pub async fn confirm(&self, what: Confirm) -> Result<(), String> {
        let hook = self.confirm_hook.read().expect("confirm hook lock").clone();
        match hook {
            Some(ask) if !ask(what).await => Err(DECLINED.into()),
            _ => {
                self.touch();
                Ok(())
            }
        }
    }

    /// The account this node acts for: its wallet, or the node itself when
    /// no wallet is configured.
    pub async fn my_account(&self) -> PeerId {
        match *self.wallet_address.read().await {
            Some(w) => w,
            None => *self.my_peer_id.read().await,
        }
    }

    /// Opens the wallet: its key and the API token (to show) go into
    /// memory, the token's hash is put right if it was missing, and the
    /// delegation is signed if the chain does not have it yet.
    pub async fn unlock_wallet(&self, key: WalletKey, api_token: Option<String>) {
        *self.wallet_address.write().await = Some(key.peer_id);
        *self.wallet_key.write().await = Some(key);
        if let Some(token) = api_token.as_deref() {
            let hash = api_token_hash(token);
            let stored = *self.api_token_hash.read().expect("api hash lock");
            if stored != Some(hash) {
                *self.api_token_hash.write().expect("api hash lock") = Some(hash);
                if let Err(e) = store_api_token_hash(&hash) {
                    tracing::warn!(error = %e, "API token hash not saved");
                }
            }
        }
        *self.api_token.write().expect("api token lock") = api_token;
        self.touch();
        self.log("Wallet unlocked".to_string()).await;
        #[cfg(feature = "client")]
        if let Err(e) = self.load_conversations().await {
            tracing::warn!(error = %e, "Saved conversations not read");
        }
        if let Err(e) = self.ensure_delegation().await {
            tracing::warn!(error = %e, "Delegation not submitted");
        }
    }

    /// Takes the wallet's key and the API token out of memory. The node
    /// keeps serving the network, and the API keeps checking tokens against
    /// the hash.
    pub async fn lock_wallet(&self) {
        // Sealed with the wallet's key, so saved while it is still here.
        #[cfg(feature = "client")]
        if let Err(e) = self.save_conversations().await {
            tracing::warn!(error = %e, "Conversations not saved");
        }
        let was = self.wallet_key.write().await.take().is_some();
        *self.api_token.write().expect("api token lock") = None;
        if was {
            self.log("Wallet locked".to_string()).await;
        }
    }

    pub fn set_auto_lock_minutes(&self, minutes: u64) {
        self.auto_lock_ms
            .store(minutes.saturating_mul(60_000), Ordering::Relaxed);
    }

    /// Locks the wallet once it has been idle longer than the configured
    /// time. Zero means never.
    pub async fn run_auto_lock(self) {
        loop {
            if self
                .sleep_or_shutdown(std::time::Duration::from_secs(15))
                .await
            {
                break;
            }
            let limit = self.auto_lock_ms.load(Ordering::Relaxed);
            let idle = now_ms().saturating_sub(self.last_activity_ms.load(Ordering::Relaxed));
            if limit > 0 && idle >= limit && self.is_unlocked().await {
                tracing::info!(idle_ms = idle, "Locking the wallet after inactivity");
                self.lock_wallet().await;
                continue;
            }
            // Until the link is on chain, keep at it while the wallet is
            // open: the first try may have gone out before the node had
            // peers or had synced.
            if self.is_unlocked().await
                && matches!(
                    self.delegation_status().await,
                    Delegation::Unsigned | Delegation::Elsewhere
                )
            {
                if let Err(e) = self.ensure_delegation().await {
                    tracing::debug!(error = %e, "Delegation retry");
                }
            }
        }
    }

    /// Signs `call` with the wallet's key and submits it, like
    /// `sign_and_submit` does with the node's.
    pub async fn sign_and_submit_as_wallet<C: serde::Serialize>(
        &self,
        pallet_idx: u8,
        call: &C,
    ) -> Result<core_crypto::Extrinsic, String> {
        let _guard = self.nonce_lock.lock().await;
        let ext = {
            let wallet = self.wallet_key.read().await;
            let Some(w) = wallet.as_ref() else {
                return Err(LOCKED.into());
            };
            let nonce = self.next_nonce_for(&w.peer_id).await;
            core_crypto::Extrinsic::sign(&w.signing_key, pallet_idx, call, w.peer_id, nonce)
        };
        self.my_inflight_txs
            .lock()
            .await
            .push((now_ms(), ext.clone()));
        drop(_guard);
        if self.is_block_producer().await {
            self.queue_pending_tx(ext.clone()).await;
        }
        self.broadcast_chain_extrinsic(&ext).await;
        self.touch();
        Ok(ext)
    }

    pub async fn delegation_status(&self) -> Delegation {
        let node = *self.my_peer_id.read().await;
        let Some(wallet) = *self.wallet_address.read().await else {
            return Delegation::SameKey;
        };
        if wallet == node {
            return Delegation::SameKey;
        }
        match self.chain.read().await.identity.wallet_of(&node) {
            Some(w) if w == wallet => return Delegation::Linked,
            Some(_) => return Delegation::Elsewhere,
            None => {}
        }
        if self.delegation_in_flight(&wallet).await {
            Delegation::Pending
        } else {
            Delegation::Unsigned
        }
    }

    async fn release_in_flight(&self, node: &PeerId) -> bool {
        use core_crypto::extrinsic::DecodedCall;
        self.my_inflight_txs.lock().await.iter().any(|(_, e)| {
            e.signer == *node
                && matches!(
                    DecodedCall::decode(e.pallet_idx, &e.args),
                    Ok(DecodedCall::Identity(IdentityCall::Revoke { .. }))
                )
        })
    }

    async fn delegation_in_flight(&self, wallet: &PeerId) -> bool {
        use core_crypto::extrinsic::DecodedCall;
        self.my_inflight_txs.lock().await.iter().any(|(_, e)| {
            e.signer == *wallet
                && matches!(
                    DecodedCall::decode(e.pallet_idx, &e.args),
                    Ok(DecodedCall::Identity(IdentityCall::Delegate { .. }))
                )
        })
    }

    /// Puts the node-to-wallet link on chain if it is not there: signed by
    /// the wallet, countersigned by the node. Returns whether it submitted.
    pub async fn ensure_delegation(&self) -> Result<bool, String> {
        let node = *self.my_peer_id.read().await;
        let wallet = {
            let w = self.wallet_key.read().await;
            match w.as_ref() {
                Some(w) => w.peer_id,
                None => return Err(LOCKED.into()),
            }
        };
        match self.delegation_status().await {
            Delegation::SameKey | Delegation::Linked | Delegation::Pending => return Ok(false),
            // The wallet was replaced (restored from other words): the node
            // leaves the old one with its own key, and joins this one on a
            // later pass, once the release is in a block.
            Delegation::Elsewhere => {
                if !self.release_in_flight(&node).await {
                    self.sign_and_submit(
                        core_crypto::extrinsic::PALLET_IDENTITY,
                        &IdentityCall::Revoke { node },
                    )
                    .await;
                    self.log("Releasing this node from its previous wallet".to_string())
                        .await;
                }
                return Ok(true);
            }
            Delegation::Unsigned => {}
        }
        let call = IdentityCall::Delegate {
            node,
            node_sig: sign_delegation(&self.signing_key, &wallet),
        };
        self.sign_and_submit_as_wallet(core_crypto::extrinsic::PALLET_IDENTITY, &call)
            .await?;
        self.log(format!(
            "Linking this node to wallet {} on chain",
            hex::encode(&wallet[..4])
        ))
        .await;
        Ok(true)
    }
}
