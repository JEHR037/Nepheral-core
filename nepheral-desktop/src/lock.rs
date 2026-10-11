// SPDX-License-Identifier: BUSL-1.1
//! The wallet's lock, from the main window.
//!
//! The node runs from launch with its own key. The wallet - the 24 words,
//! the funds - opens with its password and closes again by hand, or by
//! itself after a stretch of inactivity. Locked, chats and every action on
//! the wallet wait; the local API keeps answering whoever has its token.
//!
//! Every payment, trade and vote is confirmed in a native dialog, raised
//! here, outside the page: a page that rendered something hostile cannot
//! press its button.

use std::sync::{Arc, Mutex};

use app_kernel::nepheral_config;
use app_kernel::vault::{self, KdfCost, Secrets, Vault};
use app_kernel::wallet::{Confirm, Delegation, WalletKey, LOCKED};
use serde::Serialize;
use tauri::AppHandle;

use crate::commands::Desktop;
use crate::node_store::NodeStore;

type D<'a> = tauri::State<'a, Desktop>;

/// The open vault and what it holds, while the wallet is unlocked.
pub type Open = Arc<Mutex<Option<(Vault, Secrets)>>>;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.as_millis() as u64)
        .unwrap_or(0)
}

/// Records, in config.json, what may be known while the wallet is locked:
/// its public address, and the hash the API checks its token against.
pub fn remember_wallet(secrets: &Secrets) -> Result<(), String> {
    let mnemonic = core_crypto::NodeMnemonic::from_phrase(secrets.mnemonic.trim())?;
    let address = hex::encode(WalletKey::address_of(&mnemonic));
    nepheral_config::update(move |c| c.wallet_address = Some(address))?;
    if let Some(token) = secrets.api_token.as_deref() {
        app_kernel::wallet::store_api_token_hash(&app_kernel::wallet::api_token_hash(token))?;
    }
    Ok(())
}

/// A vault from before the node store kept the system prompt with the
/// wallet; the node needs it while locked, so it moves to the node store.
pub fn absorb_prompt(
    store: &mut NodeStore,
    secrets: &mut Secrets,
    vault: &Vault,
) -> Result<(), String> {
    let Some(prompt) = secrets.system_prompt.take() else {
        return Ok(());
    };
    if store.system_prompt.is_none() {
        store.system_prompt = Some(prompt);
        store.save(&nepheral_config::nepheral_dir())?;
    }
    vault.save(secrets).map_err(|e| e.to_string())
}

/// Keeps a copy of the node key in the vault (under the wallet's password),
/// then lets the hardware seal the store: if the TPM is ever cleared, the
/// node gets its identity back the next time the wallet opens.
pub fn escrow_node_key(
    store: &NodeStore,
    secrets: &mut Secrets,
    vault: &Vault,
) -> Result<(), String> {
    if secrets.node_seed.as_deref() != Some(store.seed_hex()) {
        secrets.node_seed = Some(store.seed_hex().to_string());
        vault.save(secrets).map_err(|e| e.to_string())?;
    }
    let first = !crate::node_store::hw_seal_allowed();
    crate::node_store::allow_hardware_seal();
    if first {
        // Sealed again, now with the TPM where there is one.
        store.save(&nepheral_config::nepheral_dir())?;
    }
    Ok(())
}

/// Installs the confirmation dialog and the watcher that drops the open
/// vault from memory when the wallet locks itself.
pub fn install(app: &AppHandle, desktop: &Desktop) {
    let lang = desktop.ui_lang.clone();
    let handle = app.clone();
    desktop
        .state
        .set_confirm_hook(Some(Arc::new(move |what: Confirm| {
            let handle = handle.clone();
            let es = lang.lock().expect("lang lock").as_str() == "es";
            Box::pin(async move {
                tokio::task::spawn_blocking(move || ask(&handle, &what, es))
                    .await
                    .unwrap_or(false)
            })
        })));

    let state = desktop.state.clone();
    let open = desktop.open.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if !state.is_unlocked().await {
                open.lock().expect("open lock").take();
            }
        }
    });
}

