// SPDX-License-Identifier: BUSL-1.1
//! Tauri command surface over the shared client-node runtime. Each
//! `*_view` command returns a snapshot DTO; mutations call the same
//! AppState methods the runtime itself exposes.

use app_kernel::nepheral_config::{self, NepheralConfig};
use app_kernel::state::{AgentSessionKind, AppState};
use core_crypto::extrinsic::DecodedCall;
use core_crypto::identity::PeerId;
use serde::{Deserialize, Serialize};

pub struct Desktop {
    pub state: AppState,
    pub first_run: std::sync::Mutex<bool>,
    /// Wizard draft config; persisted only by `setup_finish` (and by
    /// AI-settings apply/save actions).
    pub draft: std::sync::Mutex<NepheralConfig>,
    /// The open vault and what it holds, while the wallet is unlocked
    /// (lock.rs). Dropped from memory when it locks.
    pub open: crate::lock::Open,
    /// The node's own key and system prompt, sealed by the OS.
    pub node_store: std::sync::Mutex<crate::node_store::NodeStore>,
    /// The window's language, for dialogs raised outside it.
    pub ui_lang: std::sync::Arc<std::sync::Mutex<String>>,
    /// The model sync (adopt what is on disk, or fetch the recommended
    /// model) has been started. The node starts it itself except on a first
    /// run, where the wizard does once the user has had the chance to pick.
    pub model_sync_started: std::sync::atomic::AtomicBool,
}

impl Desktop {
    pub fn new(
        state: AppState,
        first_run: bool,
        node_store: crate::node_store::NodeStore,
        opened: Option<(app_kernel::vault::Vault, app_kernel::vault::Secrets)>,
    ) -> Self {
        Self {
            state,
            first_run: std::sync::Mutex::new(first_run),
            draft: std::sync::Mutex::new(nepheral_config::load_config().unwrap_or_default()),
            open: std::sync::Arc::new(std::sync::Mutex::new(opened)),
            node_store: std::sync::Mutex::new(node_store),
            ui_lang: std::sync::Arc::new(std::sync::Mutex::new("en".into())),
            model_sync_started: std::sync::atomic::AtomicBool::new(!first_run),
        }
    }

    /// Starts the model sync if nothing else will: not when a model download
    /// the user chose is already running, not for an external backend, and
    /// only once.
    async fn start_model_sync(&self, external: bool) {
        use std::sync::atomic::Ordering;
        if external || self.state.model_download_progress.lock().await.is_some() {
            return;
        }
        if self.model_sync_started.swap(true, Ordering::SeqCst) {
            return;
        }
        tokio::spawn(self.state.clone().run_model_sync());
    }
}

type D<'a> = tauri::State<'a, Desktop>;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.as_millis() as u64)
        .unwrap_or(0)
}

fn percentile(sorted_src: &[u64], pct: f64) -> u64 {
    if sorted_src.is_empty() {
        return 0;
    }
    let mut v = sorted_src.to_vec();
    v.sort_unstable();
    let idx = ((v.len() - 1) as f64 * pct.clamp(0.0, 1.0)).round() as usize;
    v[idx]
}

fn maturity_tier(value: u64, p50: u64, p90: u64, p99: u64) -> &'static str {
    if value == 0 {
        "Newcomer"
    } else if value >= p99 && p99 > 0 {
        "Elite p99+"
    } else if value >= p90 && p90 > 0 {
        "Veteran p90+"
    } else if value >= p50 && p50 > 0 {
        "Trusted p50+"
    } else {
        "Rising"
    }
}

// ============================ status ============================

#[derive(Serialize)]
pub struct StatusView {
    peer_id_short: String,
    peer_id: String,
    local_addr: String,
    peers_connected: usize,
    ai_status: String,
    chain_height: u64,
    finalized_height: u64,
    chain_blocks: usize,
    shares_per_credit: u64,
    balance: u64,
    model: String, // "ready" | "downloading:{pct}" | "none"
    uptime: String,
    is_authority: bool,
    first_run: bool,
    network: String,
    unlocked: bool,
    /// A peer speaks a newer protocol: this build should be updated.
    update_available: bool,
    /// New chain rules this build does not implement: scheduled (the date
    /// to update by) or already in force (the node stopped following).
    rules_upgrade: Option<RulesUpgradeView>,
    /// With no peers: why, as far as the node can tell.
    net_problem: Option<app_kernel::state::NetProblem>,
    /// The hardware that seals this node's own key.
    my_seal: Option<mesh_network::protocol::HwSeal>,
}

#[derive(Serialize)]
pub struct RulesUpgradeView {
    version: u32,
    activate_at_ms: u64,
    /// Already in force: the node has stopped following the chain.
    required: bool,
}

#[tauri::command]
pub async fn status(d: D<'_>) -> Result<StatusView, String> {
    let s = &d.state;
    let me = *s.my_peer_id.read().await;
    let account = s.my_account().await;
    let peers_connected = s
        .peers
        .read()
        .await
        .values()
        .filter(|p| p.connected)
        .count();
    let finalized_height = s.observed_finality().await.0;
    let (chain_height, chain_blocks, shares_per_credit, balance, unsupported) = {
        let c = s.chain.read().await;
        (
            c.head_height(),
            c.block_count(),
            c.economy.current_shares_per_credit,
            c.balance_of(&account),
            c.upgrade
                .scheduled_unsupported()
                .map(|u| (u.version, u.activate_at_ms)),
        )
    };
    let rules_upgrade = match (s.upgrade_required(), unsupported) {
        (Some(version), u) => Some(RulesUpgradeView {
            version,
            activate_at_ms: u.map(|(_, at)| at).unwrap_or(0),
            required: true,
        }),
        (None, Some((version, activate_at_ms))) => Some(RulesUpgradeView {
            version,
            activate_at_ms,
            required: false,
        }),
        (None, None) => None,
    };
    // "ready" means answering, not merely that a model file is on disk: a
    // file the engine could not load used to show as ready while every
    // query went elsewhere.
    let model = if s.am_i_ai_capable().await {
        "ready".to_string()
    } else if let Some((done, total)) = *s.model_download_progress.lock().await {
        format!(
            "downloading:{}",
            (done * 100).checked_div(total).unwrap_or(0)
        )
    } else if AppState::local_model_path().is_some() {
        "stopped".to_string()
    } else {
        "none".to_string()
    };
    let is_authority = s.am_i_authority();
    let unlocked = s.is_unlocked().await;
    Ok(StatusView {
        peer_id_short: hex::encode(&me[..8]),
        peer_id: hex::encode(me),
        local_addr: s.local_addr.to_string(),
        peers_connected,
        ai_status: s.ai_engine_status(),
        chain_height,
        finalized_height,
        chain_blocks,
        shares_per_credit,
        balance,
        model,
        uptime: s.format_uptime(),
        is_authority,
        first_run: *d.first_run.lock().expect("first_run lock"),
        network: s.network.name().to_string(),
        unlocked,
        update_available: s.update_available(),
        rules_upgrade,
        my_seal: *s.hw_seal.read().expect("hw seal"),
        net_problem: if peers_connected == 0 {
            Some(
                (*s.net_problem.lock().expect("net problem"))
                    .unwrap_or(app_kernel::state::NetProblem::Trying),
            )
        } else {
            None
        },
    })
}

#[tauri::command]
pub async fn activity_log(d: D<'_>) -> Result<Vec<String>, String> {
    let log = d.state.activity_log.lock().await;
    Ok(log.iter().rev().take(100).cloned().collect())
}

// ============================ chat ============================

#[derive(Serialize)]
pub struct ChatSessionRow {
    id: u64,
    kind: String,
    label: String,
    msgs: usize,
    awaiting: bool,
    active: bool,
    /// Seconds since the last message (or since it was created).
    idle_secs: u64,
    /// The start of the last message, for the list.
    last: String,
}

#[tauri::command]
pub async fn chat_sessions(d: D<'_>) -> Result<Vec<ChatSessionRow>, String> {
    d.state.require_unlocked().await?;
    let active = *d.state.active_agent_session.read().await;
    let sessions = d.state.agent_sessions.read().await;
    let mut rows: Vec<ChatSessionRow> = sessions
        .values()
        .map(|s| ChatSessionRow {
            id: s.id,
            kind: match s.kind {
                AgentSessionKind::Local => "local".into(),
                AgentSessionKind::P2P => "p2p".into(),
            },
            label: s.label.clone(),
            msgs: s.messages.len(),
            awaiting: s.awaiting_response,
            active: active == Some(s.id),
            idle_secs: s
                .messages
                .last()
                .map(|m| m.timestamp.elapsed().as_secs())
                .unwrap_or_else(|| s.created_at.elapsed().as_secs()),
            last: s
                .messages
                .last()
                .map(|m| m.content.chars().take(80).collect())
                .unwrap_or_default(),
        })
        .collect();
    // Most recent first.
    rows.sort_by_key(|r| (r.idle_secs, std::cmp::Reverse(r.id)));
    Ok(rows)
}

#[derive(Serialize)]
pub struct ChatMessageRow {
    from_me: bool,
    content: String,
    elapsed_secs: u64,
    rateable: bool,
    rated: Option<bool>,
    /// Never chosen as context, never sent.
    private: bool,
    /// Not an answer: what went wrong, as a code the UI words
    /// (`app_kernel::agent::error_code`).
    error: Option<&'static str>,
}

#[derive(Serialize)]
pub struct ChatThreadView {
    id: u64,
    kind: String,
    label: String,
    messages: Vec<ChatMessageRow>,
    awaiting: bool,
    awaiting_secs: u64,
    /// A node took the question and is writing the answer.
    taken: bool,
    /// The context a waiting question would carry, for the person to
    /// confirm (docs/CONVERSATIONS.md).
    pending: Option<app_kernel::conversations::ContextPreview>,
}

#[tauri::command]
pub async fn chat_thread(d: D<'_>, id: u64) -> Result<ChatThreadView, String> {
    d.state.require_unlocked().await?;
    let pending = d
        .state
        .pending_sends
        .lock()
        .await
        .get(&id)
        .map(|p| app_kernel::conversations::ContextPreview::of(&p.plan));
    let taken = d.state.session_answer_taken(id).await;
    let sessions = d.state.agent_sessions.read().await;
    let s = sessions.get(&id).ok_or("session gone")?;
    let awaiting_secs = s
        .messages
        .iter()
        .rev()
        .find(|m| m.from_me)
        .map(|m| m.timestamp.elapsed().as_secs())
        .unwrap_or(0);
    Ok(ChatThreadView {
        id: s.id,
        kind: match s.kind {
            AgentSessionKind::Local => "local".into(),
            AgentSessionKind::P2P => "p2p".into(),
        },
        label: s.label.clone(),
        messages: s
            .messages
            .iter()
            .map(|m| ChatMessageRow {
                from_me: m.from_me,
                content: m.content.clone(),
                elapsed_secs: m.timestamp.elapsed().as_secs(),
                rateable: m.rating.is_some(),
                rated: m.rated,
                private: m.private,
                error: m.error.then(|| app_kernel::agent::error_code(&m.content)),
            })
            .collect(),
        awaiting: s.awaiting_response,
        awaiting_secs,
        taken,
        pending,
    })
}

#[tauri::command]
pub async fn chat_new(d: D<'_>, kind: String) -> Result<u64, String> {
    d.state.require_unlocked().await?;
    let k = match kind.as_str() {
        "local" => AgentSessionKind::Local,
        _ => AgentSessionKind::P2P,
    };
    Ok(d.state.create_agent_session(k).await)
}

#[tauri::command]
pub async fn chat_select(d: D<'_>, id: u64) -> Result<(), String> {
    d.state.require_unlocked().await?;
    d.state.set_active_agent_session(Some(id)).await;
    Ok(())
}

#[tauri::command]
pub async fn chat_leave(d: D<'_>, id: u64) -> Result<(), String> {
    d.state.require_unlocked().await?;
    d.state.leave_agent_session(id).await;
    Ok(())
}

/// Sends a message, or with `private` only keeps it. The answer, or the
/// context to confirm, shows up in `chat_thread`.
#[tauri::command]
pub async fn chat_send(
    d: D<'_>,
    id: u64,
    content: String,
    private: Option<bool>,
) -> Result<(), String> {
    d.state.require_unlocked().await?;
    let msg = content.trim().to_string();
    if msg.is_empty() {
        return Ok(());
    }
    let state = d.state.clone();
    let private = private.unwrap_or(false);
    tokio::spawn(async move {
        if let Err(e) = state.send_to_agent_session(id, &msg, private).await {
            tracing::warn!(error = %e, session_id = id, "send_to_agent_session");
        }
    });
    Ok(())
}

