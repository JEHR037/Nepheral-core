// SPDX-License-Identifier: BUSL-1.1
//! Starting the app.
//!
//! With a wallet already on this computer, the node starts at once with its
//! own key (node_store.rs) and the main window opens locked: the node serves
//! the network while the wallet waits for its password (lock.rs). The first
//! time, a small window creates the wallet - or moves in a phrase an older
//! version kept in a plain file, or restores one from its 24 words - and the
//! node starts once that is done, with the wallet already open.

use std::sync::{Mutex, OnceLock};

use app_kernel::client_node::{self, ClientNode, NodeStart};
use app_kernel::nepheral_config;
use app_kernel::vault::{self, KdfCost, Secrets, Vault, VaultStatus};
use app_kernel::wallet::WalletKey;
use serde::Serialize;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::node_store::NodeStore;

/// The running node, once started. Shut down when the app exits.
pub static NODE: OnceLock<&'static ClientNode> = OnceLock::new();

/// A vault the first-run window just made, waiting for `vault_enter`.
#[derive(Default)]
pub struct Gate {
    pending: Mutex<Option<(Vault, Secrets)>>,
}

#[derive(Serialize)]
pub struct StatusView {
    status: &'static str,
}

/// What the window shows after making the vault: the recovery phrase, once,
/// so it can be written down.
#[derive(Serialize)]
pub struct Opened {
    phrase: Option<Vec<String>>,
}

fn words(secrets: &Secrets) -> Vec<String> {
    secrets
        .mnemonic
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// The vault's directory, for a command of the first-run window. Once the
/// node runs, the wallet belongs to the main window, and these refuse.
fn dir() -> Result<std::path::PathBuf, String> {
    if NODE.get().is_some() {
        return Err("already started".into());
    }
    let dir = nepheral_config::nepheral_dir();
    nepheral_config::ensure_private_dir(&dir)?;
    Ok(dir)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.as_millis() as u64)
        .unwrap_or(0)
}

/// The first window: the main one if a wallet exists, the first-run one
/// otherwise.
pub fn open_first_window(app: &AppHandle) -> Result<(), String> {
    let dir = nepheral_config::nepheral_dir();
    if vault::status(&dir) == VaultStatus::Sealed {
        // A store that will not open (its TPM key was cleared) is not
        // replaced: the unlock window opens instead, and the key comes back
        // from the copy in the vault (`vault_enter`).
        match NodeStore::load_or_create(&dir) {
            Ok(store) => return launch(app, store, None),
            Err(e) => {
                tracing::warn!(error = %e, "Node store unreadable; recovering from the vault")
            }
        }
    }
    let page = if crate::is_light() {
        "unlock.html?m=1"
    } else {
        "unlock.html"
    };
    let builder = WebviewWindowBuilder::new(app, FIRST_WINDOW, WebviewUrl::App(page.into()));
    #[cfg(desktop)]
    let builder = builder
        .title("Nepheral")
        .inner_size(500.0, 740.0)
        .resizable(false)
        .center();
    builder.build().map_err(|e| e.to_string())?;
    Ok(())
}

/// The first-run window. A phone has one window for the whole app, which
/// moves on to the main page once the account exists.
#[cfg(desktop)]
const FIRST_WINDOW: &str = "unlock";
#[cfg(mobile)]
const FIRST_WINDOW: &str = "main";

/// Opens the main page: the desktop's, or the phone's in the light profile.
fn open_main_window(app: &AppHandle) -> Result<(), String> {
    let page = if crate::is_light() {
        "mobile.html"
    } else {
        "index.html"
    };
    #[cfg(mobile)]
    if let Some(w) = app.get_webview_window("main") {
        return w
            .eval(&format!("location.replace('{page}')"))
            .map_err(|e| e.to_string());
    }
    let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::App(page.into()));
    #[cfg(desktop)]
    let builder = if crate::is_light() {
        // The phone app on a desktop, for working on its layout.
        builder.title("Nepheral").inner_size(420.0, 860.0)
    } else {
        builder
            .title("Nepheral")
            .inner_size(1180.0, 760.0)
            .min_inner_size(960.0, 600.0)
    };
    builder.build().map_err(|e| e.to_string())?;
    Ok(())
}