fn ask(app: &AppHandle, what: &Confirm, es: bool) -> bool {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    let (title, body) = describe(what, es);
    let (yes, no) = if es {
        ("Confirmar", "Cancelar")
    } else {
        ("Confirm", "Cancel")
    };
    let dialog = app
        .dialog()
        .message(body)
        .title(title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(yes.into(), no.into()));
    // A phone has one window and no parent to give a dialog.
    #[cfg(desktop)]
    let dialog = {
        use tauri::Manager;
        match app.get_webview_window("main") {
            Some(w) => dialog.parent(&w),
            None => dialog,
        }
    };
    dialog.blocking_show()
}

fn after(balance: u64, cost: u64) -> u64 {
    balance.saturating_sub(cost)
}

/// The dialog's title and text: what will happen, in full.
pub fn describe(what: &Confirm, es: bool) -> (String, String) {
    let tr = |es_text: String, en_text: String| if es { es_text } else { en_text };
    match what {
        Confirm::Transfer {
            to,
            amount,
            balance,
        } => {
            // The phone app speaks of credits, not of the token's name.
            let unit = match (crate::is_light(), es) {
                (true, true) => "créditos",
                (true, false) => "credits",
                (false, _) => "N-AI",
            };
            (
                tr("Confirmar envío".into(), "Confirm transfer".into()),
                tr(
                    format!(
                        "Enviar {amount} {unit}\n\nA: {to}\n\nSaldo: {balance} → {} {unit}\n\nUna transferencia no se puede deshacer.",
                        after(*balance, *amount)
                    ),
                    format!(
                        "Send {amount} {unit}\n\nTo: {to}\n\nBalance: {balance} → {} {unit}\n\nA transfer cannot be undone.",
                        after(*balance, *amount)
                    ),
                ),
            )
        }
        Confirm::OpenSession {
            channel,
            cost,
            balance,
        } => {
            let ch = channel.clone().unwrap_or_else(|| tr("prioritaria".into(), "priority".into()));
            (
                tr("Confirmar sesión".into(), "Confirm session".into()),
                tr(
                    format!(
                        "Abrir una sesión {ch} por {cost} N-AI.\n\nSaldo: {balance} → {} N-AI",
                        after(*balance, *cost)
                    ),
                    format!(
                        "Open a {ch} session for {cost} N-AI.\n\nBalance: {balance} → {} N-AI",
                        after(*balance, *cost)
                    ),
                ),
            )
        }
        Confirm::PostAsk {
            credits,
            ask_sats,
            ltc_address,
        } => (
            tr("Confirmar venta".into(), "Confirm listing".into()),
            tr(
                format!(
                    "Publicar la venta de {credits} N-AI por {ask_sats} sats.\n\nCobras en: {ltc_address}\n\nLos créditos quedan en depósito hasta que alguien compre o retires el anuncio."
                ),
                format!(
                    "List {credits} N-AI for {ask_sats} sats.\n\nPaid to: {ltc_address}\n\nThe credits stay in escrow until someone buys or you withdraw the listing."
                ),
            ),
        ),
        Confirm::PostBid { credits, bid_sats } => (
            tr("Confirmar oferta de compra".into(), "Confirm bid".into()),
            tr(
                format!("Publicar una oferta para comprar {credits} N-AI por {bid_sats} sats."),
                format!("Post a bid to buy {credits} N-AI for {bid_sats} sats."),
            ),
        ),
        Confirm::ClaimFill {
            ask_id,
            payment_ref,
        } => (
            tr("Confirmar compra".into(), "Confirm purchase".into()),
            tr(
                format!(
                    "Reclamar el anuncio #{ask_id} con el pago Litecoin:\n\n{payment_ref}\n\nLos créditos llegan a tu cartera cuando el pago se confirme."
                ),
                format!(
                    "Claim listing #{ask_id} with the Litecoin payment:\n\n{payment_ref}\n\nThe credits reach your wallet once the payment confirms."
                ),
            ),
        ),
        Confirm::Cancel { id, is_ask } => (
            tr("Confirmar retirada".into(), "Confirm withdrawal".into()),
            match (es, is_ask) {
                (true, true) => format!("Retirar el anuncio de venta #{id}. Los créditos vuelven a tu cartera."),
                (true, false) => format!("Retirar la oferta de compra #{id}."),
                (false, true) => format!("Withdraw listing #{id}. The credits return to your wallet."),
                (false, false) => format!("Withdraw bid #{id}."),
            },
        ),
        Confirm::Propose { action } => (
            tr("Confirmar propuesta".into(), "Confirm proposal".into()),
            tr(
                format!("Proponer a la red:\n\n{action}"),
                format!("Propose to the network:\n\n{action}"),
            ),
        ),
        Confirm::Vote { id, aye } => (
            tr("Confirmar voto".into(), "Confirm vote".into()),
            match (es, aye) {
                (true, true) => format!("Votar A FAVOR de la propuesta #{id}."),
                (true, false) => format!("Votar EN CONTRA de la propuesta #{id}."),
                (false, true) => format!("Vote FOR proposal #{id}."),
                (false, false) => format!("Vote AGAINST proposal #{id}."),
            },
        ),
        Confirm::Enact { id } => (
            tr("Confirmar ejecución".into(), "Confirm enactment".into()),
            tr(
                format!("Ejecutar la propuesta aprobada #{id}."),
                format!("Enact approved proposal #{id}."),
            ),
        ),
    }
}