/// Sends the waiting question with the older messages the person kept
/// (indices from `pending.fragments`; none to send it without them).
#[tauri::command]
pub async fn chat_confirm(d: D<'_>, id: u64, keep: Vec<usize>) -> Result<(), String> {
    d.state.require_unlocked().await?;
    let state = d.state.clone();
    tokio::spawn(async move {
        if let Err(e) = state.confirm_agent_send(id, &keep).await {
            tracing::warn!(error = %e, session_id = id, "confirm_agent_send");
        }
    });
    Ok(())
}

/// Takes the waiting question back: its text returns to the input box.
#[tauri::command]
pub async fn chat_cancel(d: D<'_>, id: u64) -> Result<Option<String>, String> {
    d.state.require_unlocked().await?;
    d.state.cancel_agent_send(id).await
}

#[tauri::command]
pub async fn chat_rename(d: D<'_>, id: u64, label: String) -> Result<(), String> {
    d.state.require_unlocked().await?;
    d.state.rename_agent_session(id, &label).await
}

#[tauri::command]
pub async fn chat_private(d: D<'_>, id: u64, index: usize, private: bool) -> Result<(), String> {
    d.state.require_unlocked().await?;
    d.state.set_message_private(id, index, private).await
}

#[derive(Serialize)]
pub struct MemoryView {
    model: app_kernel::conversations::RecallStatus,
    size_bytes: u64,
    keep_conversations: bool,
    /// Questions go without the confirmation card until then (Unix ms).
    snoozed_until_ms: Option<u64>,
}

/// The NAI private memory: whether its model is on the device, and whether
/// conversations are kept between runs.
#[tauri::command]
pub async fn memory_view(d: D<'_>) -> Result<MemoryView, String> {
    Ok(MemoryView {
        model: d.state.recall_status(),
        size_bytes: app_kernel::conversations::RECALL_SIZE,
        keep_conversations: app_kernel::conversations::keep_conversations(),
        snoozed_until_ms: app_kernel::conversations::confirm_snoozed_until(),
    })
}

#[tauri::command]
pub async fn memory_download(d: D<'_>) -> Result<(), String> {
    let state = d.state.clone();
    tokio::spawn(async move {
        let _ = state.download_recall().await;
    });
    Ok(())
}

#[tauri::command]
pub async fn memory_keep(d: D<'_>, keep: bool) -> Result<(), String> {
    d.state.require_unlocked().await?;
    d.state.set_keep_conversations(keep).await
}

/// "Don't ask for a while" on the confirmation card: no card for `minutes`;
/// 0 asks again from now on. Returns when the pause ends.
#[tauri::command]
pub async fn chat_snooze(d: D<'_>, minutes: u32) -> Result<Option<u64>, String> {
    d.state.require_unlocked().await?;
    app_kernel::conversations::snooze_confirmations(minutes.min(7 * 24 * 60))
}

/// levels: "any" | "p50" | "p90" | "p99" — the leaderboard trust filter.
#[tauri::command]
pub async fn set_trust_filter(d: D<'_>, level: String) -> Result<Option<u64>, String> {
    d.state.require_unlocked().await?;
    let pct = match level.as_str() {
        "any" => None,
        "p50" => Some(0.50),
        "p90" => Some(0.90),
        "p99" => Some(0.99),
        _ => return Err("unknown level".into()),
    };
    let threshold = match pct {
        None => None,
        Some(p) => {
            let chain = d.state.chain.read().await;
            let all: Vec<u64> = chain
                .contributions
                .share_counts
                .keys()
                .chain(chain.contributions.lottery_dimension_by_peer.keys())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .map(|pid| chain.contributions.maturity_of(pid))
                .filter(|v| *v > 0)
                .collect();
            if all.is_empty() {
                Some(0)
            } else {
                Some(percentile(&all, p))
            }
        }
    };
    *d.state.composer_min_maturity.write().await = threshold;
    Ok(threshold)
}

// ============================ rooms ============================

#[derive(Serialize)]
pub struct RoomRow {
    code: String,
    members: usize,
    msgs: usize,
    closing_in_secs: Option<u64>,
}

#[tauri::command]
pub async fn rooms_list(d: D<'_>) -> Result<Vec<RoomRow>, String> {
    d.state.require_unlocked().await?;
    let rooms = d.state.rooms.read().await;
    let mut rows: Vec<RoomRow> = rooms
        .iter()
        .map(|(code, st)| RoomRow {
            code: code.clone(),
            members: st.members.len() + 1,
            msgs: st.chat.len(),
            closing_in_secs: st.grace_remaining().map(|g| g.as_secs()),
        })
        .collect();
    rows.sort_by(|a, b| a.code.cmp(&b.code));
    Ok(rows)
}

#[tauri::command]
pub async fn room_join(d: D<'_>, code: String) -> Result<String, String> {
    d.state.require_unlocked().await?;
    let parsed = mesh_network::rooms::RoomCode::parse(code.trim())
        .map_err(|e| format!("Bad room code: {e}"))?;
    d.state.join_room(parsed).await
}

#[tauri::command]
pub async fn room_create(d: D<'_>) -> Result<String, String> {
    d.state.require_unlocked().await?;
    d.state
        .join_room(mesh_network::rooms::RoomCode::generate())
        .await
}

#[tauri::command]
pub async fn room_leave(d: D<'_>, code: String) -> Result<(), String> {
    d.state.require_unlocked().await?;
    d.state.leave_room(&code).await;
    Ok(())
}

#[derive(Serialize)]
pub struct RoomMsgRow {
    id: u64,
    codename: String,
    content: String,
    is_ai: bool,
    elapsed_secs: u64,
    rateable: bool,
    rated: Option<bool>,
    /// A failure the room shows ("system"), as a code the UI words.
    error: Option<&'static str>,
}

#[derive(Serialize)]
pub struct RoomView {
    code: String,
    members: usize,
    member_names: Vec<String>,
    me: String,
    ai_thinking: bool,
    closing_in_secs: Option<u64>,
    chat: Vec<RoomMsgRow>,
}

#[tauri::command]
pub async fn room_view(d: D<'_>, code: String) -> Result<RoomView, String> {
    d.state.require_unlocked().await?;
    let me = *d.state.my_peer_id.read().await;
    let rooms = d.state.rooms.read().await;
    let st = rooms.get(&code).ok_or("room gone")?;
    let mut member_names: Vec<String> = st.members.values().map(|m| m.codename.clone()).collect();
    member_names.sort();
    Ok(RoomView {
        me: mesh_network::rooms::animal_for(&me, &st.code),
        code,
        members: st.members.len() + 1,
        member_names,
        ai_thinking: st.ai_thinking,
        closing_in_secs: st.grace_remaining().map(|g| g.as_secs()),
        chat: st
            .chat
            .iter()
            .map(|e| RoomMsgRow {
                id: e.id,
                codename: e.codename.clone(),
                content: e.content.clone(),
                is_ai: e.is_ai,
                elapsed_secs: e.timestamp.elapsed().as_secs(),
                rateable: e.rating.is_some(),
                rated: e.rated,
                error: (e.codename == "system" && !e.is_ai)
                    .then(|| app_kernel::agent::error_code(&e.content)),
            })
            .collect(),
    })
}

fn verdict_from(
    like: bool,
    reason: Option<String>,
    note: Option<String>,
) -> Result<core_crypto::pallets::reputation::Verdict, String> {
    use core_crypto::pallets::reputation::{DislikeReason, Verdict};
    if like {
        return Ok(Verdict::Like);
    }
    let reason = reason
        .as_deref()
        .and_then(DislikeReason::parse)
        .ok_or("pick a reason for the dislike")?;
    Ok(Verdict::Dislike {
        reason,
        note: note.unwrap_or_default(),
    })
}

#[tauri::command]
pub async fn rate_chat_reply(
    d: D<'_>,
    id: u64,
    index: usize,
    like: bool,
    reason: Option<String>,
    note: Option<String>,
) -> Result<(), String> {
    d.state.require_unlocked().await?;
    let verdict = verdict_from(like, reason, note)?;
    d.state.rate_session_reply(id, index, verdict).await
}

#[tauri::command]
pub async fn rate_room_reply(
    d: D<'_>,
    code: String,
    message_id: u64,
    like: bool,
    reason: Option<String>,
    note: Option<String>,
) -> Result<(), String> {
    d.state.require_unlocked().await?;
    let verdict = verdict_from(like, reason, note)?;
    d.state.rate_room_reply(&code, message_id, verdict).await
}

#[tauri::command]
pub async fn room_send(d: D<'_>, code: String, content: String) -> Result<(), String> {
    d.state.require_unlocked().await?;
    let msg = content.trim().to_string();
    if msg.is_empty() {
        return Ok(());
    }
    let parsed =
        mesh_network::rooms::RoomCode::parse(&code).map_err(|e| format!("Bad room code: {e}"))?;
    d.state.send_room_message(&parsed, &msg).await
}

// ============================ peers ============================

#[derive(Serialize)]
pub struct PeersView {
    connected: Vec<PeerRow>,
    /// Alive and known through other nodes, with no direct connection:
    /// reached through the relays until a hole punch connects them.
    relayed: Vec<RelayedRow>,
    /// Addresses heard of (gossip, presence) that this node has never
    /// been connected to: only how many. Listing them would show the IP
    /// of nodes that never chose to talk to this one.
    discovered: usize,
    ai_ready_total: usize,
    caps_known_total: usize,
}

#[derive(Serialize)]
pub struct PeerRow {
    addr: String,
    peer: String,
    connected: bool,
    last_seen_secs: u64,
    ai_ready: Option<bool>,
    model: Option<String>,
    rating: Option<u64>,
    rating_votes: u64,
    /// The hardware that seals the node's key, as it declares it.
    seal: Option<mesh_network::protocol::HwSeal>,
}

#[derive(Serialize)]
pub struct RelayedRow {
    peer: String,
    ai_ready: bool,
    seal: Option<mesh_network::protocol::HwSeal>,
    model: Option<String>,
    last_seen_secs: u64,
}

/// A direct connection that dropped is listed this long, then left out:
/// the address it had is no longer worth showing.
const DROPPED_PEER_SHOWN: std::time::Duration = std::time::Duration::from_secs(60);

#[tauri::command]
pub async fn peers_view(d: D<'_>) -> Result<PeersView, String> {
    let peers = d.state.peers.read().await.clone();
    let discovered = d.state.discovered_peers.lock().await.clone();
    let caps = d.state.known_peer_caps().await;
    let relayed = d.state.relayed_peers().await;
    let now = app_kernel::state::now_ms();
    let quality_of = {
        let chain = d.state.chain.read().await;
        let height = chain.head_height();
        let mut out = std::collections::HashMap::new();
        for pid in peers.values().filter_map(|p| p.peer_id) {
            out.insert(pid, chain.reputation.quality(&pid, height));
        }
        out
    };
    let (ai_ready_total, caps_known_total) =
        (caps.values().filter(|c| c.has_ai).count(), caps.len());
    Ok(PeersView {
        ai_ready_total,
        caps_known_total,
        relayed: relayed
            .iter()
            .map(|(pid, hello)| {
                let cap = mesh_network::protocol::PeerCapabilities::from_hello(hello);
                RelayedRow {
                    peer: hex::encode(&pid[..8]),
                    ai_ready: cap.has_ai,
                    seal: cap.hw_seal,
                    model: cap.model.map(|m| m.name),
                    last_seen_secs: now.saturating_sub(hello.sent_at_ms) / 1000,
                }
            })
            .collect(),
        connected: peers
            .iter()
            .filter(|(_, p)| p.connected || p.last_seen.elapsed() < DROPPED_PEER_SHOWN)
            .map(|(addr, p)| {
                let cap = p.peer_id.and_then(|pid| caps.get(&pid));
                PeerRow {
                    addr: addr.to_string(),
                    peer: p
                        .peer_id
                        .map(|pid| hex::encode(&pid[..8]))
                        .unwrap_or_default(),
                    connected: p.connected,
                    last_seen_secs: p.last_seen.elapsed().as_secs(),
                    ai_ready: cap.map(|c| c.has_ai),
                    seal: cap.and_then(|c| c.hw_seal),
                    model: cap.and_then(|c| c.model.as_ref().map(|m| m.name.clone())),
                    rating: p
                        .peer_id
                        .and_then(|pid| quality_of.get(&pid))
                        .filter(|q| q.evidence() > 0)
                        .map(|q| q.percent),
                    rating_votes: p
                        .peer_id
                        .and_then(|pid| quality_of.get(&pid))
                        .map(|q| q.evidence())
                        .unwrap_or(0),
                }
            })
            .collect(),
        discovered: discovered
            .keys()
            .filter(|addr| !peers.contains_key(addr))
            .count(),
    })
}

#[tauri::command]
pub async fn peer_connect(d: D<'_>, addr: String) -> Result<(), String> {
    let addr: std::net::SocketAddr = addr
        .trim()
        .parse()
        .map_err(|_| format!("Invalid address: {addr}"))?;
    let state = d.state.clone();
    tokio::spawn(async move {
        match state.dial(addr, None).await {
            Ok(pid) => {
                state
                    .log(format!("Connected to {addr} ({})", hex::encode(&pid[..4])))
                    .await
            }
            Err(e) => state.log(format!("Connection to {addr} failed: {e}")).await,
        }
    });
    Ok(())
}