/// Starts the node with its own key and opens the main window. `opened` is
/// a wallet the first-run window just made, open from the start.
fn launch(
    app: &AppHandle,
    mut store: NodeStore,
    mut opened: Option<(Vault, Secrets)>,
) -> Result<(), String> {
    if let Some((vault, secrets)) = opened.as_mut() {
        crate::lock::absorb_prompt(&mut store, secrets, vault)?;
        if let Err(e) = crate::lock::escrow_node_key(&store, secrets, vault) {
            tracing::warn!(error = %e, "Node key copy not kept in the vault");
        }
    }
    let cfg = nepheral_config::load_config().unwrap_or_default();
    let wallet_address = cfg
        .wallet_address
        .as_deref()
        .and_then(|h| hex::decode(h.trim()).ok())
        .and_then(|b| <[u8; 32]>::try_from(b).ok());
    let start = NodeStart {
        node_key: store.signing_key()?,
        wallet_address,
        auto_lock_minutes: cfg
            .auto_lock_minutes
            .unwrap_or(app_kernel::wallet::DEFAULT_AUTO_LOCK_MINUTES),
        system_prompt: store.system_prompt.clone(),
        light: crate::is_light(),
    };
    let unlock = match opened.as_ref() {
        Some((_, s)) => Some((
            WalletKey::from_mnemonic(&core_crypto::NodeMnemonic::from_phrase(s.mnemonic.trim())?),
            s.api_token.clone(),
        )),
        None => None,
    };

    // The node builds its own runtime and blocks on it while starting, which
    // tokio refuses to do on a thread already running async tasks.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let started = client_node::start(start).map_err(|e| e.to_string());
        if let (Ok(node), Some((key, token))) = (&started, unlock) {
            node.rt.block_on(node.state.unlock_wallet(key, token));
        }
        let _ = tx.send(started);
    });
    let node = rx
        .recv()
        .map_err(|_| "the node did not start".to_string())??;
    let node: &'static ClientNode = Box::leak(Box::new(node));
    *node.state.hw_seal.write().expect("hw seal") = crate::node_store::hw_seal();
    let _ = NODE.set(node);

    let desktop = crate::commands::Desktop::new(node.state.clone(), node.first_run, store, opened);
    crate::lock::install(app, &desktop);
    app.manage(desktop);
    open_main_window(app)
}

#[tauri::command]
pub fn vault_status() -> StatusView {
    let status = match vault::status(&nepheral_config::nepheral_dir()) {
        VaultStatus::Missing => "missing",
        VaultStatus::Legacy => "legacy",
        VaultStatus::Sealed => "sealed",
    };
    StatusView { status }
}

fn hold(g: &State<'_, Gate>, vault: Vault, secrets: Secrets) -> Result<(), String> {
    crate::lock::remember_wallet(&secrets)?;
    *g.pending.lock().expect("gate lock") = Some((vault, secrets));
    Ok(())
}

/// A new wallet, sealed from the start.
#[tauri::command]
pub async fn vault_create(g: State<'_, Gate>, password: String) -> Result<Opened, String> {
    let dir = dir()?;
    if vault::status(&dir) != VaultStatus::Missing {
        return Err("a wallet already exists on this computer".into());
    }
    let secrets = Secrets {
        mnemonic: core_crypto::NodeMnemonic::generate().phrase().to_string(),
        api_token: Some(app_kernel::wallet::new_api_token()),
        system_prompt: None,
        node_seed: None,
    };
    let vault =
        Vault::create(&dir, &password, &secrets, KdfCost::DEFAULT).map_err(|e| e.to_string())?;
    let phrase = Some(words(&secrets));
    hold(&g, vault, secrets)?;
    Ok(Opened { phrase })
}

/// Moves a phrase an older version kept in a plain file into a new vault.
/// That phrase becomes the wallet; the node gets a key of its own.
#[tauri::command]
pub async fn vault_migrate(g: State<'_, Gate>, password: String) -> Result<Opened, String> {
    let dir = dir()?;
    let (vault, mut secrets) =
        vault::migrate_legacy(&dir, &password, KdfCost::DEFAULT).map_err(|e| e.to_string())?;
    if secrets.api_token.is_none() {
        secrets.api_token = Some(app_kernel::wallet::new_api_token());
        vault.save(&secrets).map_err(|e| e.to_string())?;
    }
    let phrase = Some(words(&secrets));
    hold(&g, vault, secrets)?;
    Ok(Opened { phrase })
}