#[derive(Serialize)]
pub struct LockView {
    unlocked: bool,
    wallet: Option<String>,
    node: String,
    delegation: Delegation,
    auto_lock_minutes: u64,
}

async fn view(d: &D<'_>) -> LockView {
    let s = &d.state;
    LockView {
        unlocked: s.is_unlocked().await,
        wallet: s.wallet_address.read().await.map(hex::encode),
        node: hex::encode(*s.my_peer_id.read().await),
        delegation: s.delegation_status().await,
        auto_lock_minutes: s.auto_lock_ms.load(std::sync::atomic::Ordering::Relaxed) / 60_000,
    }
}

#[tauri::command]
pub async fn lock_state(d: D<'_>) -> Result<LockView, String> {
    Ok(view(&d).await)
}

/// Opens the wallet with its password.
#[tauri::command]
pub async fn wallet_unlock(d: D<'_>, password: String) -> Result<LockView, String> {
    let dir = nepheral_config::nepheral_dir();
    let password = zeroize::Zeroizing::new(password);
    let (vault, secrets) = tokio::task::spawn_blocking(move || Vault::unlock(&dir, &password))
        .await
        .map_err(|_| "unlock failed".to_string())?
        .map_err(|e| e.to_string())?;
    open_with(&d, vault, secrets).await?;
    Ok(view(&d).await)
}

/// Puts an opened vault to work: the wallet's key into the node, the vault
/// and its secrets into `open`, and anything left behind by older versions
/// tidied away.
async fn open_with(d: &D<'_>, vault: Vault, mut secrets: Secrets) -> Result<(), String> {
    let (prompt, seal_changed) = {
        let mut store = d.node_store.lock().expect("node store lock");
        absorb_prompt(&mut store, &mut secrets, &vault)?;
        if let Err(e) = escrow_node_key(&store, &mut secrets, &vault) {
            tracing::warn!(error = %e, "Node key copy not kept in the vault");
        }
        let seal = crate::node_store::hw_seal();
        let mut current = d.state.hw_seal.write().expect("hw seal");
        let changed = *current != seal;
        *current = seal;
        (store.system_prompt.clone(), changed)
    };
    if seal_changed {
        // The store was just sealed with the TPM: peers see it now, not at
        // the next greeting.
        d.state.announce_capabilities().await;
    }
    *d.state.local_system_prompt.write().await = prompt;
    if let Err(e) = vault::wipe_plain_copies(&nepheral_config::nepheral_dir()) {
        tracing::warn!(error = %e, "Plain copies not fully removed");
    }
    let mnemonic = core_crypto::NodeMnemonic::from_phrase(secrets.mnemonic.trim())?;
    remember_wallet(&secrets)?;
    d.state
        .unlock_wallet(
            WalletKey::from_mnemonic(&mnemonic),
            secrets.api_token.clone(),
        )
        .await;
    *d.open.lock().expect("open lock") = Some((vault, secrets));
    Ok(())
}

