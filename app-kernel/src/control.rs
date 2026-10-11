// SPDX-License-Identifier: BUSL-1.1
//! The running node's control socket: how `nepheral-node unlock`, `lock`,
//! `status` and `wallet send` reach it.
//!
//! A Unix socket in the node's private directory, readable and writable by
//! its owner only. One JSON request per connection, one JSON reply. The
//! password crosses it once, to open the vault inside the node, and is wiped
//! there; it is never stored.

#[cfg(unix)]
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[cfg(unix)]
use app_kernel::nepheral_config;
#[cfg(unix)]
use app_kernel::state::AppState;

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Status,
    /// What monitoring reads: chain progress and connectivity, no secrets.
    Health,
    Unlock {
        password: String,
    },
    Lock,
    Send {
        to: String,
        amount: u64,
    },
}

#[derive(Serialize, Deserialize, Default)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<Health>,
}

/// The node's vital signs, for `nepheral-node health` and the watch script.
/// Holds nothing private, so it can be logged or sent to an alert channel.
#[derive(Serialize, Deserialize)]
pub struct Health {
    pub version: String,
    pub protocol: u16,
    pub height: u64,
    /// Seconds since the newest block was sealed.
    pub head_age_secs: u64,
    pub finalized: u64,
    pub validator: bool,
    pub ai_ready: bool,
    pub connections: usize,
    /// Nodes known through presence, reachable directly or by relay.
    pub known_nodes: usize,
    pub pending_txs: usize,
    pub inflight_txs: usize,
    /// Chain rules in force, and the newest this build implements.
    pub rules_version: u32,
    pub rules_supported: u32,
    /// A scheduled rules upgrade: its version and when it activates.
    pub upgrade_version: Option<u32>,
    pub upgrade_at_ms: Option<u64>,
    /// Set when the node stopped at rules it does not implement.
    pub upgrade_required: Option<u32>,
    /// NAT traversal: nodes reached directly and through
    /// relays, hole punches tried and connected since start, and where the
    /// router forwards the mesh port.
    #[serde(default)]
    pub direct_peers: usize,
    #[serde(default)]
    pub relayed_peers: usize,
    #[serde(default)]
    pub punch_tried: u64,
    #[serde(default)]
    pub punch_ok: u64,
    #[serde(default)]
    pub mapped_addr: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Status {
    pub node: String,
    pub wallet: Option<String>,
    pub unlocked: bool,
    pub delegation: String,
    pub balance: u64,
    pub auto_lock_minutes: u64,
    /// Only while unlocked: what the local API wants as its bearer token.
    pub api_token: Option<String>,
}

#[cfg(unix)]
pub fn socket_path() -> PathBuf {
    nepheral_config::nepheral_dir().join("control.sock")
}

#[cfg(unix)]
impl Reply {
    fn err(e: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(e.into()),
            ..Default::default()
        }
    }
}

/// Serves the socket until the node shuts down.
#[cfg(unix)]
pub async fn serve(state: AppState) {
    use std::os::unix::fs::PermissionsExt;

    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = match tokio::net::UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "Control socket not available");
            return;
        }
    };
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    tracing::info!(path = %path.display(), "Control socket ready");
    let shutdown = state.shutdown_notify.clone();
    let stop = shutdown.notified();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = &mut stop => break,
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    tokio::spawn(handle(state.clone(), stream));
                }
            }
        }
    }
    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
async fn handle(state: AppState, stream: tokio::net::UnixStream) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use zeroize::Zeroize;

    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    let mut reader = BufReader::new(read.take(16 * 1024));
    let reply = match reader.read_line(&mut line).await {
        Ok(_) => match serde_json::from_str::<Request>(&line) {
            Ok(req) => answer(&state, req).await,
            Err(_) => Reply::err("bad request"),
        },
        Err(_) => Reply::err("bad request"),
    };
    line.zeroize();
    let mut out = serde_json::to_string(&reply).unwrap_or_else(|_| "{}".into());
    out.push('\n');
    let _ = write.write_all(out.as_bytes()).await;
}