/// Opens an existing vault from the first-run window. Not normally reached
/// - with a vault, the app starts on the main window - but kept so the
/// window never strands anyone.
#[tauri::command]
pub async fn vault_unlock(g: State<'_, Gate>, password: String) -> Result<Opened, String> {
    let dir = dir()?;
    let (vault, secrets) = Vault::unlock(&dir, &password).map_err(|e| e.to_string())?;
    if let Err(e) = vault::wipe_plain_copies(&dir) {
        tracing::warn!(error = %e, "Plain copies not fully removed");
    }
    hold(&g, vault, secrets)?;
    Ok(Opened { phrase: None })
}

/// A wallet from its 24 words, under a new password. Any vault already here
/// is kept beside it.
#[tauri::command]
pub async fn vault_restore(
    g: State<'_, Gate>,
    phrase: String,
    password: String,
) -> Result<Opened, String> {
    let dir = dir()?;
    let secrets = Secrets {
        mnemonic: phrase.split_whitespace().collect::<Vec<_>>().join(" "),
        api_token: Some(app_kernel::wallet::new_api_token()),
        system_prompt: None,
        node_seed: None,
    };
    let vault = vault::restore(&dir, &password, &secrets, KdfCost::DEFAULT, now_ms())
        .map_err(|e| e.to_string())?;
    hold(&g, vault, secrets)?;
    Ok(Opened { phrase: None })
}

/// The phrase just shown, as a PDF under its own password, saved where the
/// user picks - a USB drive, ideally. Paper stays the recommended backup;
/// the window and the document both say so. `None` when the save dialog is
/// cancelled.
#[tauri::command]
pub async fn vault_export_pdf(
    app: AppHandle,
    g: State<'_, Gate>,
    password: String,
    lang: String,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let pdf = {
        let pending = g.pending.lock().expect("gate lock");
        let Some((_, secrets)) = pending.as_ref() else {
            return Err("unlock first".into());
        };
        let mnemonic = core_crypto::NodeMnemonic::from_phrase(secrets.mnemonic.trim())?;
        let address = core_crypto::NodeIdentity::from_mnemonic(&mnemonic)?.fingerprint_hex();
        let words: Vec<&str> = secrets.mnemonic.split_whitespace().collect();
        app_kernel::recovery_pdf::build(&words, &address, &lang, &password)?
    };
    let name = if lang == "es" {
        "nepheral-frase-de-recuperacion.pdf"
    } else {
        "nepheral-recovery-phrase.pdf"
    };
    let Some(path) = app
        .dialog()
        .file()
        .add_filter("PDF", &["pdf"])
        .set_file_name(name)
        .blocking_save_file()
    else {
        return Ok(None);
    };
    let path = path.into_path().map_err(|e| e.to_string())?;
    std::fs::write(&path, pdf).map_err(|e| format!("Could not save the PDF: {e}"))?;
    Ok(Some(path.display().to_string()))
}

/// Starts the node and swaps the first-run window for the main one, with
/// the wallet just made already open.
#[tauri::command]
pub async fn vault_enter(app: AppHandle, g: State<'_, Gate>) -> Result<(), String> {
    if NODE.get().is_some() {
        return Err("already started".into());
    }
    let Some(pending) = g.pending.lock().expect("gate lock").take() else {
        return Err("create or restore the wallet first".into());
    };
    let dir = nepheral_config::nepheral_dir();
    let store = match NodeStore::load_or_create(&dir) {
        Ok(store) => store,
        Err(e) => {
            let seed = pending.1.node_seed.clone().ok_or(e)?;
            let store = NodeStore::from_seed(&seed, None)?;
            store.save(&dir)?;
            tracing::info!("Node key restored from the vault");
            store
        }
    };
    let handle = app.clone();
    tokio::task::spawn_blocking(move || launch(&handle, store, Some(pending)))
        .await
        .map_err(|_| "the node did not start".to_string())??;
    #[cfg(desktop)]
    if let Some(w) = app.get_webview_window("unlock") {
        let _ = w.close();
    }
    Ok(())
}