#[tauri::command]
pub async fn wallet_lock(d: D<'_>) -> Result<LockView, String> {
    d.state.lock_wallet().await;
    d.open.lock().expect("open lock").take();
    Ok(view(&d).await)
}

/// The way back from a forgotten password: the 24 words and a new password
/// replace the vault, the old one is kept beside it, and the wallet opens.
/// Words of a different wallet move this node over to it.
#[tauri::command]
pub async fn wallet_restore(
    d: D<'_>,
    phrase: String,
    password: String,
) -> Result<LockView, String> {
    let phrase = zeroize::Zeroizing::new(phrase.split_whitespace().collect::<Vec<_>>().join(" "));
    core_crypto::NodeMnemonic::from_phrase(&phrase)
        .map_err(|e| format!("Invalid mnemonic: {e}"))?;
    let api_token = d
        .open
        .lock()
        .expect("open lock")
        .as_ref()
        .and_then(|(_, s)| s.api_token.clone())
        .unwrap_or_else(app_kernel::wallet::new_api_token);
    let secrets = Secrets {
        mnemonic: phrase.to_string(),
        api_token: Some(api_token),
        system_prompt: None,
        node_seed: None,
    };
    let dir = nepheral_config::nepheral_dir();
    let password = zeroize::Zeroizing::new(password);
    let to_seal = secrets.clone();
    let vault = tokio::task::spawn_blocking(move || {
        vault::restore(&dir, &password, &to_seal, KdfCost::DEFAULT, now_ms())
    })
    .await
    .map_err(|_| "restore failed".to_string())?
    .map_err(|e| e.to_string())?;
    d.state.lock_wallet().await;
    open_with(&d, vault, secrets).await?;
    Ok(view(&d).await)
}

/// Minutes of inactivity before the wallet locks itself; 0 is never.
#[tauri::command]
pub async fn set_auto_lock(d: D<'_>, minutes: u64) -> Result<LockView, String> {
    let minutes = minutes.min(24 * 60);
    nepheral_config::update(move |c| c.auto_lock_minutes = Some(minutes))?;
    d.state.set_auto_lock_minutes(minutes);
    d.state.touch();
    Ok(view(&d).await)
}

/// The window's language, for the dialogs raised outside it.
#[tauri::command]
pub async fn set_ui_lang(d: D<'_>, lang: String) -> Result<(), String> {
    *d.ui_lang.lock().expect("lang lock") = if lang == "es" {
        "es".into()
    } else {
        "en".into()
    };
    Ok(())
}

/// Changes the password that opens the wallet.
#[tauri::command]
pub async fn vault_change_password(d: D<'_>, old: String, new: String) -> Result<(), String> {
    d.state.require_unlocked().await?;
    let mut open = d.open.lock().expect("open lock");
    let Some((vault, secrets)) = open.as_mut() else {
        return Err(LOCKED.into());
    };
    vault
        .change_password(&old, &new, secrets)
        .map_err(|e| e.to_string())
}

/// Reads the open vault's secrets, or says the wallet is locked.
pub fn with_secrets<T>(d: &Desktop, f: impl FnOnce(&Secrets) -> T) -> Result<T, String> {
    let open = d.open.lock().expect("open lock");
    match open.as_ref() {
        Some((_, secrets)) => Ok(f(secrets)),
        None => Err(LOCKED.into()),
    }
}
