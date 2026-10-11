// SPDX-License-Identifier: BUSL-1.1
//! Light sync on the node side: serving the proof that a
//! light node (the phone app) starts from, and starting from one. The proof
//! and its verification are `core_crypto::light`.
//!
//! On the wire: a light node opens a stream and sends `LIGHT_SYNC_REQUEST`
//! alone; the server answers on the same stream with a u32 length and the
//! encoded `LightProof`. The light node verifies it from its own genesis,
//! adopts the state when it is ahead, then catches up with an ordinary block
//! sync from the proof's tip.
//!
//! A node that adopted a proof keeps no history before it, so its chain file
//! holds only the blocks after it, and the state it started from is stored
//! beside it (`chain-base.bin`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_crypto::block::BlockHeader;
use core_crypto::chain_file::{strip_header, with_header};
use core_crypto::light::LightProof;
use core_crypto::ChainState;
use mesh_network::protocol::{message_types, MAX_LIGHT_PROOF_BYTES};

use crate::state::AppState;

/// A proof is rebuilt at most this often. Rebuilding replays the whole
/// chain (about a second for the testnet's first 6,700 blocks); a light node
/// fetches the blocks after the tip with an ordinary block sync anyway.
pub const PROOF_MAX_AGE: Duration = Duration::from_secs(5 * 60);

/// How long a light node waits for a node that serves light sync before it
/// syncs blocks the ordinary way, so it still joins a network whose nodes
/// predate light sync.
pub const LIGHT_PROOF_PATIENCE: Duration = Duration::from_secs(2 * 60);

/// A light node whose chain is older than this when it starts asks for a
/// fresh proof instead of fetching every block since.
pub const LIGHT_RESYNC_AFTER_MS: u64 = 6 * 60 * 60 * 1_000;

/// The last proof built: when, the height of its tip, and its bytes.
#[derive(Default)]
pub struct ProofCache {
    built: Option<(Instant, u64, Arc<Vec<u8>>)>,
}

/// Where the state a light node started from is kept, beside the chain file.
pub fn base_path(chain_file: &Path) -> PathBuf {
    let stem = chain_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("chain");
    chain_file.with_file_name(format!("{stem}-base.bin"))
}

/// Reads a stored base: the header a light node started from and the state
/// it committed to. `None` when there is none, it belongs to another chain
/// generation, or the state no longer matches the header.
pub fn load_base(chain_file: &Path) -> Option<ChainState> {
    let data = std::fs::read(base_path(chain_file)).ok()?;
    let (header, state): (BlockHeader, ChainState) =
        core_crypto::wire::decode(strip_header(&data)?)?;
    (state.state_root() == header.state_root).then(|| state.imported_at(header))
}

fn store_base(chain_file: &Path, chain: &ChainState) -> Result<(), String> {
    let header = chain.base.clone().ok_or("no base to store")?;
    let mut state = chain.state_without_history();
    state.base = None;
    let data = with_header(&core_crypto::wire::encode(&(header, state)));
    crate::chain_node::write_atomically(&base_path(chain_file), &data)
}