// ============================ AI settings ============================

#[derive(Serialize)]
pub struct AiView {
    prompt_locked: bool,
    model_path: Option<String>,
    download: Option<(u64, u64)>,
    download_error: Option<String>,
    peers_with_model: Vec<(String, u64)>,
    engine_status: String,
    model_name: String,
    backend_url: String,
    download_url: String,
    tuning: Option<TuningView>,
    /// True when the sizes above were set by hand. False means they were
    /// derived from the model that is loaded, and will be derived again
    /// for the next one.
    tuning_manual: bool,
    local_prompt: String,
    thinking_enabled: bool,
    hw_available: bool,
}

/// The sizes inference will run with, whether set by hand or derived.
fn effective_tuning(
    cfg: &nepheral_config::NepheralConfig,
    model_cfg: &ai_engine::ModelConfig,
) -> Option<nepheral_config::InferenceTuningConfig> {
    if let Some(manual) = cfg.inference_tuning.clone() {
        return Some(manual);
    }
    let path = std::path::PathBuf::from(model_cfg.model_path.as_ref()?);
    let bytes = std::fs::metadata(&path).ok()?.len();
    let kv = ai_engine::gguf_probe::probe(&path).and_then(|i| i.kv_bytes_per_token);
    let ram_mb = ai_engine::hw_detect::detect().ram_mb;
    let t = ai_engine::hw_detect::tuning_for_model(bytes, kv, ram_mb);
    Some(nepheral_config::InferenceTuningConfig {
        n_ctx: t.n_ctx,
        n_ubatch: t.n_ubatch,
        max_new_tokens: t.max_new_tokens,
        estimated_ram_mb: t.estimated_ram_mb,
    })
}

#[derive(Serialize)]
pub struct TuningView {
    n_ctx: u32,
    n_ubatch: u32,
    max_new_tokens: u32,
    estimated_ram_mb: u64,
}

#[tauri::command]
pub async fn ai_view(d: D<'_>) -> Result<AiView, String> {
    // Shown only to whoever unlocked the wallet; the node uses it either way.
    let unlocked = d.state.is_unlocked().await;
    let local_prompt = if unlocked {
        d.node_store
            .lock()
            .expect("node store lock")
            .system_prompt
            .clone()
            .unwrap_or_default()
    } else {
        String::new()
    };
    let cfg = d.draft.lock().expect("draft lock").clone();
    let model_cfg = d.state.ai_model_config.read().await.clone();
    let peers_with_model = d
        .state
        .peers_with_model
        .read()
        .await
        .iter()
        .map(|(pid, (_hash, size))| (hex::encode(pid), *size))
        .collect();
    Ok(AiView {
        model_path: AppState::local_model_path().map(|p| p.display().to_string()),
        download: *d.state.model_download_progress.lock().await,
        download_error: d.state.model_download_error.lock().await.clone(),
        peers_with_model,
        engine_status: d.state.ai_engine_status(),
        model_name: model_cfg.model_name.clone(),
        backend_url: model_cfg.backend_url.clone(),
        download_url: cfg.model_download_url.clone(),
        tuning_manual: cfg.inference_tuning.is_some(),
        // What will actually be used: the manual override if there is
        // one, otherwise the sizes this model's own header implies.
        // Showing nothing when there was no override read as the tuner
        // having failed, when that is the normal state.
        tuning: effective_tuning(&cfg, &model_cfg)
            .as_ref()
            .map(|t| TuningView {
                n_ctx: t.n_ctx,
                n_ubatch: t.n_ubatch,
                max_new_tokens: t.max_new_tokens,
                estimated_ram_mb: t.estimated_ram_mb,
            }),
        local_prompt,
        prompt_locked: !unlocked,
        thinking_enabled: cfg.thinking_enabled,
        hw_available: cfg!(feature = "cpu"),
    })
}

