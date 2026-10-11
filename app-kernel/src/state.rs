// SPDX-License-Identifier: BUSL-1.1

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ai_engine::{InferenceEngine, ModelConfig};
use core_crypto::identity::PeerId;
use ed25519_dalek::SigningKey;
use mesh_network::protocol::{
    caps_bits, frame, generate_node_id, message_types, NodeId, PeerCapabilities, PeerEntry,
};
use mesh_network::tls::TlsIdentity;
use quinn::Connection;
use tokio::sync::{Mutex, Notify, RwLock};

pub use crate::agent::{
    AgentChatEntry, AgentSession, AgentSessionKind, PendingQuery, StoredAgentShare,
};
pub use crate::rooms::{RoomChatEntry, RoomMember, RoomState};

pub const ACTIVITY_LOG_CAP: usize = 500;
pub const DISCOVERED_PEERS_CAP: usize = 1024;
pub const ADVERTISED_ADDRS_CAP: usize = 8;
pub const OBSERVED_ADDR_QUORUM: usize = 2;
pub const SEND_OPEN_TIMEOUT: Duration = Duration::from_secs(5);
pub const SEND_WRITE_TIMEOUT: Duration = Duration::from_secs(20);
pub const HELLO_MAX_SKEW_MS: u64 = 10 * 60 * 1_000;

/// Why a node cannot reach the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetProblem {
    /// Dialing; nothing has failed in a telling way yet.
    Trying,
    /// The servers do not answer from here: the network may block the UDP
    /// the mesh uses.
    Blocked,
    /// No route out at all.
    NoInternet,
    /// This device's clock is too far from the network's.
    Clock,
    /// The network speaks a protocol this build does not.
    Update,
}

impl NetProblem {
    /// From a failed dial's error text.
    pub fn of_dial_error(e: &str) -> Self {
        let e = e.to_ascii_lowercase();
        if e.contains("timed out") || e.contains("timeout") {
            NetProblem::Blocked
        } else if e.contains("unreachable")
            || e.contains("no route")
            || e.contains("refused")
            || e.contains("os error 101")
            || e.contains("os error 10051")
            || e.contains("os error 10065")
        {
            NetProblem::NoInternet
        } else {
            NetProblem::Trying
        }
    }
}

#[derive(Debug, Clone)]
pub struct PeerStatus {
    pub addr: SocketAddr,
    pub peer_id: Option<PeerId>,
    pub connected: bool,
    pub last_seen: Instant,
}

/// How many times `line` is already shown as `last`: `last` itself, or
/// `last` with a `(×n)` count.
fn repeats_of(last: &str, line: &str) -> Option<u32> {
    if last == line {
        return Some(1);
    }
    last.strip_prefix(line)?
        .strip_prefix(" (×")?
        .strip_suffix(')')?
        .parse()
        .ok()
}

#[cfg(test)]
mod log_tests {
    use super::repeats_of;

    #[test]
    fn a_repeated_line_is_counted_not_duplicated() {
        assert_eq!(repeats_of("engine failed", "engine failed"), Some(1));
        assert_eq!(repeats_of("engine failed (×4)", "engine failed"), Some(4));
        assert_eq!(repeats_of("engine ready", "engine failed"), None);
        assert_eq!(repeats_of("engine failed later", "engine failed"), None);
    }
}

#[derive(Debug, Clone, Default)]
pub struct NodeStats {
    pub shares_sent: u64,
    pub lotteries_won: u64,
    pub lotteries_participated: u64,
    pub inference_ms_total: u64,
    pub queue_depth: u32,
    pub uptime_seconds: u64,
    pub performance_score: f64,
}

impl NodeStats {
    pub const WIRE_LEN: usize = 52;

    pub fn compute_score(&mut self) {
        let win_rate = if self.lotteries_participated > 0 {
            self.lotteries_won as f64 / self.lotteries_participated as f64
        } else {
            0.0
        };
        let uptime_factor = (self.uptime_seconds.max(1) as f64).ln();
        self.performance_score = win_rate * uptime_factor;
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::WIRE_LEN);
        buf.extend_from_slice(&self.shares_sent.to_be_bytes());
        buf.extend_from_slice(&self.lotteries_won.to_be_bytes());
        buf.extend_from_slice(&self.lotteries_participated.to_be_bytes());
        buf.extend_from_slice(&self.inference_ms_total.to_be_bytes());
        buf.extend_from_slice(&self.queue_depth.to_be_bytes());
        buf.extend_from_slice(&self.uptime_seconds.to_be_bytes());
        buf.extend_from_slice(&self.performance_score.to_be_bytes());
        buf
    }

    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::WIRE_LEN {
            return None;
        }
        let score = f64::from_be_bytes(buf[44..52].try_into().ok()?);
        Some(Self {
            shares_sent: u64::from_be_bytes(buf[0..8].try_into().ok()?),
            lotteries_won: u64::from_be_bytes(buf[8..16].try_into().ok()?),
            lotteries_participated: u64::from_be_bytes(buf[16..24].try_into().ok()?),
            inference_ms_total: u64::from_be_bytes(buf[24..32].try_into().ok()?),
            queue_depth: u32::from_be_bytes(buf[32..36].try_into().ok()?),
            uptime_seconds: u64::from_be_bytes(buf[36..44].try_into().ok()?),
            performance_score: if score.is_finite() { score } else { 0.0 },
        })
    }
}

pub struct BoundedSet<T: std::hash::Hash + Eq + Clone> {
    order: VecDeque<T>,
    members: HashSet<T>,
    cap: usize,
}

impl<T: std::hash::Hash + Eq + Clone> BoundedSet<T> {
    pub fn new(cap: usize) -> Self {
        Self {
            order: VecDeque::with_capacity(cap.min(1024)),
            members: HashSet::with_capacity(cap.min(1024)),
            cap: cap.max(1),
        }
    }

    pub fn insert(&mut self, value: T) -> bool {
        if self.members.contains(&value) {
            return false;
        }
        if self.order.len() >= self.cap {
            if let Some(old) = self.order.pop_front() {
                self.members.remove(&old);
            }
        }
        self.members.insert(value.clone());
        self.order.push_back(value);
        true
    }