#[cfg(unix)]
async fn answer(state: &AppState, req: Request) -> Reply {
    match req {
        Request::Status => Reply {
            ok: true,
            status: Some(status_of(state).await),
            ..Default::default()
        },
        Request::Health => Reply {
            ok: true,
            health: Some(health_of(state).await),
            ..Default::default()
        },
        Request::Unlock { password } => {
            let password = zeroize::Zeroizing::new(password);
            let dir = nepheral_config::nepheral_dir();
            let opened = tokio::task::spawn_blocking(move || {
                app_kernel::vault::Vault::unlock(&dir, &password)
            })
            .await;
            let secrets = match opened {
                Ok(Ok((_vault, secrets))) => secrets,
                Ok(Err(e)) => {
                    // Slows guessing; the socket is the owner's alone anyway.
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    return Reply::err(e.to_string());
                }
                Err(_) => return Reply::err("unlock failed"),
            };
            let mnemonic = match core_crypto::NodeMnemonic::from_phrase(secrets.mnemonic.trim()) {
                Ok(m) => m,
                Err(e) => return Reply::err(format!("the vault holds no valid phrase: {e}")),
            };
            state
                .unlock_wallet(
                    app_kernel::wallet::WalletKey::from_mnemonic(&mnemonic),
                    secrets.api_token.clone(),
                )
                .await;
            Reply {
                ok: true,
                status: Some(status_of(state).await),
                ..Default::default()
            }
        }
        Request::Lock => {
            state.lock_wallet().await;
            Reply {
                ok: true,
                status: Some(status_of(state).await),
                ..Default::default()
            }
        }
        Request::Send { to, amount } => {
            let mut receiver = [0u8; 32];
            if hex::decode_to_slice(to.trim(), &mut receiver).is_err() {
                return Reply::err("the recipient is a 64-character hex address");
            }
            match state.transfer_credits(receiver, amount).await {
                Ok(()) => Reply {
                    ok: true,
                    status: Some(status_of(state).await),
                    ..Default::default()
                },
                Err(e) => Reply::err(e),
            }
        }
    }
}

#[cfg(unix)]
async fn status_of(state: &AppState) -> Status {
    let node = *state.my_peer_id.read().await;
    let wallet = *state.wallet_address.read().await;
    let account = state.my_account().await;
    let unlocked = state.is_unlocked().await;
    Status {
        node: hex::encode(node),
        wallet: wallet.map(hex::encode),
        unlocked,
        delegation: format!("{:?}", state.delegation_status().await).to_lowercase(),
        balance: state.chain.read().await.balance_of(&account),
        auto_lock_minutes: state
            .auto_lock_ms
            .load(std::sync::atomic::Ordering::Relaxed)
            / 60_000,
        api_token: if unlocked {
            state.api_token.read().expect("api token lock").clone()
        } else {
            None
        },
    }
}

#[cfg(unix)]
async fn health_of(state: &AppState) -> Health {
    let now = app_kernel::state::now_ms();
    let (height, head_ts, finalized, rules_version, scheduled) = {
        let chain = state.chain.read().await;
        (
            chain.head_height(),
            chain.head_timestamp_ms().unwrap_or(0),
            chain.finalized().0,
            chain.rules_version(),
            chain.upgrade.scheduled.clone(),
        )
    };
    let known_nodes = state
        .presence
        .read()
        .await
        .values()
        .filter(|h| app_kernel::presence::is_fresh(h, now))
        .count();
    Health {
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol: mesh_network::protocol::PROTOCOL_VERSION,
        height,
        head_age_secs: now.saturating_sub(head_ts) / 1000,
        finalized,
        validator: state.is_block_producer().await,
        ai_ready: state.am_i_ai_capable().await,
        connections: state.active_connections.read().await.len(),
        known_nodes,
        pending_txs: state.pending_txs.lock().await.len(),
        inflight_txs: state.my_inflight_txs.lock().await.len(),
        rules_version,
        rules_supported: core_crypto::pallets::upgrade::SUPPORTED_RULES_VERSION,
        upgrade_version: scheduled.as_ref().map(|s| s.version),
        upgrade_at_ms: scheduled.as_ref().map(|s| s.activate_at_ms),
        upgrade_required: state.upgrade_required(),
        direct_peers: state.direct_peer_count().await,
        relayed_peers: state.relayed_peers().await.len(),
        punch_tried: state.punch_tried.load(std::sync::atomic::Ordering::Relaxed),
        punch_ok: state.punch_ok.load(std::sync::atomic::Ordering::Relaxed),
        mapped_addr: state
            .mapped_addr
            .read()
            .expect("mapped addr")
            .map(|a| a.to_string()),
    }
}

/// Sends one request to the running node and returns its reply.
#[cfg(unix)]
pub fn call(req: &Request) -> Result<Reply, String> {
    use std::io::{BufRead, BufReader, Write};

    let path = socket_path();
    let mut stream = std::os::unix::net::UnixStream::connect(&path).map_err(|e| {
        format!(
            "cannot reach the running node at {} ({e}). Is nepheral-node running as this user, \
             with the same HOME?",
            path.display()
        )
    })?;
    let mut line = serde_json::to_string(req).map_err(|e| e.to_string())?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|e| e.to_string())?;
    zeroize::Zeroize::zeroize(&mut line);
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&reply).map_err(|e| format!("bad reply from the node: {e}"))
}

#[cfg(not(unix))]
pub fn call(_req: &Request) -> Result<Reply, String> {
    Err("the control socket is only available on Linux and macOS".into())
}