/// Saves one setting, then refreshes the window's copy of the configuration.
///
/// The draft is read once when the window opens. Writing it back in full
/// reverts every field saved elsewhere since — which is how choosing a model
/// in one screen was undone by pressing Apply in another. Going through
/// `update` re-reads the file first, so a command only ever writes the fields
/// it is actually changing.
fn save_setting<F>(d: &D<'_>, mutate: F) -> Result<(), String>
where
    F: FnOnce(&mut nepheral_config::NepheralConfig),
{
    let saved = nepheral_config::update(mutate)?;
    *d.draft.lock().expect("draft lock") = saved;
    Ok(())
}

#[tauri::command]
pub async fn ai_apply(
    d: D<'_>,
    model_name: String,
    backend_url: String,
    download_url: String,
) -> Result<(), String> {
    save_setting(&d, |cfg| cfg.model_download_url = download_url)?;

    // Built from the configuration in force, not from defaults. Starting from
    // `Default` dropped `model_path`, so pressing Apply silently moved the
    // node off its installed model and onto the external backend — which is
    // how a screen for editing two fields came to change which engine ran.
    let config = {
        let current = d.state.ai_model_config.read().await;
        ai_engine::ModelConfig {
            model_name,
            backend_url,
            ..current.clone()
        }
    };

    // Awaited, so a rejected configuration is reported rather than discarded.
    d.state
        .update_ai_config(config)
        .await
        .map_err(|e| e.to_string())?;

    // `update_ai_config` reloads an engine that is already running. When none
    // is, Apply is how someone recovers from a model that would not load, so
    // it has to be able to start one.
    if d.state.ai_engine.read().await.is_none() {
        d.state
            .initialize_ai_engine()
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn detect_hardware() -> (HwView, TuningView, String) {
    let profile = ai_engine::hw_detect::detect();
    let gpu = match profile.gpu_kind {
        ai_engine::GpuKind::None => "None detected (CPU-only path)".to_string(),
        ai_engine::GpuKind::Vulkan => match profile.vram_mb {
            Some(v) => format!("Vulkan — {v} MB VRAM"),
            None => "Vulkan (integrated, shared RAM)".to_string(),
        },
        ai_engine::GpuKind::Cuda => format!("CUDA — {} MB VRAM", profile.vram_mb.unwrap_or(0)),
        ai_engine::GpuKind::Metal => "Metal (Apple)".to_string(),
    };
    let tuning = ai_engine::hw_detect::recommend_tuning(&profile, &profile.recommended_tier);
    (
        HwView {
            cpu_cores: profile.cpu_cores,
            ram_mb: profile.ram_mb,
            gpu,
            recommended_tier: profile.recommended_tier.model_name().to_string(),
            default_model_file: profile
                .recommended_tier
                .model_repo_file()
                .map(|(_, file)| file.to_string()),
            default_model_mb: profile.recommended_tier.approx_size_mb(),
        },
        TuningView {
            n_ctx: tuning.n_ctx,
            n_ubatch: tuning.n_ubatch,
            max_new_tokens: tuning.max_new_tokens,
            estimated_ram_mb: tuning.estimated_ram_mb,
        },
        profile
            .recommended_tier
            .model_name()
            .to_lowercase()
            .replace(' ', "_"),
    )
}

/// Sets the context sizes by hand, or clears them back to automatic.
///
/// Automatic sizing reads the model's own attention shape, which is right for
/// most cases but cannot know what else the machine is running. Passing
/// `null` removes the override so each model is sized for itself again.
#[tauri::command]
pub async fn ai_set_tuning(
    d: D<'_>,
    n_ctx: Option<u32>,
    n_ubatch: Option<u32>,
    max_new_tokens: Option<u32>,
) -> Result<(), String> {
    match (n_ctx, n_ubatch, max_new_tokens) {
        (Some(n_ctx), Some(n_ubatch), Some(max_new_tokens)) => {
            // Bounded to what llama.cpp will accept: a zero context or a
            // reply longer than the window it must fit in fails at load, and
            // reporting that as a broken model would be misleading.
            if !(256..=1_048_576).contains(&n_ctx) {
                return Err("context must be between 256 and 1048576 tokens".into());
            }
            if !(16..=8192).contains(&n_ubatch) {
                return Err("batch must be between 16 and 8192".into());
            }
            if max_new_tokens < 32 || max_new_tokens >= n_ctx {
                return Err("the reply length must be at least 32 and below the context".into());
            }
            save_setting(&d, |cfg| {
                cfg.inference_tuning = Some(nepheral_config::InferenceTuningConfig {
                    n_ctx,
                    n_ubatch,
                    max_new_tokens,
                    estimated_ram_mb: 0,
                })
            })?;
        }
        _ => save_setting(&d, |cfg| cfg.inference_tuning = None)?,
    }

    // Applied now rather than at the next restart, so the effect is visible.
    d.state
        .initialize_ai_engine()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn ai_redetect(d: D<'_>) -> Result<Option<TuningView>, String> {
    let (_hw, view, _tier) = detect_hardware();
    save_setting(&d, |cfg| {
        cfg.inference_tuning = Some(nepheral_config::InferenceTuningConfig {
            n_ctx: view.n_ctx,
            n_ubatch: view.n_ubatch,
            max_new_tokens: view.max_new_tokens,
            estimated_ram_mb: view.estimated_ram_mb,
        })
    })?;
    let state = d.state.clone();
    tokio::spawn(async move {
        let _ = state.initialize_ai_engine().await;
    });
    Ok(Some(view))
}

#[tauri::command]
pub async fn ai_save_prompt(
    d: D<'_>,
    prompt: String,
    thinking_enabled: bool,
) -> Result<(), String> {
    d.state.require_unlocked().await?;
    let trimmed = prompt.trim();
    let prompt = (!trimmed.is_empty()).then(|| trimmed.to_string());
    {
        let mut store = d.node_store.lock().expect("node store lock");
        store.system_prompt = prompt.clone();
        store.save(&nepheral_config::nepheral_dir())?;
    }
    save_setting(&d, |cfg| {
        cfg.local_system_prompt = None;
        cfg.thinking_enabled = thinking_enabled;
    })?;
    *d.state.local_system_prompt.write().await = prompt;
    let state = d.state.clone();
    tokio::spawn(async move {
        state.refresh_system_prompt().await;
        state.set_reasoning_allowed(thinking_enabled).await;
    });
    Ok(())
}

#[tauri::command]
pub async fn ai_download_from_peer(d: D<'_>, addr: String) -> Result<(), String> {
    let mut peer = [0u8; 32];
    hex::decode_to_slice(addr.trim(), &mut peer).map_err(|_| "bad peer id".to_string())?;
    d.state.request_trusted_model_from_peer(peer).await
}

// ============================ network / fragments ============================

#[derive(Serialize)]
pub struct NetworkView {
    local_addr: String,
    network: String,
    uptime: String,
    active_peers: usize,
    ai_engine: String,
    bootstrap: Vec<String>,
    custody: usize,
    pending_queries: usize,
    log: Vec<String>,
}

#[tauri::command]
pub async fn network_view(d: D<'_>) -> Result<NetworkView, String> {
    let s = &d.state;
    Ok(NetworkView {
        local_addr: s.local_addr.to_string(),
        network: s.network.name().to_string(),
        uptime: s.format_uptime(),
        active_peers: s.live_connections().await.len(),
        ai_engine: s.ai_engine_status(),
        bootstrap: s.bootstrap_introducers.read().await.clone(),
        custody: s.custody.read().await.len(),
        pending_queries: s.pending_queries.read().await.len(),
        log: s
            .activity_log
            .lock()
            .await
            .iter()
            .rev()
            .take(100)
            .cloned()
            .collect(),
    })
}

#[tauri::command]
pub async fn network_set(name: String) -> Result<String, String> {
    let network = core_crypto::network::Network::parse(&name)
        .ok_or_else(|| format!("unknown network '{name}'"))?;
    nepheral_config::update(|cfg| cfg.network = network.name().to_string())?;
    Ok(format!(
        "Network set to {}. Restart the app to switch; each network keeps its own chain.",
        network.name()
    ))
}

#[tauri::command]
pub async fn bootstrap_add(d: D<'_>, addr: String) -> Result<(), String> {
    let addr = addr.trim().to_string();
    if addr.is_empty() {
        return Ok(());
    }
    let state = d.state.clone();
    tokio::spawn(async move {
        state.add_bootstrap_introducer(addr.clone()).await;
        let _ = state.query_bootstrap_introducer(&addr).await;
    });
    Ok(())
}

// ============================ leaderboard ============================

#[derive(Serialize)]
pub struct LeaderboardView {
    my: MyNodeCard,
    bands: (u64, u64, u64),
    rows: Vec<LbRow>,
}

#[derive(Serialize)]
pub struct MyNodeCard {
    aporte: u64,
    aporte_pending: u64,
    consumo: u64,
    consumo_pending: u64,
    health: f64,
    wins: u64,
    wins_pending: u64,
    credits: u64,
    maturity: u64,
    band: &'static str,
    score: f64,
    uptime_secs: u64,
    /// What this node seals its own key with, for the badge on its row.
    seal: Option<mesh_network::protocol::HwSeal>,
}

#[derive(Serialize)]
pub struct LbRow {
    peer: String,
    is_self: bool,
    has_ai: bool,
    seal: Option<mesh_network::protocol::HwSeal>,
    model: String,
    maturity: u64,
    band: &'static str,
    aporte: u64,
    health: f64,
    wins: u64,
    credits: u64,
    uptime_secs: u64,
    /// Answer quality from the ratings pallet: the percentage the lottery
    /// weights by, and the raw counts behind it. Without these the Useful /
    /// Not useful buttons steer routing invisibly, so nobody can see that
    /// pressing them did anything.
    quality: u64,
    likes: u64,
    dislikes: u64,
}

#[tauri::command]
pub async fn leaderboard_view(d: D<'_>) -> Result<LeaderboardView, String> {
    let s = &d.state;
    let me = *s.my_peer_id.read().await;
    let account = s.my_account().await;
    let my_stats = s.my_stats.read().await.clone();
    let peer_stats = s.peer_stats.read().await.clone();
    let caps = s.peer_capabilities.read().await.clone();
    // Through relays too: the seal is declared in presence as in HELLO.
    let known = s.known_peer_caps().await;
    let relay_pending = 0u64;
    let my_model_name = s.ai_model_config.read().await.model_name.clone();
    let chain = s.chain.read().await;
    let head_height = chain.head_height();

    let net_maturity: Vec<u64> = chain
        .contributions
        .share_counts
        .keys()
        .chain(chain.contributions.lottery_dimension_by_peer.keys())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .map(|pid| chain.contributions.maturity_of(pid))
        .filter(|v| *v > 0)
        .collect();
    let (p50, p90, p99) = (
        percentile(&net_maturity, 0.50),
        percentile(&net_maturity, 0.90),
        percentile(&net_maturity, 0.99),
    );

    let my_aporte = chain.contribution_of(&me);
    let my_consumo = chain.consumed_of(&me);
    let my_wins = chain.wins_of(&me);
    let my_maturity = chain.contributions.maturity_of(&me);
    let my = MyNodeCard {
        aporte: my_aporte,
        aporte_pending: relay_pending,
        consumo: my_consumo,
        consumo_pending: my_stats.shares_sent.saturating_sub(my_consumo),
        health: my_aporte as f64 / (my_consumo.max(1) as f64),
        wins: my_wins,
        wins_pending: my_stats.lotteries_won.saturating_sub(my_wins),
        credits: chain.balance_of(&account),
        maturity: my_maturity,
        band: maturity_tier(my_maturity, p50, p90, p99),
        score: my_stats.performance_score,
        uptime_secs: my_stats.uptime_seconds,
        seal: *s.hw_seal.read().expect("hw seal"),
    };

    let mut pids: std::collections::HashSet<PeerId> = std::collections::HashSet::new();
    pids.insert(me);
    pids.extend(peer_stats.keys().copied());
    pids.extend(chain.contributions.share_counts.keys().copied());
    pids.extend(chain.contributions.shares_consumed.keys().copied());
    pids.extend(chain.contributions.wins_by_peer.keys().copied());

    let model_name_for = |pid: &PeerId, is_self: bool| -> String {
        if is_self && !my_model_name.is_empty() {
            return my_model_name.clone();
        }
        match caps.get(pid).and_then(|c| c.model.as_ref()) {
            Some(m) if !m.name.is_empty() => m.name.clone(),
            _ => "—".to_string(),
        }
    };

    let mut rows: Vec<LbRow> = pids
        .into_iter()
        .map(|pid| {
            let is_self = pid == me;
            let stats = if is_self {
                Some(&my_stats)
            } else {
                peer_stats.get(&pid)
            };
            let aporte = chain.contribution_of(&pid);
            let consumo = chain.consumed_of(&pid);
            let maturity = chain.contributions.maturity_of(&pid);
            let q = chain.reputation.quality(&pid, head_height);
            LbRow {
                peer: hex::encode(&pid[..6]),
                is_self,
                has_ai: if is_self {
                    AppState::local_model_path().is_some()
                } else {
                    caps.get(&pid).map(|c| c.has_ai).unwrap_or(false)
                },
                seal: if is_self {
                    *s.hw_seal.read().expect("hw seal")
                } else {
                    known.get(&pid).and_then(|c| c.hw_seal)
                },
                model: model_name_for(&pid, is_self),
                maturity,
                band: maturity_tier(maturity, p50, p90, p99),
                aporte,
                health: aporte as f64 / (consumo.max(1) as f64),
                wins: chain.wins_of(&pid),
                credits: chain.balance_of(&pid),
                uptime_secs: stats.map(|s| s.uptime_seconds).unwrap_or(0),
                quality: q.percent,
                likes: q.likes,
                dislikes: q.dislikes,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.maturity
            .cmp(&a.maturity)
            .then(b.wins.cmp(&a.wins))
            .then(b.uptime_secs.cmp(&a.uptime_secs))
    });

    Ok(LeaderboardView {
        my,
        bands: (p50, p90, p99),
        rows,
    })
}

// ============================ wallet ============================

#[derive(Serialize)]
pub struct WalletFull {
    /// The wallet's address: where credits and Litecoin purchases go.
    peer_id: String,
    node_id: String,
    delegation: app_kernel::wallet::Delegation,
    balance: u64,
    shares_avail: u64,
    lotteries_avail: u64,
    aporte: u64,
    aporte_pending: u64,
    consumo: u64,
    consumo_pending: u64,
    wins: u64,
    wins_pending: u64,
    rate: u64,
    redeemable_credits: u64,
    priority_cost: u64,
    direct_sale_wallet: Option<String>,
    /// False while the payment watcher cannot credit payments. The purchase
    /// instructions are withheld in that state: a buyer who cannot be credited
    /// yet should not be handed an address to send money to.
    sale_healthy: bool,
    sale_health_reason: Option<String>,
    credits_per_ltc: u64,
    sessions: Vec<SessionEntry>,
    history: Vec<HistoryRow>,
}

#[derive(Serialize)]
pub struct SessionEntry {
    token: String,
    channel: Option<String>,
    inferences_remaining: u8,
    expires_in_min: u64,
}

#[derive(Serialize)]
pub struct HistoryRow {
    height: u64,
    kind: &'static str,
    credit: String,
    color: &'static str,
    detail: String,
    filter: &'static str, // sent | received | sessions | mints | other
}

#[tauri::command]
pub async fn wallet_view(d: D<'_>) -> Result<WalletFull, String> {
    use core_crypto::pallets::economy::DEFAULT_SHARES_PER_CREDIT;
    use core_crypto::pallets::nai::{NAI_CREDITS_PER_LTC, PRIORITY_SESSION_COST};

    let s = &d.state;
    // The node earns; the wallet holds. Before the split they were one key.
    let me = *s.my_peer_id.read().await;
    let account = s.my_account().await;
    let delegation = s.delegation_status().await;
    let my_stats = s.my_stats.read().await.clone();
    let relay_pending = 0u64;
    let now = now_ms();
    let chain = s.chain.read().await;

    let (shares_avail, lotteries_avail) = chain.redeemable(&me);
    let aporte = chain.shares_of(&me);
    let consumo = chain.consumed_of(&me);
    let wins = chain.wins_of(&me);
    let rate = if chain.economy.current_shares_per_credit > 0 {
        chain.economy.current_shares_per_credit
    } else {
        DEFAULT_SHARES_PER_CREDIT
    };
    let total_equivs = shares_avail.saturating_add(lotteries_avail);

    let sessions = chain
        .nai
        .sessions
        .iter()
        .filter(|(_, se)| se.owner == account || se.owner == me)
        .map(|(token, se)| SessionEntry {
            token: hex::encode(token),
            channel: se.channel.clone(),
            inferences_remaining: se.inferences_remaining,
            expires_in_min: se.expires_at_ms.saturating_sub(now) / 60_000,
        })
        .collect();

    let mine = [account, me];
    let mut rows = chain.history_of(&account);
    if account != me {
        let mut seen: std::collections::HashSet<[u8; 32]> =
            rows.iter().map(|(_, e)| e.hash_for_signing()).collect();
        for row in chain.history_of(&me) {
            if seen.insert(row.1.hash_for_signing()) {
                rows.push(row);
            }
        }
        rows.sort_by_key(|(h, _)| *h);
    }
    let history = rows
        .into_iter()
        .rev()
        .take(30)
        .map(|(height, ext)| describe_extrinsic(height, ext, &mine))
        .collect();

    Ok(WalletFull {
        peer_id: hex::encode(account),
        node_id: hex::encode(me),
        delegation,
        balance: chain.balance_of(&account),
        shares_avail,
        lotteries_avail,
        aporte,
        aporte_pending: relay_pending,
        consumo,
        consumo_pending: my_stats.shares_sent.saturating_sub(consumo),
        wins,
        wins_pending: my_stats.lotteries_won.saturating_sub(wins),
        rate,
        redeemable_credits: total_equivs / rate.max(1),
        priority_cost: PRIORITY_SESSION_COST,
        direct_sale_wallet: chain.nai.direct_sale_wallet.clone(),
        sale_healthy: chain.nai.direct_sale_healthy,
        sale_health_reason: chain.nai.direct_sale_health_reason.clone(),
        credits_per_ltc: NAI_CREDITS_PER_LTC,
        sessions,
        history,
    })
}

fn describe_extrinsic(height: u64, ext: &core_crypto::Extrinsic, mine: &[PeerId]) -> HistoryRow {
    let is_mine = |p: &PeerId| mine.contains(p);
    use core_crypto::pallets::balances::BalancesCall;
    use core_crypto::pallets::contributions::{ContributionsCall, SHARES_PER_CREDIT};
    use core_crypto::pallets::nai::{NaiCall, PRIORITY_SESSION_COST};

    let short = |p: &PeerId| hex::encode(&p[..6]);
    let row = |kind: &'static str,
               credit: String,
               color: &'static str,
               detail: String,
               filter: &'static str| HistoryRow {
        height,
        kind,
        credit,
        color,
        detail,
        filter,
    };

    let Ok(decoded) = DecodedCall::decode(ext.pallet_idx, &ext.args) else {
        return row(
            "EXT",
            String::new(),
            "#8c8c96",
            format!("signer:{}", short(&ext.signer)),
            "other",
        );
    };

    match decoded {
        DecodedCall::Identity(core_crypto::pallets::identity::IdentityCall::Delegate {
            node,
            ..
        }) => row(
            "NODE",
            String::new(),
            "#78b4c8",
            format!("node {} works for {}", short(&node), short(&ext.signer)),
            "other",
        ),
        DecodedCall::Identity(core_crypto::pallets::identity::IdentityCall::Revoke { node }) => {
            row(
                "NODE",
                String::new(),
                "#8c8c96",
                format!("node {} released", short(&node)),
                "other",
            )
        }
        DecodedCall::Balances(BalancesCall::Burn { amount }) => row(
            "BURN",
            format!("-{amount}"),
            "#e68c50",
            format!("burned by {}", short(&ext.signer)),
            "sent",
        ),
        DecodedCall::Balances(BalancesCall::Transfer { to, amount }) => {
            if is_mine(&to) {
                row(
                    "RECV",
                    format!("+{amount}"),
                    "#64aaff",
                    format!("← {}", short(&ext.signer)),
                    "received",
                )
            } else if is_mine(&ext.signer) {
                row(
                    "SEND",
                    format!("-{amount}"),
                    "#e68c50",
                    format!("→ {}", short(&to)),
                    "sent",
                )
            } else {
                row(
                    "TRANSFER",
                    format!("{amount}"),
                    "#8c8c96",
                    format!("{} → {}", short(&ext.signer), short(&to)),
                    "other",
                )
            }
        }
        DecodedCall::Contributions(ContributionsCall::Redeem {
            receiver,
            shares,
            lottery_dimension,
        }) => row(
            "REDEEM",
            format!("+{}", (shares + lottery_dimension) / SHARES_PER_CREDIT),
            "#78c88c",
            format!("→ {} ({shares}s/{lottery_dimension}dim)", short(&receiver)),
            "other",
        ),
        DecodedCall::Contributions(ContributionsCall::RecordShare { peer }) => row(
            "SHARE",
            "+1".into(),
            "#78b4c8",
            format!("peer {}", short(&peer)),
            "other",
        ),
        DecodedCall::Contributions(ContributionsCall::SubmitReceipt { receipt }) => {
            let c = &receipt.claim;
            if is_mine(&c.responder_pid) {
                row(
                    "SERVED",
                    format!("+{}dim", c.dimension()),
                    "#78c88c",
                    format!("answered {}", short(&c.sender_pid)),
                    "other",
                )
            } else {
                row(
                    "ASKED",
                    String::new(),
                    "#dc8c50",
                    format!("answered by {}", short(&c.responder_pid)),
                    "other",
                )
            }
        }
        DecodedCall::Contributions(ContributionsCall::SubmitReceiptWithCustody {
            receipt,
            custody,
        }) => {
            let c = &receipt.claim;
            if is_mine(&c.responder_pid) {
                row(
                    "SERVED",
                    format!("+{}dim", c.dimension()),
                    "#78c88c",
                    format!("answered {}", short(&c.sender_pid)),
                    "other",
                )
            } else if is_mine(&c.sender_pid) {
                row(
                    "ASKED",
                    String::new(),
                    "#dc8c50",
                    format!("answered by {}", short(&c.responder_pid)),
                    "other",
                )
            } else if custody.custodians.iter().any(is_mine) {
                row(
                    "CUSTODY",
                    format!(
                        "+1/{}",
                        core_crypto::pallets::contributions::CUSTODY_PER_SHARE
                    ),
                    "#78b4c8",
                    format!("held a fragment for {}", short(&c.sender_pid)),
                    "other",
                )
            } else {
                row(
                    "RECEIPT",
                    String::new(),
                    "#8a9099",
                    format!(
                        "{} answered {}",
                        short(&c.responder_pid),
                        short(&c.sender_pid)
                    ),
                    "other",
                )
            }
        }
        DecodedCall::Contributions(ContributionsCall::RecordConsumption { peer }) => row(
            "USE",
            "-1".into(),
            "#dc8c50",
            format!("query by {}", short(&peer)),
            "other",
        ),
        DecodedCall::Nai(NaiCall::DirectSale {
            receiver, credits, ..
        }) => row(
            "LTC",
            format!("+{credits}"),
            "#dcc850",
            format!("→ {}", short(&receiver)),
            "mints",
        ),
        DecodedCall::Nai(NaiCall::Spend { .. }) => row(
            "SPEND",
            format!("-{PRIORITY_SESSION_COST}"),
            "#dc6464",
            format!("signer:{}", short(&ext.signer)),
            "sessions",
        ),
        DecodedCall::Nai(NaiCall::UseSession { session_token }) => row(
            "USE",
            String::new(),
            "#c8be8c",
            format!("session {}…", hex::encode(&session_token[..4])),
            "sessions",
        ),
        DecodedCall::Nai(NaiCall::OpenChannelSession { channel, .. }) => row(
            "OPEN",
            String::new(),
            "#dcb450",
            format!("channel: {channel} (signer {})", short(&ext.signer)),
            "sessions",
        ),
        DecodedCall::Nai(NaiCall::SetDirectSaleWallet { address }) => row(
            "WALLET",
            String::new(),
            "#b4b4dc",
            address
                .map(|a| format!("set: {a}"))
                .unwrap_or_else(|| "cleared".into()),
            "other",
        ),
        DecodedCall::Nai(NaiCall::SetChannelSessionCost { channel, cost }) => row(
            "CHCOST",
            format!("={cost}"),
            "#b4c8dc",
            format!("channel: {channel}"),
            "other",
        ),
        DecodedCall::Nai(NaiCall::SetDailyMintCap { cap }) => row(
            "MINTCAP",
            format!("={cap}"),
            "#dcb4b4",
            "daily mint cap updated".into(),
            "other",
        ),
        DecodedCall::Nai(NaiCall::SetSaleHealth { healthy, reason }) => row(
            "SALES",
            String::new(),
            if healthy { "#b4dcc8" } else { "#dcb4b4" },
            if healthy {
                "purchases resumed".to_string()
            } else {
                reason.unwrap_or_else(|| "purchases paused".into())
            },
            "other",
        ),
        DecodedCall::Nai(NaiCall::SetSaleOracle { pubkey }) => row(
            "ORACLE",
            String::new(),
            "#dcb4b4",
            pubkey
                .map(|pk| format!("sale oracle: {}", short(&pk)))
                .unwrap_or_else(|| "sale oracle revoked".into()),
            "other",
        ),
        DecodedCall::Authority(call) => {
            use core_crypto::pallets::authority::AuthorityCall;
            let detail = match call {
                AuthorityCall::Add {
                    pubkey, channels, ..
                } => {
                    format!("{}  channels: {}", short(&pubkey), channels.join(","))
                }
                AuthorityCall::Remove { pubkey } => format!("{}  removed", short(&pubkey)),
                AuthorityCall::SetChannels { pubkey, channels } => {
                    format!("{}  channels: {}", short(&pubkey), channels.join(","))
                }
                AuthorityCall::RotateKey {
                    old_pubkey,
                    new_pubkey,
                } => {
                    format!("{}  → {}", short(&old_pubkey), short(&new_pubkey))
                }
                AuthorityCall::Dissolve => "founder authority dissolved".to_string(),
            };
            row("AUTH", String::new(), "#dcb4dc", detail, "other")
        }
        DecodedCall::Finality(call) => {
            use core_crypto::finality::FinalityCall;
            let detail = match call {
                FinalityCall::Finalize { justification } => format!(
                    "finalized block {} ({} votes)",
                    justification.height,
                    justification.votes.len()
                ),
                FinalityCall::ReportEquivocation { proof } => format!(
                    "validator {} reported for signing two blocks at {}",
                    short(&proof.first.voter),
                    proof.first.height
                ),
            };
            row("FINAL", String::new(), "#b4b4dc", detail, "other")
        }
        DecodedCall::Upgrade(call) => {
            use core_crypto::pallets::upgrade::UpgradeCall;
            let detail = match call {
                UpgradeCall::Schedule {
                    version,
                    activate_at_ms,
                    ..
                } => format!("chain rules v{version} scheduled for {activate_at_ms} ms"),
                UpgradeCall::Cancel => "scheduled chain upgrade cancelled".to_string(),
            };
            row("UPGRADE", String::new(), "#dcc8b4", detail, "other")
        }
        DecodedCall::Reputation(call) => {
            use core_crypto::pallets::reputation::{ReputationCall, Verdict};
            let ReputationCall::Rate { verdict, .. } = call;
            let detail = match verdict {
                Verdict::Like => "rated an answer: like".to_string(),
                Verdict::Dislike { reason, .. } => {
                    format!("rated an answer: dislike ({})", reason.as_str())
                }
            };
            row("RATE", String::new(), "#c8dcb4", detail, "other")
        }
        DecodedCall::Governance(call) => {
            use core_crypto::pallets::governance::GovernanceCall;
            let detail = match call {
                GovernanceCall::Propose { action } => {
                    format!("proposal: {}", describe_gov_action(&action))
                }
                GovernanceCall::Vote { id, aye } => {
                    format!("vote {} on #{id}", if aye { "yes" } else { "no" })
                }
                GovernanceCall::Enact { id } => format!("enact #{id}"),
            };
            row("GOV", String::new(), "#b4c8f0", detail, "other")
        }
        DecodedCall::Market(call) => {
            use core_crypto::pallets::market::MarketCall;
            let (tag, amount, detail) = match call {
                MarketCall::PostAsk {
                    credits, ask_sats, ..
                } => (
                    "ASK",
                    format!("-{credits}"),
                    format!("listed for {ask_sats} sats"),
                ),
                MarketCall::CancelAsk { id } => {
                    ("ASK-", String::new(), format!("listing #{id} withdrawn"))
                }
                MarketCall::PostBid { credits, bid_sats } => (
                    "BID",
                    String::new(),
                    format!("wants {credits} for {bid_sats} sats"),
                ),
                MarketCall::CancelBid { id } => {
                    ("BID-", String::new(), format!("bid #{id} withdrawn"))
                }
                MarketCall::ClaimFill { ask_id, buyer, .. } => (
                    "CLAIM",
                    String::new(),
                    format!("listing #{ask_id} claimed for {}", short(&buyer)),
                ),
                MarketCall::FailClaim {
                    payment_ref,
                    permanent,
                } => (
                    "RETRY",
                    String::new(),
                    if permanent {
                        format!(
                            "claim {} dropped",
                            &payment_ref[..payment_ref.len().min(12)]
                        )
                    } else {
                        format!(
                            "claim {} not confirmed yet",
                            &payment_ref[..payment_ref.len().min(12)]
                        )
                    },
                ),
                MarketCall::Settle { id, buyer, .. } => (
                    "FILL",
                    String::new(),
                    format!("listing #{id} → {}", short(&buyer)),
                ),
            };
            row(tag, amount, "#b4dcc8", detail, "other")
        }
        DecodedCall::Economy(_) => row(
            "EMIT",
            String::new(),
            "#c8dcb4",
            "emission target updated".into(),
            "other",
        ),
    }
}

#[tauri::command]
pub async fn redeem(d: D<'_>, shares: Option<u64>, lotteries: Option<u64>) -> Result<(), String> {
    d.state.require_unlocked().await?;
    let state = d.state.clone();
    tokio::spawn(async move {
        let (sh, lot) = match (shares, lotteries) {
            (Some(s), Some(l)) => (s, l),
            _ => {
                let me = *state.my_peer_id.read().await;
                state.chain.read().await.redeemable(&me)
            }
        };
        state.request_redeem(sh, lot).await;
    });
    Ok(())
}

#[tauri::command]
pub async fn transfer(d: D<'_>, to: String, credits: u64) -> Result<(), String> {
    let trimmed = to.trim();
    if trimmed.len() != 64 {
        return Err("recipient must be 64 hex chars".into());
    }
    let mut receiver = [0u8; 32];
    hex::decode_to_slice(trimmed, &mut receiver).map_err(|_| "recipient is not valid hex")?;
    if credits == 0 {
        return Err("amount must be > 0".into());
    }
    d.state.transfer_credits(receiver, credits).await
}

#[derive(Serialize)]
pub struct MarketRow {
    id: u64,
    peer: String,
    peer_short: String,
    credits: u64,
    sats: u64,
    /// Litoshi per credit, so rows can be compared at a glance.
    sats_per_credit: u64,
    /// Present on asks only: where a buyer sends the Litecoin.
    ltc_address: Option<String>,
    mine: bool,
    /// What the chain already knows about this counterparty. Every figure is
    /// derived from chain state, never self-reported, so none of it can be
    /// claimed by an account that has not earned it.
    trades: u64,
    days_active: u64,
    /// A buyer's claim on this listing is waiting for the sale oracle.
    claim_pending: bool,
    /// ...and it is this node's own.
    claim_mine: bool,
    shares: u64,
    wins: u64,
}

/// How far an account is from being allowed to trade, or nothing if it is.
#[derive(Serialize)]
pub struct StandingView {
    eligible: bool,
    reason: Option<String>,
    days_active: u64,
    days_required: u64,
    shares: u64,
    shares_required: u64,
    wins: u64,
    wins_required: u64,
}

#[derive(Serialize)]
pub struct MarketView {
    asks: Vec<MarketRow>,
    bids: Vec<MarketRow>,
    /// Credits of this node currently locked in its own open asks.
    my_escrow: u64,
    balance: u64,
    peer_id: String,
    max_open: usize,
    standing: StandingView,
}

#[tauri::command]
pub async fn market_view(d: D<'_>) -> Result<MarketView, String> {
    let s = &d.state;
    let me = s.my_account().await;
    let chain = s.chain.read().await;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // Completed trades per counterparty, counted from settled fills.
    let mut trades: std::collections::HashMap<PeerId, u64> = std::collections::HashMap::new();
    for fill in chain.market.fills.values() {
        *trades.entry(fill.seller).or_insert(0) += 1;
        *trades.entry(fill.buyer).or_insert(0) += 1;
    }

    let reputation = |peer: &PeerId| -> (u64, u64, u64, u64) {
        (
            trades.get(peer).copied().unwrap_or(0),
            chain.contributions.days_active(peer, now_ms),
            chain.contributions.shares_of(peer),
            chain.contributions.wins_of(peer),
        )
    };

    let mut asks: Vec<MarketRow> = chain
        .market
        .asks
        .values()
        .map(|a| {
            let (t, d, sh, w) = reputation(&a.seller);
            let claim = chain.market.claims.values().find(|c| c.ask_id == a.id);
            MarketRow {
                claim_pending: claim.is_some(),
                claim_mine: claim.is_some_and(|c| c.buyer == me),
                id: a.id,
                peer: hex::encode(a.seller),
                peer_short: hex::encode(&a.seller[..8]),
                credits: a.credits,
                sats: a.ask_sats,
                sats_per_credit: a.ask_sats / a.credits.max(1),
                ltc_address: Some(a.ltc_address.clone()),
                mine: a.seller == me,
                trades: t,
                days_active: d,
                shares: sh,
                wins: w,
            }
        })
        .collect();
    // An order book, not a feed: cheapest offer first, and a stable tiebreak
    // so two listings at the same price do not swap places between refreshes.
    asks.sort_by_key(|r| (r.sats_per_credit, r.id));

    let mut bids: Vec<MarketRow> = chain
        .market
        .bids
        .values()
        .map(|b| {
            let (t, d, sh, w) = reputation(&b.buyer);
            MarketRow {
                claim_pending: false,
                claim_mine: false,
                id: b.id,
                peer: hex::encode(b.buyer),
                peer_short: hex::encode(&b.buyer[..8]),
                credits: b.credits,
                sats: b.bid_sats,
                sats_per_credit: b.bid_sats / b.credits.max(1),
                ltc_address: None,
                mine: b.buyer == me,
                trades: t,
                days_active: d,
                shares: sh,
                wins: w,
            }
        })
        .collect();
    // The other side of the book: the best offer to a seller is the highest.
    bids.sort_by_key(|r| (std::cmp::Reverse(r.sats_per_credit), r.id));

    Ok(MarketView {
        my_escrow: chain
            .market
            .asks
            .values()
            .filter(|a| a.seller == me)
            .map(|a| a.credits)
            .sum(),
        asks,
        bids,
        balance: chain.balance_of(&me),
        peer_id: hex::encode(me),
        max_open: core_crypto::pallets::market::MAX_OPEN_PER_ACCOUNT,
        standing: {
            use core_crypto::pallets::market::{
                check_eligible_group, MIN_DAYS_ACTIVE, MIN_SHARES, MIN_WINS,
            };
            // The wallet's standing counts the nodes working for it.
            let group = chain.identity.group_of(&me);
            let c = &chain.contributions;
            let verdict = check_eligible_group(c, &group, now_ms);
            StandingView {
                eligible: verdict.is_ok(),
                reason: verdict.err().map(|e| e.to_string()),
                days_active: group
                    .iter()
                    .map(|p| c.days_active(p, now_ms))
                    .max()
                    .unwrap_or(0),
                days_required: MIN_DAYS_ACTIVE,
                shares: group.iter().map(|p| c.maturity_of(p)).sum(),
                shares_required: MIN_SHARES,
                wins: group.iter().map(|p| c.wins_of(p)).sum(),
                wins_required: MIN_WINS,
            }
        },
    })
}

#[tauri::command]
pub async fn market_post_ask(
    d: D<'_>,
    credits: u64,
    sats: u64,
    ltc_address: String,
) -> Result<(), String> {
    use core_crypto::pallets::market::{MAX_LTC_ADDRESS_LEN, MIN_CREDITS, MIN_SATS};

    let addr = ltc_address.trim().to_string();
    if credits < MIN_CREDITS {
        return Err("amount must be > 0".into());
    }
    if sats < MIN_SATS {
        return Err("price must be > 0".into());
    }
    if addr.is_empty() || addr.len() > MAX_LTC_ADDRESS_LEN {
        return Err("a Litecoin address to be paid at is required".into());
    }
    if is_private_ltc_address(&addr) {
        return Err(
            "use an ordinary Litecoin address: nothing paid to a private (MWEB) address can be \
             seen, so no buyer's payment could ever be confirmed"
                .into(),
        );
    }
    d.state.market_post_ask(credits, sats, addr).await
}

#[tauri::command]
pub async fn market_post_bid(d: D<'_>, credits: u64, sats: u64) -> Result<(), String> {
    use core_crypto::pallets::market::{MIN_CREDITS, MIN_SATS};

    if credits < MIN_CREDITS {
        return Err("amount must be > 0".into());
    }
    if sats < MIN_SATS {
        return Err("price must be > 0".into());
    }
    d.state.market_post_bid(credits, sats).await
}

/// An address inside Litecoin's private extension (MWEB). What is paid to one
/// is invisible to every explorer, so a listing there could never settle.
fn is_private_ltc_address(address: &str) -> bool {
    let a = address.trim().to_ascii_lowercase();
    a.starts_with("ltcmweb1") || a.starts_with("tmweb1")
}

/// The OP_RETURN payload a buyer attaches to pay for listing `ask_id`: this
/// wallet and the listing, so the payment cannot be credited to anyone else.
#[tauri::command]
pub async fn market_fill_memo(d: D<'_>, ask_id: u64) -> Result<String, String> {
    let me = d.state.my_account().await;
    Ok(hex::encode(core_crypto::payment_memo::encode_fill(
        &me, ask_id,
    )))
}

#[tauri::command]
pub async fn market_claim(d: D<'_>, ask_id: u64, payment_ref: String) -> Result<(), String> {
    d.state.market_claim_fill(ask_id, &payment_ref).await
}

#[tauri::command]
pub async fn market_cancel(d: D<'_>, id: u64, is_ask: bool) -> Result<(), String> {
    d.state.market_cancel(id, is_ask).await
}

fn describe_gov_action(action: &core_crypto::pallets::governance::GovAction) -> String {
    use core_crypto::pallets::governance::GovAction;
    let short = |p: &[u8; 32]| hex::encode(&p[..8]);
    match action {
        GovAction::SetSaleOracle { pubkey } => match pubkey {
            Some(pk) => format!("set sale oracle to {}", short(pk)),
            None => "revoke the sale oracle".into(),
        },
        GovAction::SetDirectSaleWallet { address } => match address {
            Some(a) => format!("set direct sale wallet to {a}"),
            None => "clear the direct sale wallet".into(),
        },
        GovAction::SetDailyMintCap { cap } => format!("set daily mint cap to {cap}"),
        GovAction::SetChannelSessionCost { channel, cost } => {
            format!("set {channel} session cost to {cost}")
        }
        GovAction::SetEmissionTarget {
            target_credits_per_hour,
        } => format!("set emission target to {target_credits_per_hour} credits/hour"),
        GovAction::AddPartner {
            pubkey, channels, ..
        } => format!("add partner {} ({})", short(pubkey), channels.join(",")),
        GovAction::RemovePartner { pubkey } => format!("remove partner {}", short(pubkey)),
        GovAction::ScheduleUpgrade {
            version,
            activate_at_ms,
            ..
        } => format!("upgrade the chain rules to v{version} at {activate_at_ms} ms"),
        GovAction::CancelUpgrade => "cancel the scheduled chain upgrade".into(),
    }
}

#[derive(Serialize)]
pub struct ProposalView {
    id: u64,
    proposer: String,
    summary: String,
    membership: bool,
    status: String,
    created_at: u64,
    voting_closes_at: u64,
    enactable_at: u64,
    aye: u64,
    nay: u64,
    electorate: u64,
    passes: bool,
    my_vote: Option<bool>,
}

#[derive(Serialize)]
pub struct GovernanceView {
    /// Current value of each lever a proposal can move, shown beside it.
    emission_target: u64,
    daily_mint_cap: u64,
    session_costs: Vec<(String, u64)>,
    head: u64,
    my_weight: u64,
    founder: Option<String>,
    dissolved_at: Option<u64>,
    partners: Vec<String>,
    oracle_replaced_by_vote: bool,
    voting_period: u64,
    enactment_delay: u64,
    proposals: Vec<ProposalView>,
}

#[tauri::command]
pub async fn governance_view(d: D<'_>) -> Result<GovernanceView, String> {
    use core_crypto::pallets::governance::{
        ProposalStatus, ENACTMENT_DELAY_BLOCKS, VOTING_PERIOD_BLOCKS,
    };
    let s = &d.state;
    let me = *s.my_peer_id.read().await;
    let my_weight = s.my_voting_weight().await;
    let chain = s.chain.read().await;
    let mut proposals: Vec<ProposalView> = chain
        .governance
        .proposals
        .values()
        .map(|p| {
            let (aye, nay) = p.tally();
            ProposalView {
                id: p.id,
                proposer: hex::encode(&p.proposer[..8]),
                summary: describe_gov_action(&p.action),
                membership: p.action.changes_membership(),
                status: match p.status {
                    ProposalStatus::Open => "open".into(),
                    ProposalStatus::Enacted { at } => format!("enacted at {at}"),
                    ProposalStatus::Rejected { at } => format!("rejected at {at}"),
                    ProposalStatus::Failed { at } => format!("failed at {at}"),
                },
                created_at: p.created_at,
                voting_closes_at: p.voting_closes_at(),
                enactable_at: p.enactable_at(),
                aye,
                nay,
                electorate: p.electorate_weight,
                passes: p.passes(),
                my_vote: p.votes.get(&me).map(|(v, _)| *v),
            }
        })
        .collect();
    proposals.sort_by_key(|p| std::cmp::Reverse(p.id));
    let session_costs = ["chat", "large_chat", "frontier"]
        .iter()
        .map(|ch| {
            let cost = chain
                .nai
                .top_tier_session_costs
                .get(*ch)
                .copied()
                .unwrap_or(core_crypto::pallets::nai::DEFAULT_CHANNEL_SESSION_COST);
            (ch.to_string(), cost)
        })
        .collect();
    Ok(GovernanceView {
        emission_target: chain.economy.target_credits_per_hour,
        daily_mint_cap: match chain.nai.daily_mint_cap {
            0 => core_crypto::pallets::nai::DEFAULT_DAILY_MINT_CAP,
            cap => cap,
        },
        session_costs,
        head: chain.head_height(),
        my_weight,
        founder: chain.authority.founder.map(hex::encode),
        dissolved_at: chain.authority.dissolved_at,
        partners: chain.authority.partners().iter().map(hex::encode).collect(),
        oracle_replaced_by_vote: chain.governance.oracle_replaced_by_vote,
        voting_period: VOTING_PERIOD_BLOCKS,
        enactment_delay: ENACTMENT_DELAY_BLOCKS,
        proposals,
    })
}

fn parse_key(hex_key: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(hex_key.trim()).map_err(|_| "the key must be hex".to_string())?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| "the key must be 32 bytes".to_string())
}

#[tauri::command]
pub async fn governance_propose(
    d: D<'_>,
    kind: String,
    key: Option<String>,
    text: Option<String>,
    amount: Option<u64>,
) -> Result<(), String> {
    use core_crypto::pallets::governance::GovAction;
    let text = text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let key = key.filter(|k| !k.trim().is_empty());
    let amount_required = || amount.ok_or_else(|| "an amount is required".to_string());
    let action = match kind.as_str() {
        "sale_oracle" => GovAction::SetSaleOracle {
            pubkey: key.as_deref().map(parse_key).transpose()?,
        },
        "direct_sale_wallet" => GovAction::SetDirectSaleWallet { address: text },
        "daily_mint_cap" => GovAction::SetDailyMintCap {
            cap: amount_required()?,
        },
        "channel_cost" => GovAction::SetChannelSessionCost {
            channel: text.ok_or("a channel is required")?,
            cost: amount_required()?,
        },
        "emission_target" => GovAction::SetEmissionTarget {
            target_credits_per_hour: amount_required()?,
        },
        "add_partner" => GovAction::AddPartner {
            pubkey: parse_key(key.as_deref().ok_or("a partner key is required")?)?,
            channels: text
                .map(|t| {
                    t.split(',')
                        .map(|c| c.trim().to_string())
                        .filter(|c| !c.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            attestation_hash: [0u8; 32],
        },
        "remove_partner" => GovAction::RemovePartner {
            pubkey: parse_key(key.as_deref().ok_or("a partner key is required")?)?,
        },
        _ => return Err("unknown proposal kind".into()),
    };
    d.state.governance_propose(action).await
}

#[tauri::command]
pub async fn governance_vote(d: D<'_>, id: u64, aye: bool) -> Result<(), String> {
    d.state.governance_vote(id, aye).await
}

#[tauri::command]
pub async fn governance_enact(d: D<'_>, id: u64) -> Result<(), String> {
    d.state.governance_enact(id).await
}

#[tauri::command]
pub async fn open_session(d: D<'_>, channel: Option<String>) -> Result<String, String> {
    use app_kernel::state::OpenSessionError;
    match d
        .state
        .open_session_onchain(channel.filter(|c| !c.trim().is_empty()))
        .await
    {
        Ok(token) => Ok(hex::encode(token)),
        Err(OpenSessionError::InvalidChannel) => Err("invalid channel name".into()),
        Err(OpenSessionError::InsufficientBalance) => {
            Err("insufficient balance — redeem contributions first".into())
        }
        Err(OpenSessionError::NotIncluded) => {
            Err("session tx not included in a block within 15s — authority unreachable?".into())
        }
        Err(OpenSessionError::Locked) => Err(app_kernel::wallet::LOCKED.into()),
        Err(OpenSessionError::Declined) => Err(app_kernel::wallet::DECLINED.into()),
    }
}

// ============================ model catalog ============================

#[derive(Serialize)]
pub struct CatalogRepo {
    id: String,
    downloads: u64,
    likes: u64,
    params_b: Option<f32>,
    trusted: bool,
}

#[derive(Serialize)]
pub struct CatalogFile {
    repo: String,
    /// The path as published, which may name a subdirectory.
    file: String,
    /// What the file is called once it is here. Repositories publish nested
    /// paths, and two files in different directories can share a name, so the
    /// list needs both: this to match against what is installed, `file` to
    /// tell two otherwise identical-looking entries apart.
    local_name: String,
    url: String,
    size_mb: u64,
    sha256: Option<String>,
    quant: Option<String>,
    params_b: Option<f32>,
    /// "comfortable" | "tight" | "toobig"
    fit: &'static str,
    needed_mb: u64,
    available_mb: u64,
    est_secs: u32,
    uses_gpu: bool,
    /// "yes" | "no" | "unknown": whether the engine in this build implements
    /// the tensor formats the file is stored in. Separate from `fit`, which
    /// only compares sizes against memory and says nothing about whether the
    /// file can be read at all.
    runnable: &'static str,
    /// The ggml type ids this build does not implement, when `runnable` is
    /// "no". Shown so the reason is checkable rather than asserted.
    unsupported_types: Vec<u32>,
}

#[derive(Serialize)]
pub struct InstalledModelView {
    file_name: String,
    size_mb: u64,
    active: bool,
}

fn machine_memory() -> (u64, Option<u64>) {
    let p = ai_engine::hw_detect::detect();
    (p.ram_mb, p.vram_mb)
}

fn checked_model_name(file_name: &str) -> Result<String, String> {
    match ai_engine::model_store::local_file_name(file_name) {
        Some(clean) if clean == file_name && !clean.starts_with('.') => Ok(clean),
        _ => Err(format!("{file_name} is not an installed model name")),
    }
}

fn fit_tag(f: ai_engine::Fit) -> &'static str {
    match f {
        ai_engine::Fit::Comfortable => "comfortable",
        ai_engine::Fit::Tight => "tight",
        ai_engine::Fit::TooBig => "toobig",
    }
}

/// Display name for whichever GGUF is actually active, so the leaderboard
/// and AI Settings always reflect the real file instead of a hand-typed
/// label that can drift out of sync after a catalog download or switch.
fn derive_model_name(path: &std::path::Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "local".to_string())
}

/// Browse GGUF chat models on HuggingFace. Empty query = most downloaded
/// from trusted publishers.
#[tauri::command]
pub async fn catalog_search(
    query: String,
    include_untrusted: bool,
) -> Result<Vec<CatalogRepo>, String> {
    let repos = ai_engine::model_catalog::search_repos(&query, 40)
        .await
        .map_err(|e| e.to_string())?;
    Ok(repos
        .into_iter()
        .filter(|r| include_untrusted || r.trusted)
        .take(25)
        .map(|r| CatalogRepo {
            id: r.id,
            downloads: r.downloads,
            likes: r.likes,
            params_b: r.params_b,
            trusted: r.trusted,
        })
        .collect())
}

/// List the GGUF variants of one repository, each judged against this
/// machine's memory so the UI can grey out what will not run.
#[tauri::command]
pub async fn catalog_files(repo: String) -> Result<Vec<CatalogFile>, String> {
    let files = ai_engine::model_catalog::list_files(&repo)
        .await
        .map_err(|e| e.to_string())?;
    let (ram_mb, vram_mb) = machine_memory();

    // Each file's header is read over HTTP before any of them is offered.
    // A repository holds a handful of files, and the alternative is telling
    // someone a model "runs well here" on a memory calculation alone and
    // letting them find out after gigabytes that the engine cannot read it.
    let verdicts = futures::future::join_all(
        files
            .iter()
            .map(|f| ai_engine::model_catalog::probe_published_header(&f.url)),
    )
    .await;

    let limit = ai_engine::tensor_type_count();
    Ok(files
        .into_iter()
        .zip(verdicts)
        .map(|(f, header)| {
            let r = ai_engine::model_catalog::assess(f.size_bytes, f.params_b, ram_mb, vram_mb);
            let (runnable, unsupported_types) = match header
                .as_ref()
                .map(|h| h.runnable_with(limit))
                .unwrap_or(ai_engine::gguf_probe::Runnable::Unknown)
            {
                ai_engine::gguf_probe::Runnable::Yes => ("yes", Vec::new()),
                ai_engine::gguf_probe::Runnable::No { unsupported } => ("no", unsupported),
                ai_engine::gguf_probe::Runnable::Unknown => ("unknown", Vec::new()),
            };
            CatalogFile {
                local_name: ai_engine::model_store::local_file_name(&f.file)
                    .unwrap_or_else(|| f.file.clone()),
                repo: f.repo,
                file: f.file,
                url: f.url,
                size_mb: f.size_bytes / (1024 * 1024),
                sha256: f.sha256_hex,
                quant: f.quant,
                params_b: f.params_b,
                fit: fit_tag(r.fit),
                needed_mb: r.needed_mb,
                available_mb: r.available_mb,
                est_secs: r.est_secs_per_reply,
                uses_gpu: r.uses_gpu,
                runnable,
                unsupported_types,
            }
        })
        .collect())
}

#[tauri::command]
pub async fn catalog_download(
    d: D<'_>,
    url: String,
    file_name: String,
    sha256: Option<String>,
) -> Result<(), String> {
    // Published paths may name a subdirectory; models are stored flat, so
    // every check and every record below uses the name it will actually have.
    let local_name = ai_engine::model_store::local_file_name(&file_name)
        .ok_or_else(|| format!("{file_name} is not a usable file name"))?;
    let (repo, published) = ai_engine::model_store::parse_hf_url(&url)
        .ok_or_else(|| "only huggingface.co resolve links can be installed".to_string())?;
    if published != file_name {
        return Err("the download link does not match the chosen file".into());
    }
    let listed = ai_engine::model_catalog::list_files(&repo)
        .await
        .map_err(|e| format!("could not read the publisher's checksums: {e}"))?;
    let publisher_hash = listed
        .into_iter()
        .find(|f| f.file == published)
        .and_then(|f| f.sha256_hex)
        .ok_or_else(|| {
            format!("{repo} publishes no checksum for {published}; refusing an unverifiable model")
        })?;
    if let Some(claimed) = sha256.as_deref() {
        if !claimed.trim().eq_ignore_ascii_case(publisher_hash.trim()) {
            return Err("the checksum does not match the one the publisher lists".into());
        }
    }
    let sha256 = Some(publisher_hash);

    let already_installed = ai_engine::ModelStore::default_dir()
        .join("models")
        .join(&local_name)
        .exists();
    if already_installed {
        return Err(format!("{local_name} is already installed"));
    }

    // Read the header before spending the bandwidth. The list has usually
    // checked this already, but the download is where the cost is paid, so it
    // is checked again here rather than trusted from a view that may be
    // stale. A header that cannot be read is not treated as a refusal: the
    // load itself will report anything this misses.
    if let Some(header) = ai_engine::model_catalog::probe_published_header(&url).await {
        if let ai_engine::gguf_probe::Runnable::No { unsupported } =
            header.runnable_with(ai_engine::tensor_type_count())
        {
            let ids = unsupported
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "{file_name} is stored in a tensor format this build cannot read \
                 (ggml type {ids}). It is built for a patched or newer llama.cpp, so \
                 downloading it would not give you a model you can run."
            ));
        }
    }
    {
        // Hold the lock across check-and-set so two rapid clicks can't both
        // observe an empty slot and both start downloading — the progress
        // tracker is a single global slot, so only one download may run.
        let mut progress = d.state.model_download_progress.lock().await;
        if progress.is_some() {
            return Err("a model download is already in progress".to_string());
        }
        *progress = Some((0, 0));
    }
    let state = d.state.clone();
    tokio::spawn(async move {
        let store = ai_engine::ModelStore::new(ai_engine::ModelStore::default_dir());
        let progress_state = state.clone();
        let result = store
            .download_model(&url, &file_name, sha256.as_deref(), move |done, total| {
                if let Ok(mut g) = progress_state.model_download_progress.try_lock() {
                    *g = Some((done, total));
                }
            })
            .await;
        *state.model_download_progress.lock().await = None;
        match result {
            Ok(path) => {
                tracing::info!(path = %path.display(), "Model downloaded and verified");
                {
                    let mut mc = state.ai_model_config.write().await;
                    mc.model_path = Some(path.display().to_string());
                    mc.model_name = derive_model_name(&path);
                }
                // A checksum proves the bytes arrived intact, not that this
                // build can run them. The download is only adopted as the
                // active model once it has actually loaded.
                match state.initialize_ai_engine().await {
                    Ok(()) => {
                        let _ = nepheral_config::update(|cfg| {
                            cfg.active_model = Some(local_name.clone())
                        });
                    }
                    Err(e) => {
                        tracing::error!(error = %e, file = %local_name, "Downloaded model will not load");
                        *state.model_download_error.lock().await = Some(e.to_string());
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, file = %local_name, "Model download failed");
                // Surfaced, not just logged: the button that started this
                // otherwise waits for a file that is never going to appear.
                *state.model_download_error.lock().await = Some(e.to_string());
                state.log(format!("Model download failed: {e}")).await;
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn models_installed(d: D<'_>) -> Result<Vec<InstalledModelView>, String> {
    let store = ai_engine::ModelStore::new(ai_engine::ModelStore::default_dir());
    let active = nepheral_config::load_config()
        .and_then(|c| c.active_model)
        .unwrap_or_default();
    let current = AppState::local_model_path();
    let _ = &d;
    Ok(store
        .installed()
        .into_iter()
        .map(|m| InstalledModelView {
            active: m.file_name == active
                || current.as_ref().map(|p| *p == m.path).unwrap_or(false),
            file_name: m.file_name,
            size_mb: m.size_bytes / (1024 * 1024),
        })
        .collect())
}

/// Switches the model in use, and reports it if that does not work.
///
/// The load is awaited rather than spawned, and the choice is recorded only
/// once it succeeds. Recording first meant a model that llama.cpp refuses was
/// stored as the active one while the previously loaded engine went on
/// answering: the app showed one model and spoke as another, and said nothing
/// until the next restart left it with no engine at all.
#[tauri::command]
pub async fn model_switch(d: D<'_>, file_name: String) -> Result<(), String> {
    let file_name = checked_model_name(&file_name)?;
    let path = AppState::models_dir().join(&file_name);
    if !path.exists() {
        return Err(format!("{file_name} is not installed"));
    }

    // Switching now stops the running model before loading the next, so a
    // file that cannot load would leave nothing running at all. The tensor
    // formats are checked first, while the current model is still there to
    // keep.
    if let Some(header) = ai_engine::gguf_probe::probe(&path) {
        if let ai_engine::gguf_probe::Runnable::No { unsupported } =
            header.runnable_with(ai_engine::tensor_type_count())
        {
            let ids = unsupported
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "{file_name} is stored in a tensor format this build cannot read \
                 (ggml type {ids}). The model in use was left running."
            ));
        }
    }

    let previous = {
        let mc = d.state.ai_model_config.read().await;
        (mc.model_path.clone(), mc.model_name.clone())
    };
    {
        let mut mc = d.state.ai_model_config.write().await;
        mc.model_path = Some(path.display().to_string());
        mc.model_name = derive_model_name(&path);
    }

    match d.state.initialize_ai_engine().await {
        Ok(()) => {
            let saved = nepheral_config::update(|cfg| cfg.active_model = Some(file_name))?;
            *d.draft.lock().expect("draft lock") = saved;
            Ok(())
        }
        Err(e) => {
            // The previous engine was stopped to make room, so it is loaded
            // again rather than leaving the node with none.
            {
                let mut mc = d.state.ai_model_config.write().await;
                mc.model_path = previous.0;
                mc.model_name = previous.1;
            }
            match d.state.initialize_ai_engine().await {
                Ok(()) => Err(format!("{e}. The previous model was loaded again.")),
                Err(again) => Err(format!(
                    "{e}. Loading the previous model again also failed: {again}"
                )),
            }
        }
    }
}

#[tauri::command]
pub async fn model_delete(d: D<'_>, file_name: String) -> Result<(), String> {
    let file_name = checked_model_name(&file_name)?;
    let path = AppState::models_dir().join(&file_name);
    if !path
        .extension()
        .is_some_and(|x| x.eq_ignore_ascii_case("gguf"))
    {
        return Err("only installed .gguf models can be deleted".into());
    }

    // Deciding "was this the active model" before removing the file, since
    // AppState::local_model_path() re-resolves from what's on disk and would
    // no longer agree with `path` once it's gone.
    let was_active = AppState::local_model_path()
        .map(|p| p == path)
        .unwrap_or(false);

    std::fs::remove_file(&path).map_err(|e| format!("could not delete: {e}"))?;

    if let Some(mut cfg) = nepheral_config::load_config() {
        if cfg.active_model.as_deref() == Some(file_name.as_str()) {
            cfg.active_model = None;
            let _ = nepheral_config::save_config(&cfg);
        }
    }
    {
        let mut draft = d.draft.lock().expect("draft lock");
        if draft.active_model.as_deref() == Some(file_name.as_str()) {
            draft.active_model = None;
        }
    }

    if was_active {
        // Don't leave ai_model_config/ai_engine pointing at a file that no
        // longer exists. Fall back to whatever other model is on disk (now
        // that active_model is cleared, local_model_path() picks the next
        // one it finds), or clear the engine entirely if nothing is left.
        match AppState::local_model_path() {
            Some(fallback) => {
                tracing::info!(
                    deleted = %file_name,
                    fallback = %fallback.display(),
                    "Deleted active model — switching engine to next available model"
                );
                {
                    let mut mc = d.state.ai_model_config.write().await;
                    mc.model_path = Some(fallback.display().to_string());
                    mc.model_name = derive_model_name(&fallback);
                }
                let state = d.state.clone();
                tokio::spawn(async move {
                    let _ = state.initialize_ai_engine().await;
                });
            }
            None => {
                tracing::warn!(
                    deleted = %file_name,
                    "Deleted active model and no other model is installed — clearing engine"
                );
                {
                    let mut mc = d.state.ai_model_config.write().await;
                    mc.model_path = None;
                }
                *d.state.ai_engine.write().await = None;
            }
        }
    }

    Ok(())
}

// ============================ setup wizard ============================

#[derive(Serialize)]
pub struct SetupView {
    first_run: bool,
    phrase: Vec<String>,
    peer_id: String,
    model_tier: String,
    backend_external: bool,
    external_url: String,
    download_url: String,
    model_present: bool,
    download: Option<(u64, u64)>,
    download_error: Option<String>,
    hw: Option<HwView>,
    tuning: Option<TuningView>,
}

#[derive(Serialize)]
pub struct HwView {
    cpu_cores: usize,
    ram_mb: u64,
    gpu: String,
    recommended_tier: String,
    /// The exact GGUF this machine installs when nothing is pinned, and its
    /// download size — so the tier shown is something concrete rather than a
    /// label whose model the user has to guess at.
    default_model_file: Option<String>,
    default_model_mb: u64,
}

#[tauri::command]
pub async fn setup_view(d: D<'_>) -> Result<SetupView, String> {
    let cfg = d.draft.lock().expect("draft lock").clone();
    let first_run = *d.first_run.lock().expect("first_run lock");
    // The phrase is shown once, by the window that made the wallet.
    let phrase = Vec::new();
    let me = d.state.my_account().await;
    let download = *d.state.model_download_progress.lock().await;
    let download_error = d.state.model_download_error.lock().await.clone();
    Ok(SetupView {
        first_run,
        phrase,
        peer_id: hex::encode(me),
        model_tier: cfg.model_tier.clone(),
        backend_external: uses_external(&cfg),
        external_url: cfg.external_url.clone().unwrap_or_default(),
        download_url: cfg.model_download_url.clone(),
        model_present: AppState::local_model_path().is_some(),
        download,
        download_error,
        hw: None,
        tuning: cfg.inference_tuning.as_ref().map(|t| TuningView {
            n_ctx: t.n_ctx,
            n_ubatch: t.n_ubatch,
            max_new_tokens: t.max_new_tokens,
            estimated_ram_mb: t.estimated_ram_mb,
        }),
    })
}

#[tauri::command]
pub async fn setup_hw_detect(d: D<'_>) -> Result<(HwView, TuningView), String> {
    let (hw, tuning, tier_slug) = detect_hardware();
    {
        let mut draft = d.draft.lock().expect("draft lock");
        draft.model_tier = tier_slug;
        draft.inference_tuning = Some(nepheral_config::InferenceTuningConfig {
            n_ctx: tuning.n_ctx,
            n_ubatch: tuning.n_ubatch,
            max_new_tokens: tuning.max_new_tokens,
            estimated_ram_mb: tuning.estimated_ram_mb,
        });
    }
    Ok((hw, tuning))
}

#[derive(Deserialize)]
pub struct SetupModelChoice {
    pub tier: String, // "minimum" | "standard" | "performance" | "external"
    pub external_url: Option<String>,
}

#[tauri::command]
pub async fn setup_set_model(d: D<'_>, choice: SetupModelChoice) -> Result<(), String> {
    let mut draft = d.draft.lock().expect("draft lock");
    if choice.tier == "external" {
        draft.backend = nepheral_config::BackendChoice::External;
        draft.external_url = choice.external_url.filter(|u| !u.trim().is_empty());
    } else {
        draft.backend = nepheral_config::BackendChoice::Local;
        draft.model_tier = choice.tier;
    }
    Ok(())
}

#[tauri::command]
pub async fn setup_start_download(d: D<'_>, url: String) -> Result<(), String> {
    {
        let mut draft = d.draft.lock().expect("draft lock");
        draft.model_download_url = url.clone();
    }
    let state = d.state.clone();
    *d.state.model_download_error.lock().await = None;
    tokio::spawn(async move {
        let Some((hf_repo, hf_file)) = ai_engine::model_store::parse_hf_url(&url) else {
            let msg = format!(
                "{url} is not a huggingface.co resolve link, so no publisher checksum exists. Refusing to install an unverified model."
            );
            tracing::warn!("{msg}");
            *state.model_download_progress.lock().await = None;
            *state.model_download_error.lock().await = Some(msg);
            return;
        };
        let expected_hash = match ai_engine::model_catalog::list_files(&hf_repo).await {
            Ok(files) => files
                .into_iter()
                .find(|f| f.file == hf_file)
                .and_then(|f| f.sha256_hex),
            Err(e) => {
                tracing::warn!(error = %e, repo = %hf_repo, "Failed to query model checksum from catalog");
                None
            }
        };
        let Some(expected_hash) = expected_hash else {
            let msg = format!(
                "{hf_repo} publishes no checksum for {hf_file}. Refusing to install an unverified model."
            );
            tracing::warn!("{msg}");
            *state.model_download_progress.lock().await = None;
            *state.model_download_error.lock().await = Some(msg);
            return;
        };

        let store = ai_engine::ModelStore::new(ai_engine::ModelStore::default_dir());
        let progress_state = state.clone();
        let result = store
            .download_model(&url, &hf_file, Some(&expected_hash), move |done, total| {
                if let Ok(mut g) = progress_state.model_download_progress.try_lock() {
                    *g = Some((done, total));
                }
            })
            .await;
        match result {
            Ok(path) => {
                *state.model_download_progress.lock().await = None;
                {
                    let mut cfg = state.ai_model_config.write().await;
                    cfg.model_path = Some(path.display().to_string());
                    cfg.model_name = derive_model_name(&path);
                }
                match state.initialize_ai_engine().await {
                    Ok(()) => {
                        let _ = app_kernel::nepheral_config::update(|cfg| {
                            cfg.active_model = Some(hf_file.clone())
                        });
                    }
                    Err(e) => {
                        tracing::error!(error = %e, file = %hf_file, "Downloaded model will not load");
                        *state.model_download_error.lock().await = Some(e.to_string());
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Model download failed in setup wizard");
                *state.model_download_progress.lock().await = None;
                *state.model_download_error.lock().await = Some(e.to_string());
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn setup_finish(d: D<'_>) -> Result<(), String> {
    let draft = d.draft.lock().expect("draft lock").clone();
    let saved = nepheral_config::update(|cfg| {
        // The wizard downloads a model while it is still open and records the
        // choice straight to disk, so that one field on disk is newer than
        // the copy the wizard is holding.
        let downloaded = cfg.active_model.take();
        *cfg = draft;
        if cfg.active_model.is_none() {
            cfg.active_model = downloaded;
        }
        cfg.desktop_setup_done = true;
    })?;
    let external = uses_external(&saved);
    *d.draft.lock().expect("draft lock") = saved;
    *d.first_run.lock().expect("first_run lock") = false;
    // Finishing without having picked a model still installs one.
    d.start_model_sync(external).await;
    Ok(())
}

/// The node answers through an external server rather than a local model.
/// `backend` alone cannot say so: it defaults to External in a config that
/// nobody has chosen anything in yet.
fn uses_external(cfg: &NepheralConfig) -> bool {
    cfg.uses_external_backend()
}

/// The wizard's download step: installs the recommended model unless the
/// user already picked one.
#[tauri::command]
pub async fn setup_fetch_model(d: D<'_>) -> Result<(), String> {
    let external = uses_external(&d.draft.lock().expect("draft lock"));
    d.start_model_sync(external).await;
    Ok(())
}

// ============================ api tab ============================

#[derive(Serialize)]
pub struct ApiTabView {
    enabled: bool,
    addr: String,
    /// Empty while locked: the API keeps answering, the token stays hidden.
    token: String,
    locked: bool,
    policy_summary: String,
    policy_tag: &'static str, // cross | standard | toptier | local
}

#[tauri::command]
pub async fn api_view(d: D<'_>) -> Result<ApiTabView, String> {
    use nepheral_api::creds::CredsMode;
    let cfg = nepheral_config::load_config().unwrap_or_default();
    let policy = d.state.api_routing_policy.read().await.clone();
    Ok(ApiTabView {
        enabled: cfg.local_api_server_enabled,
        addr: format!(
            "{}:{}",
            cfg.local_api_server_host, cfg.local_api_server_port
        ),
        token: crate::lock::with_secrets(&d, |s| s.api_token.clone())
            .ok()
            .flatten()
            .unwrap_or_default(),
        locked: !d.state.is_unlocked().await,
        policy_summary: policy.human_summary(),
        policy_tag: match policy {
            CredsMode::Cross { .. } => "cross",
            CredsMode::StandardOnly { .. } => "standard",
            CredsMode::TopTierOnly { .. } => "toptier",
            CredsMode::LocalOnly => "local",
        },
    })
}

#[tauri::command]
pub async fn api_set_routing(d: D<'_>, tag: String) -> Result<(), String> {
    use nepheral_api::creds::CredsMode;
    let mode = match tag.as_str() {
        "cross" => CredsMode::default(),
        "standard" => CredsMode::StandardOnly {
            total_deadline_ms: 30_000,
        },
        "toptier" => CredsMode::TopTierOnly {
            channel: "chat".into(),
            total_deadline_ms: 60_000,
        },
        "local" => CredsMode::LocalOnly,
        _ => return Err("unknown mode".into()),
    };
    let state = d.state.clone();
    tokio::spawn(async move {
        *state.api_routing_policy.write().await = mode;
        state.save_routing_policy().await;
    });
    Ok(())
}

/// The phone's "help the network in the background" switch. A desktop
/// always helps while it runs.
#[tauri::command]
pub fn custody_background() -> bool {
    #[cfg(target_os = "android")]
    {
        crate::android::custody_background()
    }
    #[cfg(not(target_os = "android"))]
    {
        true
    }
}

#[tauri::command]
pub fn set_custody_background(on: bool) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        crate::android::set_custody_background(on)?;
        Ok(on)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = on;
        Err("background help is a phone setting".into())
    }
}

pub use tauri_handler::handler;

mod tauri_handler {
    use super::*;

    pub fn handler() -> impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static {
        tauri::generate_handler![
            status,
            crate::updates::update_check,
            crate::updates::update_install,
            activity_log,
            chat_sessions,
            chat_thread,
            chat_new,
            chat_select,
            chat_leave,
            chat_send,
            chat_confirm,
            chat_cancel,
            chat_snooze,
            chat_rename,
            chat_private,
            memory_view,
            memory_download,
            memory_keep,
            custody_background,
            set_custody_background,
            set_trust_filter,
            rooms_list,
            room_join,
            room_create,
            room_leave,
            room_view,
            room_send,
            peers_view,
            peer_connect,
            ai_view,
            ai_apply,
            ai_redetect,
            ai_save_prompt,
            ai_download_from_peer,
            network_view,
            network_set,
            bootstrap_add,
            leaderboard_view,
            wallet_view,
            redeem,
            transfer,
            open_session,
            market_view,
            governance_view,
            rate_chat_reply,
            rate_room_reply,
            governance_propose,
            governance_vote,
            governance_enact,
            market_post_ask,
            market_post_bid,
            market_cancel,
            crate::lock::vault_change_password,
            crate::lock::lock_state,
            crate::lock::wallet_unlock,
            crate::lock::wallet_lock,
            crate::lock::wallet_restore,
            crate::lock::set_auto_lock,
            crate::lock::set_ui_lang,
            crate::gate::vault_status,
            crate::gate::vault_create,
            crate::gate::vault_migrate,
            crate::gate::vault_unlock,
            crate::gate::vault_restore,
            crate::gate::vault_export_pdf,
            crate::gate::vault_enter,
            market_fill_memo,
            market_claim,
            catalog_search,
            catalog_files,
            catalog_download,
            ai_set_tuning,
            models_installed,
            model_switch,
            model_delete,
            setup_view,
            setup_hw_detect,
            setup_set_model,
            setup_start_download,
            setup_finish,
            setup_fetch_model,
            api_view,
            api_set_routing
        ]
    }
}