    pub fn contains(&self, value: &T) -> bool {
        self.members.contains(value)
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

pub type StreamHandler = Arc<
    dyn Fn(
            quinn::SendStream,
            quinn::RecvStream,
            AppState,
            SocketAddr,
            PeerId,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

pub type MeshEndpoints = Arc<RwLock<Option<(quinn::Endpoint, Option<quinn::Endpoint>)>>>;

pub type ModelFileHash = Arc<RwLock<Option<(u64, [u8; 32])>>>;

/// What a model file says about itself in its GGUF header, kept for the
/// path and size it was read from.
#[derive(Debug, Clone)]
pub struct ModelMeta {
    pub path: String,
    pub size: u64,
    pub name: Option<String>,
    pub params_b: Option<u16>,
}

pub type ModelOffer = ([u8; 32], u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenSessionError {
    InvalidChannel,
    InsufficientBalance,
    NotIncluded,
    /// The wallet that pays is locked.
    Locked,
    /// Its owner did not confirm the payment.
    Declined,
}

/// Which ranks have declined a given query, keyed by (asker, message id).
type DeclinesByQuery = HashMap<(PeerId, u64), std::collections::BTreeSet<PeerId>>;
/// The one mesh query a node has promised to answer, and when it promised.
/// A claim this node made and has not finished serving: the query, when
/// it was claimed, and whether a worker has started on it.
pub type SlotReservation = Vec<((PeerId, u64), std::time::Instant, bool)>;
/// Share indexes released to this node per claimed query, and when it claimed it.
pub type CustodyReleases = HashMap<(PeerId, u64), (Instant, std::collections::BTreeSet<u8>)>;
/// Claims that arrived before their push, per query, and when the first came.
pub type EarlyClaims = HashMap<(PeerId, u64), (Instant, Vec<mesh_network::protocol::AgentClaim>)>;

#[derive(Clone)]
pub struct AppState {
    pub local_addr: SocketAddr,
    pub node_id: NodeId,
    pub network: core_crypto::Network,
    pub data_dir: std::path::PathBuf,
    pub tls_identity: TlsIdentity,

    pub advertised_addrs: Arc<RwLock<Vec<std::net::IpAddr>>>,
    pub observed_addr_votes: Arc<Mutex<HashMap<std::net::IpAddr, HashSet<PeerId>>>>,
    pub peer_dialable: Arc<RwLock<HashMap<PeerId, Vec<SocketAddr>>>>,
    pub peers: Arc<RwLock<HashMap<SocketAddr, PeerStatus>>>,
    pub active_connections: Arc<RwLock<HashMap<SocketAddr, Connection>>>,
    pub peer_id_by_addr: Arc<RwLock<HashMap<SocketAddr, PeerId>>>,
    pub bootstrap_introducers: Arc<RwLock<Vec<String>>>,
    pub discovered_peers: Arc<Mutex<HashMap<SocketAddr, u32>>>,
    pub failed_dial_peers: Arc<Mutex<HashMap<SocketAddr, Instant>>>,
    pub bootstrap_peers: Arc<RwLock<Vec<SocketAddr>>>,
    pub mesh_endpoints: MeshEndpoints,
    pub can_relay: Arc<AtomicBool>,
    /// Where the router forwards the mesh port, when UPnP, NAT-PMP or PCP
    /// got a mapping (`portmap.rs`).
    pub mapped_addr: Arc<std::sync::RwLock<Option<SocketAddr>>>,
    /// Outside sockets that connected to this node unasked: two make it
    /// publicly reachable, and so a relay.
    pub inbound_strangers: Arc<std::sync::Mutex<HashSet<SocketAddr>>>,
    /// Hole punches since start: tried, and those that connected.
    pub punch_tried: Arc<std::sync::atomic::AtomicU64>,
    pub punch_ok: Arc<std::sync::atomic::AtomicU64>,
    pub relay_routes: Arc<RwLock<HashMap<(PeerId, u64), crate::relay::RelayRoute>>>,
    /// The newest signed HELLO of every node heard of, directly or through
    /// others (`presence.rs`).
    pub presence: Arc<RwLock<HashMap<PeerId, mesh_network::protocol::Hello>>>,
    /// Connections this node dialed, by `stable_id`. Two nodes that dial each
    /// other at once - which a hole punch does on purpose - end up with two
    /// connections, and both must keep the same one (`register_connection`).
    pub outbound_conns: Arc<std::sync::Mutex<HashSet<usize>>>,
    /// Hole punches tried per peer: when, and how many have failed.
    pub punch_attempts: Arc<Mutex<HashMap<PeerId, (Instant, u32)>>>,
    pub punches_in_flight: Arc<Mutex<HashSet<PeerId>>>,
    /// Addresses a public node said a punch would come from.
    pub expected_punches: Arc<Mutex<HashMap<SocketAddr, Instant>>>,
    /// Introductions made as a public node, per pair, to rate-limit them.
    pub punch_intros_sent: Arc<Mutex<HashMap<(PeerId, PeerId), Instant>>>,
    /// Which share indexes reached this node for each query it claimed,
    /// including those that arrive after it had enough: every custodian
    /// that released is credited, not only the first threshold of them.
    pub custody_releases: Arc<RwLock<CustodyReleases>>,
    /// The newest protocol version a peer has announced. Above this build's
    /// own, the user is told to update rather than left with an empty mesh.
    pub newest_peer_protocol: Arc<std::sync::atomic::AtomicU16>,
    /// The chain rules version this build reached and does not implement,
    /// 0 while there is none. Set, the node has stopped following the chain.
    pub upgrade_required: Arc<std::sync::atomic::AtomicU32>,

    pub is_running: Arc<Mutex<bool>>,
    pub peer_notify: Arc<Notify>,
    pub shutdown_notify: Arc<Notify>,
    pub discovery_notify: Arc<Notify>,
    pub start_time: Instant,
    pub activity_log: Arc<Mutex<VecDeque<String>>>,

    pub ai_engine: Arc<RwLock<Option<Arc<InferenceEngine>>>>,
    pub ai_model_config: Arc<RwLock<ModelConfig>>,
    pub ai_engine_status_cache: Arc<std::sync::Mutex<String>>,
    pub ai_engine_init_lock: Arc<Mutex<()>>,
    pub inference_busy: Arc<Mutex<bool>>,
    /// Per-GPU mode: a copy of the model on each further GPU,
    /// each a worker of its own, with whether it is busy.
    pub extra_engines: Arc<RwLock<Vec<Arc<InferenceEngine>>>>,
    pub extra_busy: Arc<Mutex<Vec<bool>>>,
    /// The owner's own chats being answered right now.
    pub local_inferences: Arc<std::sync::atomic::AtomicUsize>,
    /// Ranks that have declined a query, keyed by (asker, message id). A
    /// candidate waiting out its rank delay consults this so it can start the
    /// moment everyone ahead of it has stood down, instead of sleeping the
    /// full 8 or 16 seconds for an answer that is not coming.
    pub observed_declines: Arc<RwLock<DeclinesByQuery>>,
    /// When each peer last declined a query, as seen by this node. Feeds the
    /// lottery: a node that just turned work away is probably still busy.
    pub recent_declines: Arc<RwLock<HashMap<PeerId, Instant>>>,
    /// Winners that stalled a question of this node's, kept out of its draws
    /// for a while (`agent::SLOW_MEMORY`).
    pub slow_responders: Arc<RwLock<HashMap<PeerId, Instant>>>,
    /// Set when this node claims a mesh query, cleared once it has replied.
    /// A claim is a promise to the ranks behind it, which stand down on
    /// seeing it, so it has to hold the slot from that moment - see
    /// `AppState::reserve_slot`.
    pub claim_reservation: Arc<Mutex<SlotReservation>>,

    pub my_peer_id: Arc<RwLock<PeerId>>,
    pub signing_key: Arc<SigningKey>,
    pub peer_capabilities: Arc<RwLock<HashMap<PeerId, PeerCapabilities>>>,
    pub my_stats: Arc<RwLock<NodeStats>>,
    pub peer_stats: Arc<RwLock<HashMap<PeerId, NodeStats>>>,
    /// The local system prompt, from the desktop's vault. Kept here rather
    /// than in config.json so it is never written to disk in the clear.
    pub local_system_prompt: Arc<RwLock<Option<String>>>,
    /// The wallet's key, only while it is unlocked (`wallet.rs`).
    pub wallet_key: Arc<RwLock<Option<crate::wallet::WalletKey>>>,
    /// The wallet this node works for, known while locked too.
    pub wallet_address: Arc<RwLock<Option<PeerId>>>,
    /// The local API's token, to show while the wallet is unlocked.
    pub api_token: Arc<std::sync::RwLock<Option<String>>>,
    /// What the API checks requests against, locked or not: a hash, so the
    /// token is not needed - nor kept - to verify it.
    pub api_token_hash: Arc<std::sync::RwLock<Option<[u8; 32]>>>,
    pub last_activity_ms: Arc<std::sync::atomic::AtomicU64>,
    /// Idle time after which the wallet locks itself; 0 is never.
    pub auto_lock_ms: Arc<std::sync::atomic::AtomicU64>,
    pub confirm_hook: Arc<std::sync::RwLock<Option<crate::wallet::ConfirmHook>>>,

    pub rooms: Arc<RwLock<HashMap<String, RoomState>>>,
    pub room_seen: Arc<Mutex<BoundedSet<[u8; 16]>>>,
    pub room_notify: Arc<Notify>,
    pub room_redactors: Arc<Mutex<HashMap<String, ai_engine::redaction::Redactor>>>,

    pub model_download_progress: Arc<Mutex<Option<(u64, u64)>>>,
    pub model_download_error: Arc<Mutex<Option<String>>>,
    pub model_file_hash: ModelFileHash,
    pub model_meta: Arc<RwLock<Option<ModelMeta>>>,
    pub peers_with_model: Arc<RwLock<HashMap<PeerId, ModelOffer>>>,
    pub pending_model_requests: Arc<Mutex<HashMap<PeerId, (ModelOffer, String)>>>,
    pub active_model_uploads: Arc<AtomicUsize>,
    pub model_transfer_notify: Arc<Notify>,

    pub chain: Arc<RwLock<core_crypto::ChainState>>,
    pub chain_notify: Arc<Notify>,
    pub chain_path: Arc<RwLock<Option<std::path::PathBuf>>>,
    pub chain_persisted: Arc<Mutex<(u64, [u8; 32])>>,
    /// The light-sync proof this node serves, rebuilt when stale.
    pub light_proof: Arc<Mutex<crate::light_sync::ProofCache>>,
    /// A light node (the phone app): it holds fragments and asks, but runs
    /// no model, produces nothing, and starts from a light proof instead of
    /// replaying the whole chain.
    pub light: Arc<AtomicBool>,
    /// The light node has a chain to follow, or gave up waiting for a proof
    /// and syncs blocks the ordinary way.
    pub light_bootstrap_done: Arc<AtomicBool>,
    /// The only validator at genesis: the team key in every real network.
    /// Light sync verifies from the genesis it implies; tests set their own.
    pub genesis_authority: Arc<std::sync::RwLock<PeerId>>,
    pub authority_key: Option<Arc<SigningKey>>,
    pub pending_txs: Arc<Mutex<Vec<core_crypto::Extrinsic>>>,
    pub my_inflight_txs: Arc<Mutex<Vec<(u64, core_crypto::Extrinsic)>>>,
    /// Extrinsics already gossiped, by content. Keyed by signer and nonce it
    /// swallowed a node's second use of a nonce whose first transaction had
    /// been dropped, so the replacement was never passed on.
    pub seen_chain_txs: Arc<Mutex<BoundedSet<[u8; 32]>>>,
    pub nonce_lock: Arc<Mutex<()>>,
    pub finality: Arc<Mutex<crate::finality::FinalityTracker>>,

    pub custody: Arc<RwLock<HashMap<(PeerId, u64), StoredAgentShare>>>,
    pub observed_agent_claims: Arc<RwLock<HashMap<(PeerId, u64), u8>>>,
    /// Claims that arrived before the push they refer to, by query.
    pub early_claims: Arc<RwLock<EarlyClaims>>,
    pub agent_claim_collectors: Arc<RwLock<HashMap<(PeerId, u64), crate::agent::ClaimCollector>>>,
    pub served_sessions: Arc<Mutex<HashMap<[u8; 32], u8>>>,
    pub agent_sessions: Arc<RwLock<HashMap<u64, AgentSession>>>,
    pub session_redactors: Arc<Mutex<HashMap<u64, ai_engine::redaction::Redactor>>>,
    pub active_agent_session: Arc<RwLock<Option<u64>>>,
    pub next_agent_session_id: Arc<Mutex<u64>>,
    pub pending_queries: Arc<RwLock<HashMap<u64, PendingQuery>>>,
    pub agent_session_notify: Arc<Notify>,
    pub composer_min_maturity: Arc<RwLock<Option<u64>>>,
    /// Bumped on every change to the conversations; the saver writes when it
    /// moves past what was saved (`conversations.rs`).
    pub conversations_gen: Arc<std::sync::atomic::AtomicU64>,
    pub conversations_saved_gen: Arc<std::sync::atomic::AtomicU64>,
    pub conversations_loaded: Arc<std::sync::atomic::AtomicBool>,
    /// Why the node is not on the network, while it is not: a code the UI
    /// words in its language (`NetProblem`).
    pub net_problem: Arc<std::sync::Mutex<Option<NetProblem>>>,
    /// What seals this node's own key, set by the app that holds it.
    pub hw_seal: Arc<std::sync::RwLock<Option<mesh_network::protocol::HwSeal>>>,
    /// The memory model that chooses each question's context, once loaded.
    #[cfg(feature = "client")]
    pub recall: Arc<RwLock<Option<Arc<nepheral_recall::memory::Recall>>>>,
    #[cfg(feature = "client")]
    pub recall_status: Arc<std::sync::Mutex<crate::conversations::RecallStatus>>,
    /// Questions waiting for the person to confirm their context.
    #[cfg(feature = "client")]
    pub pending_sends: Arc<Mutex<HashMap<u64, crate::conversations::PendingSend>>>,

    #[cfg(feature = "local-api-server")]
    pub api_routing_policy: Arc<RwLock<nepheral_api::creds::CredsMode>>,

    stream_handler: Arc<std::sync::OnceLock<StreamHandler>>,
}

fn load_authority_key_from_env() -> Option<Arc<SigningKey>> {
    let hex_str = std::env::var("NEPHERAL_AUTHORITY_KEY").ok()?;
    let bytes = hex::decode(hex_str.trim()).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    let key = SigningKey::from_bytes(&arr);
    tracing::info!(
        pubkey = %hex::encode(key.verifying_key().to_bytes()),
        "Authority signing key loaded — this node may produce blocks in its slots"
    );
    Some(Arc::new(key))
}

fn detect_local_addrs() -> Vec<std::net::IpAddr> {
    let mut out = Vec::new();
    if let Ok(v6) = local_ip_address::local_ipv6() {
        out.push(v6);
    }
    if let Ok(v4) = local_ip_address::local_ip() {
        out.push(v4);
    }
    out.retain(|ip| !ip.is_loopback() && !ip.is_unspecified());
    out
}

/// Whether an address another node passed on can mean anything here. A
/// private or link-local one only does when the node that passed it on is
/// itself on a private network, which this node then shares.
pub fn reachable_from_here(addr: &SocketAddr, from: &std::net::IpAddr) -> bool {
    use mesh_network::protocol::is_local_scope;
    !is_local_scope(&addr.ip()) || is_local_scope(from)
}

/// See `AppState::keep_new_connection`.
pub fn keep_new_link(old_outbound: bool, new_outbound: bool, peer: PeerId, me: PeerId) -> bool {
    if old_outbound == new_outbound {
        return true;
    }
    let new_dialer = if new_outbound { me } else { peer };
    new_dialer == me.min(peer)
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub async fn send_frame(conn: &Connection, frame_bytes: &[u8]) -> Result<(), String> {
    let (mut send, _recv) = tokio::time::timeout(SEND_OPEN_TIMEOUT, conn.open_bi())
        .await
        .map_err(|_| "timed out opening a stream".to_string())?
        .map_err(|e| format!("open_bi: {e}"))?;
    tokio::time::timeout(SEND_WRITE_TIMEOUT, send.write_all(frame_bytes))
        .await
        .map_err(|_| "timed out writing".to_string())?
        .map_err(|e| format!("write: {e}"))?;
    send.finish().map_err(|e| format!("finish: {e}"))?;
    let _ = tokio::time::timeout(SEND_OPEN_TIMEOUT, send.stopped()).await;
    Ok(())
}

pub fn spawn_send(conn: Connection, frame_bytes: Arc<Vec<u8>>, what: &'static str) {
    tokio::spawn(async move {
        if let Err(e) = send_frame(&conn, &frame_bytes).await {
            tracing::debug!(peer = %conn.remote_address(), error = %e, what, "send failed");
        }
    });
}

impl AppState {
    #[cfg(feature = "local-api-server")]
    fn load_routing_policy_from_config() -> nepheral_api::creds::CredsMode {
        crate::nepheral_config::load_config()
            .and_then(|c| c.api_routing_policy)
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    #[cfg(feature = "local-api-server")]
    pub async fn save_routing_policy(&self) {
        let policy = self.api_routing_policy.read().await.clone();
        let Ok(value) = serde_json::to_value(&policy) else {
            return;
        };
        if let Err(e) = crate::nepheral_config::update(|cfg| cfg.api_routing_policy = Some(value)) {
            tracing::warn!(error = %e, "could not persist routing policy");
        }
    }

    pub fn new(
        local_addr: SocketAddr,
        bootstrap_peers: Vec<SocketAddr>,
        signing_key: SigningKey,
        tls_identity: TlsIdentity,
    ) -> Self {
        let network = core_crypto::network::active();
        let data_dir = crate::nepheral_config::network_dir(network);
        let default_introducers: Vec<String> = std::env::var("NEPHERAL_BOOTSTRAP")
            .ok()
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_else(|| {
                network
                    .default_bootstraps()
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            });
        let my_pid = signing_key.verifying_key().to_bytes();

        Self {
            local_addr,
            node_id: generate_node_id(),
            network,
            data_dir,
            tls_identity,
            advertised_addrs: Arc::new(RwLock::new(detect_local_addrs())),
            observed_addr_votes: Arc::new(Mutex::new(HashMap::new())),
            peer_dialable: Arc::new(RwLock::new(HashMap::new())),
            peers: Arc::new(RwLock::new(HashMap::new())),
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            peer_id_by_addr: Arc::new(RwLock::new(HashMap::new())),
            bootstrap_introducers: Arc::new(RwLock::new(default_introducers)),
            discovered_peers: Arc::new(Mutex::new(HashMap::new())),
            failed_dial_peers: Arc::new(Mutex::new(HashMap::new())),
            bootstrap_peers: Arc::new(RwLock::new(bootstrap_peers)),
            mesh_endpoints: Arc::new(RwLock::new(None)),
            can_relay: Arc::new(AtomicBool::new(false)),
            mapped_addr: Arc::new(std::sync::RwLock::new(None)),
            inbound_strangers: Arc::new(std::sync::Mutex::new(HashSet::new())),
            punch_tried: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            punch_ok: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            relay_routes: Arc::new(RwLock::new(HashMap::new())),
            presence: Arc::new(RwLock::new(HashMap::new())),
            outbound_conns: Arc::new(std::sync::Mutex::new(HashSet::new())),
            punch_attempts: Arc::new(Mutex::new(HashMap::new())),
            punches_in_flight: Arc::new(Mutex::new(HashSet::new())),
            expected_punches: Arc::new(Mutex::new(HashMap::new())),
            punch_intros_sent: Arc::new(Mutex::new(HashMap::new())),
            custody_releases: Arc::new(RwLock::new(HashMap::new())),
            newest_peer_protocol: Arc::new(std::sync::atomic::AtomicU16::new(0)),
            upgrade_required: Arc::new(std::sync::atomic::AtomicU32::new(0)),

            is_running: Arc::new(Mutex::new(true)),
            peer_notify: Arc::new(Notify::new()),
            shutdown_notify: Arc::new(Notify::new()),
            discovery_notify: Arc::new(Notify::new()),
            start_time: Instant::now(),
            activity_log: Arc::new(Mutex::new(VecDeque::with_capacity(ACTIVITY_LOG_CAP))),

            ai_engine: Arc::new(RwLock::new(None)),
            ai_model_config: Arc::new(RwLock::new(ModelConfig::default())),
            ai_engine_status_cache: Arc::new(std::sync::Mutex::new("not_initialized".to_string())),
            ai_engine_init_lock: Arc::new(Mutex::new(())),
            inference_busy: Arc::new(Mutex::new(false)),
            extra_engines: Arc::new(RwLock::new(Vec::new())),
            extra_busy: Arc::new(Mutex::new(Vec::new())),
            local_inferences: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            observed_declines: Arc::new(RwLock::new(HashMap::new())),
            recent_declines: Arc::new(RwLock::new(HashMap::new())),
            slow_responders: Arc::new(RwLock::new(HashMap::new())),
            claim_reservation: Arc::new(Mutex::new(Vec::new())),

            my_peer_id: Arc::new(RwLock::new(my_pid)),
            signing_key: Arc::new(signing_key),
            peer_capabilities: Arc::new(RwLock::new(HashMap::new())),
            my_stats: Arc::new(RwLock::new(NodeStats::default())),
            peer_stats: Arc::new(RwLock::new(HashMap::new())),
            local_system_prompt: Arc::new(RwLock::new(None)),
            wallet_key: Arc::new(RwLock::new(None)),
            wallet_address: Arc::new(RwLock::new(None)),
            api_token: Arc::new(std::sync::RwLock::new(None)),
            api_token_hash: Arc::new(std::sync::RwLock::new(None)),
            last_activity_ms: Arc::new(std::sync::atomic::AtomicU64::new(now_ms())),
            auto_lock_ms: Arc::new(std::sync::atomic::AtomicU64::new(
                crate::wallet::DEFAULT_AUTO_LOCK_MINUTES * 60_000,
            )),
            confirm_hook: Arc::new(std::sync::RwLock::new(None)),

            rooms: Arc::new(RwLock::new(HashMap::new())),
            room_seen: Arc::new(Mutex::new(BoundedSet::new(16_384))),
            room_notify: Arc::new(Notify::new()),
            room_redactors: Arc::new(Mutex::new(HashMap::new())),

            model_download_progress: Arc::new(Mutex::new(None)),
            model_download_error: Arc::new(Mutex::new(None)),
            model_file_hash: Arc::new(RwLock::new(None)),
            model_meta: Arc::new(RwLock::new(None)),
            peers_with_model: Arc::new(RwLock::new(HashMap::new())),
            pending_model_requests: Arc::new(Mutex::new(HashMap::new())),
            active_model_uploads: Arc::new(AtomicUsize::new(0)),
            model_transfer_notify: Arc::new(Notify::new()),

            chain: Arc::new(RwLock::new(core_crypto::ChainState::new())),
            chain_notify: Arc::new(Notify::new()),
            chain_path: Arc::new(RwLock::new(None)),
            chain_persisted: Arc::new(Mutex::new((0, [0u8; 32]))),
            light_proof: Arc::new(Mutex::new(Default::default())),
            light: Arc::new(AtomicBool::new(false)),
            light_bootstrap_done: Arc::new(AtomicBool::new(false)),
            genesis_authority: Arc::new(std::sync::RwLock::new(core_crypto::chain::team_pubkey())),
            authority_key: load_authority_key_from_env(),
            pending_txs: Arc::new(Mutex::new(Vec::new())),
            my_inflight_txs: Arc::new(Mutex::new(Vec::new())),
            seen_chain_txs: Arc::new(Mutex::new(BoundedSet::new(8_192))),
            nonce_lock: Arc::new(Mutex::new(())),
            finality: Arc::new(Mutex::new(crate::finality::FinalityTracker::default())),

            custody: Arc::new(RwLock::new(HashMap::new())),
            observed_agent_claims: Arc::new(RwLock::new(HashMap::new())),
            early_claims: Arc::new(RwLock::new(HashMap::new())),
            agent_claim_collectors: Arc::new(RwLock::new(HashMap::new())),
            served_sessions: Arc::new(Mutex::new(HashMap::new())),
            agent_sessions: Arc::new(RwLock::new(HashMap::new())),
            session_redactors: Arc::new(Mutex::new(HashMap::new())),
            active_agent_session: Arc::new(RwLock::new(None)),
            next_agent_session_id: Arc::new(Mutex::new(1)),
            pending_queries: Arc::new(RwLock::new(HashMap::new())),
            agent_session_notify: Arc::new(Notify::new()),
            composer_min_maturity: Arc::new(RwLock::new(None)),
            conversations_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            conversations_saved_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            conversations_loaded: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            net_problem: Arc::new(std::sync::Mutex::new(None)),
            hw_seal: Arc::new(std::sync::RwLock::new(None)),
            #[cfg(feature = "client")]
            recall: Arc::new(RwLock::new(None)),
            #[cfg(feature = "client")]
            recall_status: Arc::new(std::sync::Mutex::new(
                crate::conversations::RecallStatus::Missing,
            )),
            #[cfg(feature = "client")]
            pending_sends: Arc::new(Mutex::new(HashMap::new())),

            #[cfg(feature = "local-api-server")]
            api_routing_policy: Arc::new(RwLock::new(Self::load_routing_policy_from_config())),

            stream_handler: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Remembers why the last attempt to reach the network failed. A
    /// clock or version problem is not overwritten by the plainer failures
    /// that follow from it.
    pub fn note_net_problem(&self, p: NetProblem) {
        let mut cur = self.net_problem.lock().expect("net problem");
        let sticky = matches!(*cur, Some(NetProblem::Clock) | Some(NetProblem::Update));
        if !sticky || matches!(p, NetProblem::Clock | NetProblem::Update) {
            *cur = Some(p);
        }
    }

    pub fn clear_net_problem(&self) {
        *self.net_problem.lock().expect("net problem") = None;
    }

    /// Something in the conversations changed: the saver writes them soon.
    pub fn conversations_changed(&self) {
        self.conversations_gen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub async fn set_my_peer_id(&self, pid: PeerId) {
        *self.my_peer_id.write().await = pid;
    }

    pub fn set_stream_handler(&self, handler: StreamHandler) {
        if self.stream_handler.set(handler).is_err() {
            tracing::warn!("Stream handler already installed — ignoring second install");
        }
    }

    pub async fn log(&self, line: impl Into<String>) {
        let line = line.into();
        let mut log = self.activity_log.lock().await;
        // A retry loop repeats itself; one line with a count says the same
        // without pushing everything else off the screen.
        if let Some(last) = log.back_mut() {
            if let Some(n) = repeats_of(last, &line) {
                *last = format!("{line} (×{})", n + 1);
                return;
            }
        }
        if log.len() >= ACTIVITY_LOG_CAP {
            log.pop_front();
        }
        log.push_back(line);
    }

    pub async fn sleep_or_shutdown(&self, dur: Duration) -> bool {
        let notified = self.shutdown_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !*self.is_running.lock().await {
            return true;
        }
        tokio::select! {
            _ = &mut notified => true,
            _ = tokio::time::sleep(dur) => false,
        }
    }

    pub async fn running(&self) -> bool {
        *self.is_running.lock().await
    }

    pub async fn begin_shutdown(&self) {
        *self.is_running.lock().await = false;
        self.shutdown_notify.notify_waiters();
        self.chain_notify.notify_waiters();
    }

    #[cfg(feature = "client")]
    pub fn format_uptime(&self) -> String {
        let elapsed = self.start_time.elapsed().as_secs();
        format!(
            "{:02}:{:02}:{:02}",
            elapsed / 3600,
            (elapsed % 3600) / 60,
            elapsed % 60
        )
    }

    pub fn am_i_authority(&self) -> bool {
        self.authority_key.is_some()
    }

    pub async fn update_peer(&self, addr: SocketAddr, peer_id: Option<PeerId>, connected: bool) {
        let mut peers = self.peers.write().await;
        let status = peers.entry(addr).or_insert_with(|| PeerStatus {
            addr,
            peer_id,
            connected: false,
            last_seen: Instant::now(),
        });
        if peer_id.is_some() {
            status.peer_id = peer_id;
        }
        status.connected = connected;
        status.last_seen = Instant::now();
        drop(peers);
        self.peer_notify.notify_waiters();
    }

    pub async fn live_connections(&self) -> Vec<(SocketAddr, PeerId, Connection)> {
        let conns = self.active_connections.read().await;
        let pids = self.peer_id_by_addr.read().await;
        conns
            .iter()
            .filter(|(_, c)| c.close_reason().is_none())
            .filter_map(|(a, c)| pids.get(a).map(|p| (*a, *p, c.clone())))
            .collect()
    }

    pub async fn broadcast(&self, msg_type: u8, payload: &[u8], exclude: Option<SocketAddr>) {
        let bytes = Arc::new(frame(msg_type, payload));
        for (addr, _, conn) in self.live_connections().await {
            if Some(addr) == exclude {
                continue;
            }
            spawn_send(conn, bytes.clone(), "broadcast");
        }
    }

    pub async fn register_connection(&self, addr: SocketAddr, conn: Connection) {
        let Some(pid) = mesh_network::tls::peer_id_of(&conn) else {
            conn.close(0u32.into(), b"unauthenticated");
            return;
        };
        let me = *self.my_peer_id.read().await;
        if pid == me {
            conn.close(0u32.into(), b"self");
            return;
        }
        // Already connected to this peer at another address, or by the other
        // direction. Both ends apply the same rule, so both keep the same
        // connection; otherwise each closes the one the other kept and the
        // pair ends up with none.
        let superseded = match self.connection_for_peer(&pid).await {
            Some((old_addr, old)) if old.stable_id() != conn.stable_id() => {
                if self.keep_new_connection(&old, &conn, pid, me) {
                    Some((old_addr, old))
                } else {
                    self.forget_outbound(&conn);
                    conn.close(0u32.into(), b"duplicate");
                    return;
                }
            }
            _ => None,
        };
        let replaced = self
            .active_connections
            .write()
            .await
            .insert(addr, conn.clone());
        if let Some(old) = replaced {
            if old.stable_id() != conn.stable_id() {
                old.close(0u32.into(), b"superseded");
            }
        }
        self.peer_id_by_addr.write().await.insert(addr, pid);
        self.update_peer(addr, Some(pid), true).await;
        self.spawn_stream_reader(addr, pid, conn.clone());
        if let Some((old_addr, old)) = superseded {
            if old_addr != addr {
                old.close(0u32.into(), b"superseded");
            }
        }

        let state = self.clone();
        tokio::spawn(async move {
            state.send_hello(&conn, Some(addr)).await;
            state.send_node_stats(&conn).await;
        });
    }

    /// Which of two connections to the same peer both ends keep. Opened in
    /// the same direction, the newer one: the older is likely a peer that
    /// restarted and has not timed out yet. In opposite directions, the one
    /// the lower identity dialed, which both ends can work out alone.
    fn keep_new_connection(
        &self,
        old: &Connection,
        new: &Connection,
        peer: PeerId,
        me: PeerId,
    ) -> bool {
        let outbound = self.outbound_conns.lock().expect("outbound lock");
        keep_new_link(
            outbound.contains(&old.stable_id()),
            outbound.contains(&new.stable_id()),
            peer,
            me,
        )
    }

    fn forget_outbound(&self, conn: &Connection) {
        self.outbound_conns
            .lock()
            .expect("outbound lock")
            .remove(&conn.stable_id());
    }

    fn spawn_stream_reader(&self, addr: SocketAddr, pid: PeerId, conn: Connection) {
        let Some(handler) = self.stream_handler.get().cloned() else {
            tracing::error!(remote = %addr, "No stream handler installed");
            return;
        };
        let state = self.clone();
        tokio::spawn(async move {
            loop {
                match conn.accept_bi().await {
                    Ok((send, recv)) => {
                        tokio::spawn(handler(send, recv, state.clone(), addr, pid));
                    }
                    Err(e) => {
                        tracing::info!(remote = %addr, error = %e, "Connection closed");
                        state.remove_connection(addr, conn.stable_id()).await;
                        state.forget_outbound(&conn);
                        break;
                    }
                }
            }
        });
    }

    pub async fn remove_connection(&self, addr: SocketAddr, stable_id: usize) {
        let removed = {
            let mut conns = self.active_connections.write().await;
            match conns.get(&addr) {
                Some(c) if c.stable_id() == stable_id => {
                    conns.remove(&addr);
                    true
                }
                _ => false,
            }
        };
        if !removed {
            return;
        }
        let pid = self.peer_id_by_addr.write().await.remove(&addr);
        if let Some(pid) = pid {
            let still_connected = self
                .peer_id_by_addr
                .read()
                .await
                .values()
                .any(|p| *p == pid);
            if !still_connected {
                self.peer_capabilities.write().await.remove(&pid);
                self.peers_with_model.write().await.remove(&pid);
                self.peer_stats.write().await.remove(&pid);
            }
        }
        self.update_peer(addr, None, false).await;
    }

    pub async fn connection_for_peer(&self, pid: &PeerId) -> Option<(SocketAddr, Connection)> {
        let addr = {
            let map = self.peer_id_by_addr.read().await;
            map.iter().find(|(_, p)| *p == pid).map(|(a, _)| *a)?
        };
        let conn = self.active_connections.read().await.get(&addr).cloned()?;
        if conn.close_reason().is_some() {
            return None;
        }
        Some((addr, conn))
    }

    pub async fn caps_byte(&self) -> u8 {
        let mut b = 0u8;
        if self.am_i_ai_capable().await {
            b |= caps_bits::HAS_AI;
        }
        if self.can_relay.load(Ordering::Relaxed) {
            b |= caps_bits::CAN_RELAY;
        }
        if self.serves_light().await {
            b |= caps_bits::SERVES_LIGHT;
        }
        if self.is_light() {
            b |= caps_bits::LIGHT;
        }
        b | caps_bits::SEALED_REPLIES
            | mesh_network::protocol::HwSeal::bits(*self.hw_seal.read().expect("hw seal"))
    }

    pub async fn am_i_ai_capable(&self) -> bool {
        let guard = self.ai_engine.read().await;
        match guard.as_ref() {
            Some(engine) => matches!(
                engine.get_state().await,
                ai_engine::EngineState::Ready | ai_engine::EngineState::Processing
            ),
            None => false,
        }
    }

    pub async fn my_model_info(&self) -> Option<mesh_network::protocol::ModelInfo> {
        if !self.am_i_ai_capable().await {
            return None;
        }
        let cfg = self.ai_model_config.read().await.clone();
        let hash = self
            .model_file_hash
            .read()
            .await
            .map(|(_, h)| h)
            .unwrap_or([0u8; 32]);
        let meta = match cfg.model_path.as_deref() {
            Some(path) => self.model_meta(path).await,
            None => None,
        };
        // A file name that says nothing of the model ("model.gguf") gives
        // way to the name the file carries inside.
        let name = match meta.as_ref().and_then(|m| m.name.clone()) {
            Some(inside) if crate::models::size_in_name(&cfg.model_name).is_none() => inside,
            _ => cfg.model_name.clone(),
        };
        let name: String = name
            .chars()
            .take(mesh_network::protocol::ModelInfo::MAX_NAME_LEN)
            .collect();
        // The weights the file holds, not what its name claims.
        let params_b = meta
            .and_then(|m| m.params_b)
            .unwrap_or_else(|| crate::models::params_b_from_name(&name));
        Some(mesh_network::protocol::ModelInfo {
            tier: crate::models::tier_for_params(params_b),
            name,
            hash,
            params_b,
        })
    }

    /// The GGUF header of the model at `path`, read once per file and size.
    pub async fn model_meta(&self, path: &str) -> Option<ModelMeta> {
        let size = tokio::fs::metadata(path).await.ok()?.len();
        if let Some(m) = self.model_meta.read().await.as_ref() {
            if m.path == path && m.size == size {
                return Some(m.clone());
            }
        }
        let owned = path.to_string();
        let info = tokio::task::spawn_blocking(move || {
            ai_engine::gguf_probe::probe(std::path::Path::new(&owned))
        })
        .await
        .ok()?;
        let meta = ModelMeta {
            path: path.to_string(),
            size,
            name: info
                .as_ref()
                .and_then(|i| i.name.clone())
                .filter(|n| !n.trim().is_empty()),
            params_b: info.as_ref().and_then(|i| i.params_b()),
        };
        *self.model_meta.write().await = Some(meta.clone());
        Some(meta)
    }

    pub async fn build_hello(&self, observed: Option<SocketAddr>) -> mesh_network::protocol::Hello {
        use ed25519_dalek::Signer;
        let mut addrs = self.advertised_addrs.read().await.clone();
        addrs.truncate(mesh_network::protocol::MAX_HELLO_ADDRS);
        let mut hello = mesh_network::protocol::Hello {
            version: mesh_network::protocol::PROTOCOL_VERSION,
            peer_id: *self.my_peer_id.read().await,
            listen_port: self.local_addr.port(),
            caps: self.caps_byte().await,
            model: self.my_model_info().await,
            addrs,
            sent_at_ms: now_ms(),
            observed,
            signature: [0u8; 64],
        };
        hello.signature = self.signing_key.sign(&hello.signing_digest()).to_bytes();
        hello
    }

    pub async fn announce_capabilities(&self) {
        for (addr, _, conn) in self.live_connections().await {
            let state = self.clone();
            tokio::spawn(async move {
                state.send_hello(&conn, Some(addr)).await;
            });
        }
        self.broadcast_presence().await;
    }

    pub async fn send_hello(&self, conn: &Connection, observed: Option<SocketAddr>) {
        let bytes = self.build_hello(observed).await.to_bytes();
        if let Err(e) = send_frame(conn, &frame(message_types::HELLO, &bytes)).await {
            tracing::debug!(error = %e, "HELLO send failed");
        }
    }

    pub async fn process_hello(
        &self,
        hello: mesh_network::protocol::Hello,
        source: SocketAddr,
        tls_pid: PeerId,
    ) -> Result<(), String> {
        self.note_peer_protocol(hello.version);
        if !mesh_network::protocol::speaks(hello.version) {
            if hello.version > mesh_network::protocol::PROTOCOL_VERSION {
                self.note_net_problem(NetProblem::Update);
            }
            return Err(format!(
                "peer speaks protocol v{}, this node needs v{} or later",
                hello.version,
                mesh_network::protocol::MIN_PROTOCOL_VERSION
            ));
        }
        if hello.peer_id != tls_pid {
            return Err("HELLO names an identity other than the one the connection proved".into());
        }
        hello.verify()?;
        if hello.sent_at_ms.abs_diff(now_ms()) > HELLO_MAX_SKEW_MS {
            self.note_net_problem(NetProblem::Clock);
            return Err("HELLO timestamp too far from local clock".into());
        }

        let mine = self.advertised_addrs.read().await.clone();
        let dialable = hello.dialable_addrs_from(source.ip(), &mine);
        self.peer_capabilities
            .write()
            .await
            .insert(tls_pid, PeerCapabilities::from_hello(&hello));
        self.remember_presence(hello.clone()).await;
        self.peer_dialable
            .write()
            .await
            .insert(tls_pid, dialable.clone());
        {
            let mut discovered = self.discovered_peers.lock().await;
            for addr in &dialable {
                if discovered.len() < DISCOVERED_PEERS_CAP {
                    discovered.entry(*addr).or_insert(60);
                }
            }
        }
        self.discovery_notify.notify_waiters();
        if let Some(observed) = hello.observed {
            self.vote_own_address(observed.ip(), tls_pid).await;
        }
        tracing::info!(
            peer_fp = %hex::encode(&tls_pid[..8]),
            source = %source,
            caps = format!("{:#04b}", hello.caps),
            model = ?hello.model.as_ref().map(|m| m.name.clone()),
            "HELLO from authenticated peer"
        );
        Ok(())
    }

    pub fn note_peer_protocol(&self, version: u16) {
        self.newest_peer_protocol
            .fetch_max(version, Ordering::Relaxed);
    }

    /// A peer speaks a newer protocol than this build: time to update.
    pub fn update_available(&self) -> bool {
        self.newest_peer_protocol.load(Ordering::Relaxed) > mesh_network::protocol::PROTOCOL_VERSION
    }

    pub async fn vote_own_address(&self, ip: std::net::IpAddr, voter: PeerId) {
        if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
            return;
        }
        let quorum_reached = {
            let mut votes = self.observed_addr_votes.lock().await;
            if votes.len() > 64 && !votes.contains_key(&ip) {
                return;
            }
            let voters = votes.entry(ip).or_default();
            if voters.len() < 16 {
                voters.insert(voter);
            }
            voters.len() >= OBSERVED_ADDR_QUORUM
        };
        let from_bootstrap = {
            let introducers = self.bootstrap_introducers.read().await.clone();
            let by_addr = self.peer_id_by_addr.read().await;
            let mut found = false;
            for (addr, pid) in by_addr.iter() {
                if *pid == voter
                    && introducers
                        .iter()
                        .any(|i| i.parse::<SocketAddr>().ok() == Some(*addr))
                {
                    found = true;
                }
            }
            found
        };
        if !quorum_reached && !from_bootstrap {
            return;
        }
        let mut addrs = self.advertised_addrs.write().await;
        if addrs.contains(&ip) {
            return;
        }
        if addrs.len() >= ADVERTISED_ADDRS_CAP {
            return;
        }
        addrs.push(ip);
        addrs.sort_by_key(|a| u8::from(a.is_ipv4()));
        tracing::info!(addr = %ip, "Learned own public address from peers");
    }

    pub async fn dialable_peer_entries(&self, exclude: PeerId) -> Vec<PeerEntry> {
        let dialable = self.peer_dialable.read().await;
        let mut out = Vec::new();
        for (_, pid, _) in self.live_connections().await {
            if pid == exclude {
                continue;
            }
            let Some(best) = dialable.get(&pid).and_then(|v| v.first()) else {
                continue;
            };
            if out
                .iter()
                .any(|e: &PeerEntry| e.addr == best.ip() && e.port == best.port())
            {
                continue;
            }
            out.push(PeerEntry {
                addr: best.ip(),
                port: best.port(),
                last_seen_ms: 0,
            });
            if out.len() >= mesh_network::protocol::MAX_PEER_LIST_ENTRIES {
                break;
            }
        }
        out
    }

    /// Sends this node's stats to every peer now, rather than at the next
    /// heartbeat. Called when it starts or stops answering, which is exactly
    /// what the others' lotteries want to know.
    pub async fn broadcast_node_stats(&self) {
        for (_, _, conn) in self.live_connections().await {
            let state = self.clone();
            tokio::spawn(async move {
                state.send_node_stats(&conn).await;
            });
        }
    }

    pub async fn send_node_stats(&self, conn: &Connection) {
        let full = self.all_workers_busy().await;
        let stats_bytes = {
            let mut stats = self.my_stats.write().await;
            stats.uptime_seconds = self.start_time.elapsed().as_secs();
            stats.queue_depth = u32::from(full);
            stats.compute_score();
            stats.to_bytes()
        };
        let _ = send_frame(conn, &frame(message_types::NODE_STATS, &stats_bytes)).await;
    }

    pub async fn install_mesh_endpoints(&self, v4: quinn::Endpoint, v6: Option<quinn::Endpoint>) {
        *self.mesh_endpoints.write().await = Some((v4, v6));
    }

    pub async fn mesh_client_for(
        &self,
        target: SocketAddr,
    ) -> Result<mesh_network::quic_client::MeshClient, String> {
        let guard = self.mesh_endpoints.read().await;
        let Some((v4, v6)) = guard.as_ref() else {
            return mesh_network::quic_client::MeshClient::ephemeral(
                target,
                self.tls_identity.clone(),
            );
        };
        let endpoint = if target.is_ipv6() {
            v6.clone()
                .ok_or_else(|| format!("no IPv6 listener; cannot dial {target}"))?
        } else {
            v4.clone()
        };
        Ok(mesh_network::quic_client::MeshClient::from_endpoint(
            endpoint,
            self.tls_identity.clone(),
        ))
    }

    pub async fn dial(&self, addr: SocketAddr, expected: Option<PeerId>) -> Result<PeerId, String> {
        let client = self.mesh_client_for(addr).await?;
        let (conn, pid) = client.connect_to_peer(addr, expected).await?;
        self.outbound_conns
            .lock()
            .expect("outbound lock")
            .insert(conn.stable_id());
        self.register_connection(addr, conn).await;
        Ok(pid)
    }

    pub async fn process_gossip_peer(
        &self,
        data: &[u8],
        from: std::net::IpAddr,
    ) -> Result<(), String> {
        let gossip = mesh_network::protocol::GossipPeer::from_bytes(data)?;
        if !reachable_from_here(&gossip.addr, &from) {
            return Ok(());
        }
        let mut discovered = self.discovered_peers.lock().await;
        if discovered.len() >= DISCOVERED_PEERS_CAP && !discovered.contains_key(&gossip.addr) {
            return Ok(());
        }
        let score = discovered.entry(gossip.addr).or_insert(0);
        *score = (*score + gossip.quality_score) / 2;
        drop(discovered);
        self.discovery_notify.notify_waiters();
        Ok(())
    }

    pub async fn gossip_flood(&self) {
        use mesh_network::protocol::GossipPeer;
        let mut announcements: Vec<SocketAddr> = Vec::new();
        if let Some(mine) = self.best_self_addr().await {
            announcements.push(mine);
        }
        {
            let dialable = self.peer_dialable.read().await;
            for (_, pid, _) in self.live_connections().await {
                if let Some(best) = dialable.get(&pid).and_then(|v| v.first()) {
                    announcements.push(*best);
                }
            }
        }
        announcements.sort();
        announcements.dedup();
        let connections = self.live_connections().await;
        for addr in announcements {
            let bytes = Arc::new(frame(
                message_types::GOSSIP_PEER,
                &GossipPeer::new(addr, 60).to_bytes(),
            ));
            for (target, _, conn) in &connections {
                if target.ip() == addr.ip() {
                    continue;
                }
                spawn_send(conn.clone(), bytes.clone(), "gossip");
            }
        }
    }

    pub async fn best_self_addr(&self) -> Option<SocketAddr> {
        if let Some(mapped) = *self.mapped_addr.read().expect("mapped addr") {
            return Some(mapped);
        }
        self.advertised_addrs
            .read()
            .await
            .first()
            .map(|ip| SocketAddr::new(*ip, self.local_addr.port()))
    }

    pub async fn connect_discovered_peers(&self) {
        const DIAL_BACKOFF: Duration = Duration::from_secs(5 * 60);
        const MAX_PARALLEL_DIALS: usize = 8;
        let now = Instant::now();
        self.failed_dial_peers
            .lock()
            .await
            .retain(|_, last| now.duration_since(*last) < DIAL_BACKOFF);
        let connected_addrs: HashSet<SocketAddr> = self
            .active_connections
            .read()
            .await
            .keys()
            .copied()
            .collect();
        let blacklisted: HashSet<SocketAddr> = self
            .failed_dial_peers
            .lock()
            .await
            .keys()
            .copied()
            .collect();
        let known_addrs: HashSet<SocketAddr> = {
            let dialable = self.peer_dialable.read().await;
            self.live_connections()
                .await
                .iter()
                .filter_map(|(_, pid, _)| dialable.get(pid))
                .flatten()
                .copied()
                .collect()
        };
        let mine: HashSet<SocketAddr> = {
            let port = self.local_addr.port();
            self.advertised_addrs
                .read()
                .await
                .iter()
                .map(|ip| SocketAddr::new(*ip, port))
                .collect()
        };
        let to_connect: Vec<SocketAddr> = self
            .discovered_peers
            .lock()
            .await
            .keys()
            .filter(|addr| {
                !connected_addrs.contains(addr)
                    && !blacklisted.contains(addr)
                    && !known_addrs.contains(addr)
                    && !mine.contains(addr)
            })
            .take(MAX_PARALLEL_DIALS)
            .copied()
            .collect();
        for addr in to_connect {
            let state = self.clone();
            tokio::spawn(async move {
                match state.dial(addr, None).await {
                    Ok(pid) => {
                        state.failed_dial_peers.lock().await.remove(&addr);
                        state
                            .log(format!(
                                "Connected to discovered peer {addr} ({})",
                                hex::encode(&pid[..4])
                            ))
                            .await;
                    }
                    Err(e) => {
                        state
                            .failed_dial_peers
                            .lock()
                            .await
                            .insert(addr, Instant::now());
                        tracing::debug!(peer = %addr, error = %e, "dial failed — backing off");
                    }
                }
            });
        }
    }

    #[cfg(feature = "client")]
    pub async fn add_bootstrap_introducer(&self, addr: String) {
        let mut introducers = self.bootstrap_introducers.write().await;
        if !introducers.contains(&addr) && introducers.len() < 32 {
            introducers.push(addr.clone());
            drop(introducers);
            self.log(format!("Added bootstrap introducer: {addr}"))
                .await;
        }
    }

    pub async fn query_bootstrap_introducer(
        &self,
        introducer_addr: &str,
    ) -> Result<Vec<SocketAddr>, String> {
        let introducer: SocketAddr = tokio::net::lookup_host(introducer_addr)
            .await
            .map_err(|e| format!("DNS resolution failed for '{introducer_addr}': {e}"))?
            .next()
            .ok_or_else(|| format!("No addresses found for '{introducer_addr}'"))?;
        let existing = self
            .active_connections
            .read()
            .await
            .get(&introducer)
            .cloned()
            .filter(|c| c.close_reason().is_none());
        let conn = match existing {
            Some(c) => c,
            None => {
                self.dial(introducer, None).await?;
                self.active_connections
                    .read()
                    .await
                    .get(&introducer)
                    .cloned()
                    .ok_or_else(|| "introducer connection vanished".to_string())?
            }
        };
        self.request_peer_list(&conn).await
    }

    pub async fn request_peer_list(&self, conn: &Connection) -> Result<Vec<SocketAddr>, String> {
        use mesh_network::protocol::{PeerList, MAX_PEER_LIST_BYTES};
        const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
        let (mut send, mut recv) = tokio::time::timeout(REPLY_TIMEOUT, conn.open_bi())
            .await
            .map_err(|_| "timed out opening a stream for the peer list".to_string())?
            .map_err(|e| format!("Failed to open stream: {e}"))?;
        send.write_all(&[message_types::PEER_LIST_REQUEST])
            .await
            .map_err(|e| format!("Failed to send request: {e}"))?;
        send.finish()
            .map_err(|e| format!("Failed to finish request: {e}"))?;
        let reply = tokio::time::timeout(REPLY_TIMEOUT, async {
            let mut header = [0u8; 5];
            recv.read_exact(&mut header)
                .await
                .map_err(|e| format!("Failed to read peer list header: {e}"))?;
            if header[0] != message_types::PEER_LIST {
                return Err(format!("expected PEER_LIST, got 0x{:02x}", header[0]));
            }
            let data_len =
                u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
            if data_len > MAX_PEER_LIST_BYTES {
                return Err(format!("PEER_LIST payload too large: {data_len}"));
            }
            let mut payload = vec![0u8; data_len];
            recv.read_exact(&mut payload)
                .await
                .map_err(|e| format!("Failed to read peer list: {e}"))?;
            Ok(payload)
        })
        .await
        .map_err(|_| "Timed out waiting for peer list".to_string())??;
        let from = conn.remote_address().ip();
        let discovered: Vec<SocketAddr> = PeerList::from_bytes(&reply)?
            .to_socket_addrs()
            .into_iter()
            .filter(|a| reachable_from_here(a, &from))
            .collect();
        self.record_discovered(&discovered, 50).await;
        Ok(discovered)
    }

    pub async fn record_discovered(&self, addrs: &[SocketAddr], score: u32) {
        let mut map = self.discovered_peers.lock().await;
        for peer in addrs {
            if map.len() >= DISCOVERED_PEERS_CAP {
                break;
            }
            map.entry(*peer).or_insert(score);
        }
        drop(map);
        self.discovery_notify.notify_waiters();
    }

    pub async fn prune_stale_peers(&self) {
        let stale = Duration::from_secs(300);
        self.peers
            .write()
            .await
            .retain(|_, s| s.connected || s.last_seen.elapsed() < stale);
        let live: HashSet<PeerId> = self
            .live_connections()
            .await
            .into_iter()
            .map(|(_, p, _)| p)
            .collect();
        self.peer_dialable
            .write()
            .await
            .retain(|pid, _| live.contains(pid));
        let mut discovered = self.discovered_peers.lock().await;
        if discovered.len() > DISCOVERED_PEERS_CAP / 2 {
            let mut entries: Vec<(SocketAddr, u32)> = discovered.drain().collect();
            entries.sort_by_key(|(_, q)| std::cmp::Reverse(*q));
            entries.truncate(DISCOVERED_PEERS_CAP / 2);
            discovered.extend(entries);
        }
        self.observed_addr_votes
            .lock()
            .await
            .retain(|_, voters| voters.iter().any(|v| live.contains(v)));
        self.prune_presence().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_ends_of_a_double_dial_keep_the_same_connection() {
        let (low, high) = ([1u8; 32], [2u8; 32]);
        // Low dialed one connection, high dialed the other. On low's side
        // its own is outbound; on high's side the same one is inbound.
        let low_keeps_its_own = !keep_new_link(true, false, high, low);
        let high_keeps_lows = keep_new_link(true, false, low, high);
        assert!(low_keeps_its_own && high_keeps_lows);
        // Same direction: the newer one wins, whoever is lower.
        assert!(keep_new_link(false, false, low, high));
        assert!(keep_new_link(true, true, high, low));
    }

    #[test]
    fn a_private_address_from_a_public_peer_is_not_dialed() {
        let public: std::net::IpAddr = "93.189.89.201".parse().unwrap();
        let lan: std::net::IpAddr = "192.168.1.4".parse().unwrap();
        let vpn: SocketAddr = "10.2.0.2:443".parse().unwrap();
        assert!(!reachable_from_here(&vpn, &public));
        assert!(reachable_from_here(&vpn, &lan));
        assert!(reachable_from_here(
            &"8.8.8.8:443".parse().unwrap(),
            &public
        ));
    }

    #[test]
    fn bounded_set_evicts_the_oldest() {
        let mut s = BoundedSet::new(2);
        assert!(s.insert(1));
        assert!(s.insert(2));
        assert!(!s.insert(2));
        assert!(s.insert(3));
        assert!(!s.contains(&1));
        assert!(s.contains(&3));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn node_stats_roundtrip_and_reject_non_finite_scores() {
        let mut stats = NodeStats {
            shares_sent: 3,
            performance_score: f64::NAN,
            ..Default::default()
        };
        let back = NodeStats::from_bytes(&stats.to_bytes()).unwrap();
        assert_eq!(back.shares_sent, 3);
        assert_eq!(back.performance_score, 0.0);
        stats.performance_score = 1.5;
        assert_eq!(
            NodeStats::from_bytes(&stats.to_bytes())
                .unwrap()
                .performance_score,
            1.5
        );
    }
}

#[cfg(test)]
mod net_problem_tests {
    use super::NetProblem;

    #[test]
    fn dial_failures_say_why() {
        assert_eq!(NetProblem::of_dial_error("timed out"), NetProblem::Blocked);
        assert_eq!(
            NetProblem::of_dial_error("connect: Network is unreachable (os error 101)"),
            NetProblem::NoInternet
        );
        assert_eq!(
            NetProblem::of_dial_error("handshake failed"),
            NetProblem::Trying
        );
    }
}