impl AppState {
    pub fn is_light(&self) -> bool {
        self.light.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether this node should wait for a light proof instead of syncing
    /// blocks: a light node holding no chain yet, or one far behind, until
    /// its bootstrap is over.
    pub async fn awaits_light_proof(&self) -> bool {
        use std::sync::atomic::Ordering;
        if !self.is_light() || self.light_bootstrap_done.load(Ordering::Relaxed) {
            return false;
        }
        let chain = self.chain.read().await;
        chain.head_height() == 0
            || chain
                .head_timestamp_ms()
                .is_some_and(|t| crate::state::now_ms().saturating_sub(t) > LIGHT_RESYNC_AFTER_MS)
    }

    /// While a light node holds no chain (or a stale one), asks the
    /// connected nodes that serve light sync for a proof, one after another.
    /// After `LIGHT_PROOF_PATIENCE` without one it syncs blocks instead.
    pub async fn run_light_bootstrap(self) {
        self.light_bootstrap_until_done().await;
        self.light_bootstrap_done
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // The ordinary syncs refused while this waited.
        for (_, _, conn) in self.live_connections().await {
            self.request_block_sync(conn).await;
        }
    }

    async fn light_bootstrap_until_done(&self) {
        let deadline = Instant::now() + LIGHT_PROOF_PATIENCE;
        loop {
            if !self.awaits_light_proof().await {
                return;
            }
            let servers: Vec<[u8; 32]> = self
                .peer_capabilities
                .read()
                .await
                .iter()
                .filter(|(_, c)| c.serves_light)
                .map(|(pid, _)| *pid)
                .collect();
            for pid in servers {
                let Some((_, conn)) = self.connection_for_peer(&pid).await else {
                    continue;
                };
                match self.light_sync_from(&conn).await {
                    Ok(height) if height > 0 => return,
                    Ok(_) => {}
                    Err(e) => tracing::warn!(
                        peer_fp = %hex::encode(&pid[..8]),
                        error = %e,
                        "Light sync failed; trying another node"
                    ),
                }
            }
            if Instant::now() >= deadline {
                tracing::info!("No node served a light proof in time; syncing blocks instead");
                return;
            }
            if self.sleep_or_shutdown(Duration::from_secs(5)).await {
                return;
            }
        }
    }

    /// The genesis state this node trusts, which every proof starts from.
    pub fn genesis_state(&self) -> ChainState {
        let authority = *self.genesis_authority.read().expect("genesis lock");
        ChainState::with_genesis_authority(authority)
    }

    /// Whether this node can serve light sync: it holds the whole history
    /// and something of it is final.
    pub async fn serves_light(&self) -> bool {
        let chain = self.chain.read().await;
        chain.base.is_none() && chain.finalized().0 > 0
    }

    /// The encoded proof for the newest finalized state, from the cache
    /// while it is current or young enough.
    pub async fn light_proof_bytes(&self) -> Option<Arc<Vec<u8>>> {
        // Held while building, so concurrent requests wait for one build.
        let mut cache = self.light_proof.lock().await;
        let finalized = self.chain.read().await.finalized().0;
        if let Some((at, height, bytes)) = &cache.built {
            if *height >= finalized || at.elapsed() < PROOF_MAX_AGE {
                return Some(bytes.clone());
            }
        }
        let blocks = {
            let chain = self.chain.read().await;
            if chain.base.is_some() {
                return None;
            }
            chain.blocks.clone()
        };
        let genesis = self.genesis_state();
        let built = tokio::task::spawn_blocking(move || {
            core_crypto::light::build_proof(genesis, &blocks)
                .map(|p| (p.tip.header.height, core_crypto::wire::encode(&p)))
        })
        .await
        .ok()
        .flatten()?;
        if built.1.len() > MAX_LIGHT_PROOF_BYTES {
            tracing::warn!(bytes = built.1.len(), "Light proof too large to serve");
            return None;
        }
        tracing::info!(
            tip = built.0,
            kb = built.1.len() / 1024,
            "Light proof built"
        );
        let bytes = Arc::new(built.1);
        cache.built = Some((Instant::now(), built.0, bytes.clone()));
        Some(bytes)
    }

    pub async fn serve_light_sync(&self, send: &mut quinn::SendStream) -> Result<(), String> {
        let bytes = self
            .light_proof_bytes()
            .await
            .ok_or("no light proof to serve")?;
        send.write_all(&(bytes.len() as u32).to_be_bytes())
            .await
            .map_err(|e| e.to_string())?;
        send.write_all(&bytes).await.map_err(|e| e.to_string())?;
        let _ = send.finish();
        Ok(())
    }

    /// Fetches a proof from `conn` and verifies it from this node's own
    /// genesis. When it is ahead of the local chain, the node continues from
    /// its state and stores it; then an ordinary block sync brings it to the
    /// head. Returns the height the node is at afterwards.
    pub async fn light_sync_from(&self, conn: &quinn::Connection) -> Result<u64, String> {
        let proof = fetch_proof(conn).await?;
        let genesis = self.genesis_state();
        let state = tokio::task::spawn_blocking(move || core_crypto::light::verify(proof, genesis))
            .await
            .map_err(|e| format!("verify task: {e}"))?
            .map_err(|e| format!("light proof rejected: {e}"))?;
        let tip = (state.head_height(), state.head_hash());
        {
            let mut chain = self.chain.write().await;
            if chain.head_height() >= tip.0 {
                return Ok(chain.head_height());
            }
            *chain = state;
        }
        if let Some(path) = self.chain_path.read().await.clone() {
            let chain = self.chain.read().await.clone();
            store_base(&path, &chain)?;
        }
        // The chain file restarts at the base: rewrite it on the next persist.
        *self.chain_persisted.lock().await = (0, [0u8; 32]);
        {
            let mut tracker = self.finality.lock().await;
            if tracker.observed.0 < tip.0 {
                tracker.observed = tip;
            }
        }
        self.chain_notify.notify_waiters();
        tracing::info!(height = tip.0, "Started from a verified light proof");
        self.request_block_sync(conn.clone()).await;
        Ok(tip.0)
    }
}

async fn fetch_proof(conn: &quinn::Connection) -> Result<LightProof, String> {
    let (mut send, mut recv) = tokio::time::timeout(Duration::from_secs(10), conn.open_bi())
        .await
        .map_err(|_| "open timeout".to_string())?
        .map_err(|e| e.to_string())?;
    send.write_all(&[message_types::LIGHT_SYNC_REQUEST])
        .await
        .map_err(|e| e.to_string())?;
    send.finish().map_err(|e| e.to_string())?;
    tokio::time::timeout(Duration::from_secs(120), async {
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf)
            .await
            .map_err(|e| e.to_string())?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_LIGHT_PROOF_BYTES {
            return Err(format!("light proof too large: {len}"));
        }
        let mut buf = vec![0u8; len];
        recv.read_exact(&mut buf).await.map_err(|e| e.to_string())?;
        core_crypto::wire::decode(&buf).ok_or_else(|| "light proof malformed".to_string())
    })
    .await
    .map_err(|_| "light sync timed out".to_string())?
}
