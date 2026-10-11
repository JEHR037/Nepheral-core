// SPDX-License-Identifier: BUSL-1.1

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_crypto::agent_crypto::EncryptedBundle;
use core_crypto::identity::PeerId;
use mesh_network::protocol::{
    frame, message_types, AgentClaim, AgentReply, AgentResponseDirect, LotteryInputs, SharePush,
    ShareRelease, MAX_CANDIDATES, MAX_CLAIM_RANK,
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use zeroize::Zeroize;

use crate::state::{now_ms, send_frame, AppState};

pub const AGENT_SHARE_TTL: Duration = Duration::from_secs(10 * 60);
pub const CUSTODY_CAP: usize = 4_096;
pub const COLLECTOR_TTL: Duration = Duration::from_secs(3 * 60);
/// How long after claiming an answer waits for the last custodians' shares
/// before it names who held them.
pub const CUSTODY_GRACE: Duration = Duration::from_secs(2);
pub const RELAY_ROUTE_TTL: Duration = Duration::from_secs(2 * 60);
pub const RELAY_ROUTE_CAP: usize = 4_096;
/// How many relays carry each relayed frame.
pub const RELAY_FANOUT: usize = 2;
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(120);
pub const SHARE_PUSH_MAX_SKEW_MS: u64 = 10 * 60 * 1_000;
pub const RANK_DELAYS: [Duration; 3] = [
    Duration::ZERO,
    Duration::from_secs(8),
    Duration::from_secs(16),
];
pub const LOWER_RANK_GRACE: Duration = Duration::from_secs(60);
/// How often a waiting candidate rechecks whether the ranks ahead of it have
/// claimed or declined. Short enough that a decline is acted on almost at
/// once, long enough that six waiting candidates cost nothing.
pub const DECLINE_POLL: Duration = Duration::from_millis(200);
/// How many times a query is re-drawn when every candidate declined. Each
/// node answers one question at a time, so on a small mesh a burst of
/// parallel questions finds everyone busy; these are the retries that let it
/// land as soon as somebody frees up rather than failing at the deadline.
pub const MAX_QUERY_ATTEMPTS: u8 = 8;
/// Breathing room before a re-draw, so a burst does not spin.
pub const RETRY_BACKOFF_STEP: Duration = Duration::from_millis(700);
pub const RETRY_BACKOFF_MAX: Duration = Duration::from_secs(4);
/// Upper bound on how long a claim holds this node's slot if nothing
/// releases it - the case where the asker vanished and the shares never
/// reached threshold. Only has to cover claim-to-inference, a second or two;
/// once inference starts, `inference_busy` keeps other claims out.
pub const RESERVATION_TTL: Duration = Duration::from_secs(10);
/// Upper bound on a slot whose worker started, should its release be lost.
pub const SERVING_TTL: Duration = Duration::from_secs(10 * 60);

/// How long a decline counts against a node in the lottery.
pub const DECLINE_MEMORY: Duration = Duration::from_secs(20);
/// A winner that has not answered this long after claiming is taken to be
/// stuck (a model thinking on a CPU, a machine gone to sleep): the asker
/// draws again without it.
pub const STALLED_CLAIM: Duration = Duration::from_secs(75);
/// How long this node keeps a stalled winner out of its draws.
pub const SLOW_MEMORY: Duration = Duration::from_secs(10 * 60);
/// How long a claim that arrived before its push is kept.
pub const EARLY_CLAIM_TTL: Duration = Duration::from_secs(60);

/// A candidate's lottery weight, lowered when it is probably occupied.
///
/// Reputation sets the weight; availability only shrinks it, never to zero,
/// so a busy node stays reachable when it is the only one. A node that just
/// declined a query is the strongest sign: a quarter. One whose last stats
/// say it is answering: half. Each draw that lands on an occupied node costs
/// the asker a decline round-trip before another node takes the query.
pub fn availability_weight(weight: u8, declined_recently: bool, busy: bool) -> u8 {
    let mut w = weight;
    if declined_recently {
        w /= 4;
    }
    if busy {
        w /= 2;
    }
    w.max(1)
}

/// Grows with each attempt so eight of them span most of the asker's
/// 30-second deadline: an answer on a busy node takes 5-10s, and the retries
/// have to still be running when one frees up.
fn retry_backoff(attempt: u8) -> Duration {
    (RETRY_BACKOFF_STEP * (u32::from(attempt) + 1)).min(RETRY_BACKOFF_MAX)
}
pub const CONTEXT_WINDOW_MESSAGES: usize = 4;
pub const MEMORY_MAX_CHARS: usize = 2_000;
pub const MAX_RECONSTRUCT_ATTEMPTS: usize = 20;

/// What became of a message the person sent.
#[cfg(feature = "client")]
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SendOutcome {
    Sent,
    /// Private: kept in the conversation, never sent.
    Kept,
    /// Waiting for the person to confirm the older context it would carry.
    Confirm(crate::conversations::ContextPreview),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSessionKind {
    Local,
    P2P,
}

/// What the device keeps of a message to choose context with
/// (`nepheral_recall`). Only the desktop and phone compute it.
#[cfg(feature = "client")]
pub type Vectors = nepheral_recall::memory::MessageVectors;
#[cfg(not(feature = "client"))]
#[derive(Debug, Clone)]
pub struct Vectors;

#[derive(Debug, Clone)]
pub struct AgentChatEntry {
    pub from_me: bool,
    pub content: String,
    pub timestamp: Instant,
    /// Wall-clock time, kept when the conversation is saved.
    pub at_ms: u64,
    pub rating: Option<RatingRef>,
    pub rated: Option<bool>,
    /// Marked private: never chosen as context, never sent
    /// (docs/CONVERSATIONS.md).
    pub private: bool,
    pub vectors: Option<Vectors>,
    /// Not an answer: what went wrong, in the node's words (`error_code`
    /// turns it into something the UI says in its language).
    pub error: bool,
}

/// A failure shown in a conversation, as a code the UI words in its own
/// language. The text stays in English for logs and older screens.
pub fn error_code(message: &str) -> &'static str {
    let m = message.to_ascii_lowercase();
    if m.contains("no ai-capable peer") {
        "no_ai"
    } else if m.contains("no mesh peers connected") {
        "offline"
    } else if m.contains("currently serves the") {
        "no_channel"
    } else if m.contains("every ai node was busy") {
        "busy"
    } else if m.contains("in time") || m.contains("timeout") || m.contains("abandoned") {
        "timeout"
    } else if m.contains("local engine not initialized") {
        "local_not_ready"
    } else if m.contains("local agent busy") {
        "local_busy"
    } else if m.contains("local engine error") {
        "local_error"
    } else {
        "generic"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RatingRef {
    pub query_digest: [u8; 32],
    pub responder: PeerId,
}

impl Drop for AgentChatEntry {
    fn drop(&mut self) {
        self.content.zeroize();
    }
}

#[derive(Debug, Clone)]
pub struct AgentSession {
    pub id: u64,
    pub kind: AgentSessionKind,
    pub label: String,
    pub messages: Vec<AgentChatEntry>,
    pub created_at: Instant,
    pub created_ms: u64,
    pub awaiting_response: bool,
}

#[derive(Debug, Clone)]
pub struct StoredAgentShare {
    pub push: SharePush,
    pub stored_at: Instant,
    pub releases: Vec<(u8, Instant)>,
}

pub enum QueryTarget {
    Session(u64),
    Room(String),
    Oneshot(oneshot::Sender<Result<String, String>>),
}

impl QueryTarget {
    /// True once whoever asked has stopped waiting. Only an API call can do
    /// that - it hangs up at its own deadline - whereas a session or room
    /// reply is delivered whenever it lands, so those are never abandoned.
    fn abandoned(&self) -> bool {
        matches!(self, QueryTarget::Oneshot(tx) if tx.is_closed())
    }
}

pub struct PendingQuery {
    pub target: QueryTarget,
    pub wrapped_for: Vec<PeerId>,
    pub bundle_digest: [u8; 32],
    pub sent_at: Instant,
    /// Kept so a query whose candidates all declined can be drawn again.
    /// This is the asker's own plaintext, which it already holds - no
    /// responder stores anything it would not have stored anyway.
    pub payload: QueryPayload,
    pub attempt: u8,
    /// Who was given a share, so a custody list in the answer can be checked
    /// against the custodians this node actually chose.
    pub custodians: Vec<PeerId>,
    /// The candidate that claimed it, and when: the asker shows that its
    /// question is being answered, and hands it to another draw if that
    /// candidate stalls.
    pub claimed: Option<(PeerId, Instant)>,
}

#[derive(Debug, Clone)]
pub struct ClaimCollector {
    pub push: SharePush,
    pub rank: u8,
    pub shares: HashMap<u8, Vec<u8>>,
    pub started_at: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTurn {
    pub r: char,
    pub c: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentQuery {
    pub version: u8,
    pub prompt: String,
    pub window: Vec<AgentTurn>,
    pub memory: Option<String>,
    pub system: Option<String>,
    pub want_memory: bool,
}

impl AgentQuery {
    pub const VERSION: u8 = 2;

    pub fn encode(&self) -> Vec<u8> {
        core_crypto::wire::encode(self)
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let q: Self = core_crypto::wire::decode(bytes)?;
        (q.version == Self::VERSION && q.window.len() <= CONTEXT_WINDOW_MESSAGES * 2).then_some(q)
    }
}

/// Everything about a query except where the answer goes. Split out so a
/// dispatch can be repeated: `QueryTarget::Oneshot` owns a channel sender and
/// cannot be cloned, but the question itself can.
#[derive(Clone)]
pub struct QueryPayload {
    pub prompt: String,
    pub window: Vec<AgentTurn>,
    pub memory: Option<String>,
    pub system: Option<String>,
    pub target_channel: Option<String>,
    pub session_token: Option<[u8; 32]>,
    pub prefer_large: bool,
}

pub struct PrivateQuery {
    pub prompt: String,
    pub window: Vec<AgentTurn>,
    pub memory: Option<String>,
    pub system: Option<String>,
    pub target_channel: Option<String>,
    pub session_token: Option<[u8; 32]>,
    /// Drawn among nodes with a model of `LARGE_MODEL_B` billion parameters
    /// or more while there are any: a summary of a whole conversation is
    /// beyond the smallest models.
    pub prefer_large: bool,
    pub target: QueryTarget,
}

/// What `PrivateQuery::prefer_large` asks for, in billions of parameters.
pub const LARGE_MODEL_B: u16 = 3;

impl PrivateQuery {
    fn payload(&self) -> QueryPayload {
        QueryPayload {
            prompt: self.prompt.clone(),
            window: self.window.clone(),
            memory: self.memory.clone(),
            system: self.system.clone(),
            target_channel: self.target_channel.clone(),
            session_token: self.session_token,
            prefer_large: self.prefer_large,
        }
    }

    fn from_parts(payload: QueryPayload, target: QueryTarget) -> Self {
        Self {
            prompt: payload.prompt,
            window: payload.window,
            memory: payload.memory,
            system: payload.system,
            target_channel: payload.target_channel,
            session_token: payload.session_token,
            prefer_large: payload.prefer_large,
            target,
        }
    }
}

// Leads with the task and scopes the placeholder rule tightly on purpose.
//
// The previous wording opened with "a conversation relayed through a privacy
// network" and said "never guess". Models in the 0.5B-3B range that answer on
// this mesh do not compartmentalise that: they read it as a posture rather
// than a rule about two token shapes, and settle into refusing - one of them
// answered unrelated questions with "no tengo la capacidad de ... estar
// conectado a redes privadas", quoting this sentence back. Where the message
// came from is not the responder's business and saying so only primes it.
//
// The placeholder rule goes in only when the question carries placeholders:
// shown on every question, its examples taught small models to write
// "[[RESTAURANT_1]]" in place of restaurants they know.
const RESPONDER_INSTRUCTION: &str =
    "Answer the user's question directly and completely, as you normally would.";

const PLACEHOLDER_RULE: &str = "Some values appear as placeholders like [[EMAIL_1]] or [[NAME_2]]: copy those through exactly as written and do not speculate about what they stand for. That rule applies only to those bracketed tokens - everything else in the message is an ordinary request. Do not write placeholders of your own.";

/// Who answers, for when the person asks: the model by its
/// name, through Nepheral. Worded as a fact to state only when asked, so it
/// does not become a preamble or a reason to refuse.
fn identity_line(model: &str) -> String {
    format!(
        "If you are asked who or what you are, or which model is answering, say that this answer comes from the model {model} through the Nepheral ecosystem."
    )
}

/// A model's file name as a person would say it: "Qwen2.5-3B-Instruct-Q4_K_M"
/// is "Qwen2.5 3B Instruct". Everything from the quantisation tag on goes.
fn display_model_name(name: &str) -> String {
    const TAGS: &[&str] = &[
        "q2_", "q3_", "q4_", "q5_", "q6_", "q8_", "q4", "q5", "q6", "q8", "iq1", "iq2", "iq3",
        "iq4", "bf16", "f16", "f32", "fp16", "fp8", "gguf",
    ];
    let lower = name.to_ascii_lowercase();
    let is_tag = |i: usize| {
        let rest = &lower[i..];
        TAGS.iter().any(|t| {
            rest.starts_with(t)
                && rest[t.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_ascii_alphabetic() || t.ends_with('_') || c == 'k')
        })
    };
    let cut = name
        .char_indices()
        .find(|&(i, c)| i > 0 && matches!(c, '-' | '_' | '.') && is_tag(i + 1))
        .map_or(name.len(), |(i, _)| i);
    let shown = name[..cut].replace(['-', '_'], " ");
    let shown = shown.split_whitespace().collect::<Vec<_>>().join(" ");
    if shown.is_empty() {
        name.to_string()
    } else {
        shown
    }
}

fn carries_placeholders(query: &AgentQuery) -> bool {
    let has = |t: &str| t.contains("[[");
    has(&query.prompt)
        || query.window.iter().any(|t| has(&t.c))
        || query.memory.as_deref().is_some_and(has)
        || query.system.as_deref().is_some_and(has)
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F | 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

/// Small Qwen models now and then end an answer in another language with a
/// stray Chinese character. Taken off when it is a single character (a word
/// such as "arigatou" written in kana stays) and nothing else in the answer
/// is Chinese, Japanese or Korean.
fn trim_stray_cjk(text: &str) -> &str {
    let body = text.trim_end();
    let head = body.trim_end_matches(|c: char| is_cjk(c) || c.is_whitespace());
    let tail = &body[head.len()..];
    let letters = tail
        .chars()
        .filter(|&c| is_cjk(c) && !matches!(c as u32, 0x3000..=0x303F | 0xFF00..=0xFFEF))
        .count();
    if tail.is_empty() || letters > 1 || head.chars().any(is_cjk) {
        return text;
    }
    if head.chars().filter(|c| c.is_alphabetic()).count() < 10 {
        return text;
    }
    head
}

/// A question about Chinese, Japanese or Korean, or one written in them: its
/// answer may quote those characters on purpose.
fn asks_about_cjk(prompt: &str) -> bool {
    const LANGUAGES: [&str; 22] = [
        "chinese",
        "chino",
        "chinois",
        "chinesisch",
        "mandarin",
        "mandar\u{ed}n",
        "cantonese",
        "canton\u{e9}s",
        "japanese",
        "japon\u{e9}s",
        "japones",
        "japonais",
        "japanisch",
        "korean",
        "coreano",
        "cor\u{e9}en",
        "koreanisch",
        "kanji",
        "hanzi",
        "hiragana",
        "katakana",
        "hangul",
    ];
    if prompt.chars().any(is_cjk) {
        return true;
    }
    let lower = prompt.to_lowercase();
    LANGUAGES.iter().any(|l| lower.contains(l)) || lower.contains("pinyin")
}

/// [`trim_stray_cjk`], and also a word or two that slipped into the middle
/// of an answer in another script ("sound 传播 through the air"). Only
/// when the answer is long enough to be clearly in that other script, the
/// stray characters are few (four at most, in one or two places), and the
/// question neither contains them nor asks about those languages.
fn strip_stray_cjk(text: &str, prompt: &str) -> String {
    let head = trim_stray_cjk(text);
    if asks_about_cjk(prompt) {
        return head.to_string();
    }
    let is_letter = |c: char| is_cjk(c) && !matches!(c as u32, 0x3000..=0x303F | 0xFF00..=0xFFEF);
    let letters = head.chars().filter(|&c| is_letter(c)).count();
    let others = head
        .chars()
        .filter(|&c| c.is_alphabetic() && !is_cjk(c))
        .count();
    let mut runs = 0;
    let mut in_run = false;
    for c in head.chars() {
        if is_cjk(c) && !in_run {
            runs += 1;
        }
        in_run = is_cjk(c);
    }
    if letters == 0 || letters > 4 || runs > 2 || others < 40 {
        return head.to_string();
    }
    let mut out = String::with_capacity(head.len());
    for c in head.chars().filter(|&c| !is_cjk(c)) {
        // Close the gap the removed word leaves: no double space, no space
        // before punctuation.
        if c == ' ' && out.ends_with(' ') {
            continue;
        }
        if matches!(c, ',' | '.' | ';' | ':' | '!' | '?') && out.ends_with(' ') {
            out.pop();
        }
        out.push(c);
    }
    out
}

/// The same question sent again within this window, before anything came
/// back for the first, is taken for a double tap and dropped.
#[cfg(feature = "client")]
const REPEAT_SEND_WINDOW_MS: u64 = 10_000;

const MEMORY_INSTRUCTION: &str = "Update the conversation memory. Output at most 12 short bullet points with the facts, decisions and preferences established so far, merging the previous memory with the latest exchange. Keep every [[PLACEHOLDER]] exactly as written. Output only the bullet list.";

/// Re-enters the dispatcher for another draw.
///
/// The watchdog this is called from was spawned *by* the dispatcher, so the
/// future is recursive and `impl Future` cannot be proven `Send`. Returning a
/// boxed trait object that asserts `Send` cuts the cycle.
fn redraw(
    state: AppState,
    q: PrivateQuery,
    attempt: u8,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<u64, String>> + Send>> {
    Box::pin(async move { state.dispatch_private_query_attempt(q, attempt).await })
}

/// True when every candidate ahead of `rank` appears in `decliners`. Ranks
/// are positions in the push's `wrapped_for`, so only the node actually
/// holding a place can give it up.
fn ranks_ahead_declined(
    decliners: &std::collections::BTreeSet<PeerId>,
    rank: u8,
    wrapped_for: &[PeerId],
) -> bool {
    let rank = rank as usize;
    rank > 0
        && wrapped_for.len() >= rank
        && wrapped_for[..rank].iter().all(|p| decliners.contains(p))
}

fn rank_delay(rank: u8) -> Duration {
    RANK_DELAYS
        .get(rank as usize)
        .copied()
        .unwrap_or(RANK_DELAYS[RANK_DELAYS.len() - 1])
}

impl AppState {
    pub async fn create_agent_session(&self, kind: AgentSessionKind) -> u64 {
        let id = {
            let mut next = self.next_agent_session_id.lock().await;
            let v = *next;
            *next += 1;
            v
        };
        let label = match kind {
            AgentSessionKind::Local => format!("Local #{id}"),
            AgentSessionKind::P2P => format!("P2P #{id}"),
        };
        self.agent_sessions.write().await.insert(
            id,
            AgentSession {
                id,
                kind,
                label,
                messages: Vec::new(),
                created_at: Instant::now(),
                created_ms: now_ms(),
                awaiting_response: false,
            },
        );
        self.conversations_changed();
        let terms = crate::nepheral_config::load_config()
            .map(|c| c.redact_terms)
            .unwrap_or_default();
        self.session_redactors
            .lock()
            .await
            .insert(id, ai_engine::redaction::Redactor::with_terms(terms));
        *self.active_agent_session.write().await = Some(id);
        self.agent_session_notify.notify_waiters();
        id
    }

    pub async fn leave_agent_session(&self, id: u64) {
        self.agent_sessions.write().await.remove(&id);
        self.session_redactors.lock().await.remove(&id);
        #[cfg(feature = "client")]
        self.pending_sends.lock().await.remove(&id);
        self.conversations_changed();
        {
            let mut current = self.active_agent_session.write().await;
            if *current == Some(id) {
                *current = None;
            }
        }
        self.pending_queries
            .write()
            .await
            .retain(|_, q| !matches!(q.target, QueryTarget::Session(sid) if sid == id));
        self.agent_session_notify.notify_waiters();
    }

    pub async fn set_active_agent_session(&self, id: Option<u64>) {
        *self.active_agent_session.write().await = id;
        self.agent_session_notify.notify_waiters();
    }

    /// Whether the question a conversation waits on has been taken by a
    /// node (it is being written), as opposed to still looking for one.
    pub async fn session_answer_taken(&self, session_id: u64) -> bool {
        self.pending_queries.read().await.values().any(|p| {
            matches!(p.target, QueryTarget::Session(id) if id == session_id) && p.claimed.is_some()
        })
    }

    /// A failure, shown in the conversation in place of an answer.
    pub async fn push_session_error(&self, session_id: u64, message: String) {
        self.push_session_message(session_id, false, message, None, false)
            .await;
        if let Some(m) = self
            .agent_sessions
            .write()
            .await
            .get_mut(&session_id)
            .and_then(|s| s.messages.last_mut())
        {
            m.error = true;
        }
    }

    pub async fn push_session_entry(&self, session_id: u64, from_me: bool, content: String) {
        self.push_session_reply(session_id, from_me, content, None)
            .await;
    }

    pub async fn push_session_reply(
        &self,
        session_id: u64,
        from_me: bool,
        content: String,
        rating: Option<RatingRef>,
    ) {
        self.push_session_message(session_id, from_me, content, rating, false)
            .await;
    }

    pub async fn push_session_message(
        &self,
        session_id: u64,
        from_me: bool,
        content: String,
        rating: Option<RatingRef>,
        private: bool,
    ) {
        if let Some(s) = self.agent_sessions.write().await.get_mut(&session_id) {
            s.messages.push(AgentChatEntry {
                from_me,
                content,
                timestamp: Instant::now(),
                at_ms: now_ms(),
                rating,
                rated: None,
                private,
                vectors: None,
                error: false,
            });
            if !from_me {
                s.awaiting_response = false;
            }
        }
        self.conversations_changed();
        self.agent_session_notify.notify_waiters();
    }

    /// Adds the person's message to a conversation and asks for an answer
    /// with the context chosen on the device. A private message is only
    /// kept. When the context needs the person's confirmation, the question
    /// waits (`confirm_agent_send`, `cancel_agent_send`).
    #[cfg(feature = "client")]
    pub async fn send_to_agent_session(
        &self,
        session_id: u64,
        content: &str,
        private: bool,
    ) -> Result<SendOutcome, String> {
        let kind = self
            .agent_sessions
            .read()
            .await
            .get(&session_id)
            .map(|s| s.kind)
            .ok_or_else(|| "unknown session".to_string())?;
        if self.pending_sends.lock().await.contains_key(&session_id) {
            return Err("a question is waiting for confirmation".into());
        }
        // A second tap on Send while the first was still being prepared (the
        // context is chosen before anything is marked as waiting) sent the
        // same question twice, and two nodes answered it. An answer or an
        // error after the first one means it is a real second try.
        let repeated = self
            .agent_sessions
            .read()
            .await
            .get(&session_id)
            .is_some_and(|s| {
                s.messages.last().is_some_and(|m| {
                    m.from_me
                        && !m.private
                        && m.content == content
                        && now_ms().saturating_sub(m.at_ms) < REPEAT_SEND_WINDOW_MS
                })
            });
        if repeated {
            return Ok(SendOutcome::Sent);
        }
        self.push_session_message(session_id, true, content.to_string(), None, private)
            .await;
        if private {
            return Ok(SendOutcome::Kept);
        }
        let pending = match self
            .plan_context(session_id, kind == AgentSessionKind::Local)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                self.push_session_error(session_id, e.clone()).await;
                return Err(e);
            }
        };
        if pending.plan.needs_confirmation {
            let preview = crate::conversations::ContextPreview::of(&pending.plan);
            self.pending_sends.lock().await.insert(session_id, pending);
            self.agent_session_notify.notify_waiters();
            return Ok(SendOutcome::Confirm(preview));
        }
        self.dispatch_planned(session_id, kind, pending).await?;
        Ok(SendOutcome::Sent)
    }

    /// Sends the waiting question with the older fragments the person kept
    /// (message indices; empty sends none).
    #[cfg(feature = "client")]
    pub async fn confirm_agent_send(&self, session_id: u64, keep: &[usize]) -> Result<(), String> {
        let mut pending = self
            .pending_sends
            .lock()
            .await
            .remove(&session_id)
            .ok_or_else(|| "nothing is waiting".to_string())?;
        let total = pending.older_total;
        pending.plan.keep_only(keep, total);
        let kind = self
            .agent_sessions
            .read()
            .await
            .get(&session_id)
            .map(|s| s.kind)
            .ok_or_else(|| "unknown session".to_string())?;
        self.agent_session_notify.notify_waiters();
        self.dispatch_planned(session_id, kind, pending).await
    }

    /// Drops the waiting question from the conversation and gives its text
    /// back, to edit or send another way.
    #[cfg(feature = "client")]
    pub async fn cancel_agent_send(&self, session_id: u64) -> Result<Option<String>, String> {
        let pending = self
            .pending_sends
            .lock()
            .await
            .remove(&session_id)
            .ok_or_else(|| "nothing is waiting".to_string())?;
        let text = {
            let mut sessions = self.agent_sessions.write().await;
            match sessions.get_mut(&session_id) {
                Some(s) if s.messages.len() == pending.question + 1 => {
                    s.messages.pop().map(|mut m| std::mem::take(&mut m.content))
                }
                _ => None,
            }
        };
        self.conversations_changed();
        self.agent_session_notify.notify_waiters();
        Ok(text)
    }

    #[cfg(feature = "client")]
    async fn dispatch_planned(
        &self,
        session_id: u64,
        kind: AgentSessionKind,
        pending: crate::conversations::PendingSend,
    ) -> Result<(), String> {
        let question = self
            .agent_sessions
            .read()
            .await
            .get(&session_id)
            .and_then(|s| s.messages.get(pending.question))
            .map(|m| m.content.clone())
            .ok_or_else(|| "the question is gone".to_string())?;
        match kind {
            AgentSessionKind::Local => {
                self.dispatch_local_for_session(session_id, &question, &pending.plan)
                    .await
            }
            AgentSessionKind::P2P => {
                self.dispatch_p2p_for_session(session_id, &question, &pending.plan)
                    .await
            }
        }
    }

    /// The thread the question continues, as the model reads it.
    #[cfg(feature = "client")]
    async fn planned_turns(
        &self,
        session_id: u64,
        plan: &nepheral_recall::select::Plan,
    ) -> Result<Vec<(char, String)>, String> {
        let sessions = self.agent_sessions.read().await;
        let s = sessions
            .get(&session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        Ok(plan
            .recent
            .iter()
            .filter_map(|&i| s.messages.get(i))
            .filter(|m| !m.private)
            .map(|m| (if m.from_me { 'u' } else { 'a' }, m.content.clone()))
            .collect())
    }

    #[cfg(feature = "client")]
    async fn dispatch_local_for_session(
        &self,
        session_id: u64,
        prompt: &str,
        plan: &nepheral_recall::select::Plan,
    ) -> Result<(), String> {
        if self.ai_engine.read().await.is_none() {
            self.push_session_error(session_id, "Local engine not initialized.".into())
                .await;
            return Err("ai engine not ready".into());
        }
        let Some((worker, engine)) = self.take_worker().await else {
            self.push_session_error(
                session_id,
                "Local Agent busy — wait for the current turn.".into(),
            )
            .await;
            return Err("busy".into());
        };
        self.local_inferences
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.broadcast_node_stats().await;
        // Nothing leaves the device: the same rules choose the context, with
        // more room, and nothing is redacted.
        let mut system = self.active_system_prompt().await.unwrap_or_default();
        let mut prompt = prompt.to_string();
        match plan.memory_text() {
            Some(m) if plan.summary => prompt = format!("{prompt}\n\n{m}"),
            Some(m) => {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(&m);
            }
            None => {}
        }
        let mut messages = Vec::new();
        if !system.is_empty() {
            messages.push(ai_engine::Message {
                role: ai_engine::MessageRole::System,
                content: system,
                timestamp: 0,
            });
        }
        for (role, c) in self.planned_turns(session_id, plan).await? {
            messages.push(ai_engine::Message {
                role: if role == 'a' {
                    ai_engine::MessageRole::Assistant
                } else {
                    ai_engine::MessageRole::User
                },
                content: c,
                timestamp: 0,
            });
        }
        messages.push(ai_engine::Message {
            role: ai_engine::MessageRole::User,
            content: prompt,
            timestamp: 0,
        });
        if let Some(s) = self.agent_sessions.write().await.get_mut(&session_id) {
            s.awaiting_response = true;
        }
        self.agent_session_notify.notify_waiters();
        let state = self.clone();
        tokio::spawn(async move {
            let response = engine.infer_stateless(messages, None).await;
            state.free_worker(worker).await;
            state
                .local_inferences
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            state.broadcast_node_stats().await;
            match response {
                Ok(r) => state.push_session_entry(session_id, false, r.text).await,
                Err(e) => {
                    state
                        .push_session_error(session_id, format!("Local engine error: {e}"))
                        .await
                }
            }
        });
        Ok(())
    }

    #[cfg(feature = "client")]
    pub async fn dispatch_p2p_for_session(
        &self,
        session_id: u64,
        prompt: &str,
        plan: &nepheral_recall::select::Plan,
    ) -> Result<(), String> {
        let window_raw = self.planned_turns(session_id, plan).await?;
        // A summary's excerpts go in the question itself, which small
        // models follow better than a memory in the system prompt (measured:
        // a 0.5B model refused a summary from its system prompt).
        let (prompt_raw, memory_raw) = match plan.memory_text() {
            Some(m) if plan.summary => (format!("{prompt}\n\n{m}"), None),
            m => (prompt.to_string(), m),
        };
        let prompt = prompt_raw.as_str();
        let system = self.active_system_prompt().await;
        let (prompt_r, window, system_r, memory) = {
            let mut redactors = self.session_redactors.lock().await;
            let r = redactors
                .entry(session_id)
                .or_insert_with(ai_engine::redaction::Redactor::new);
            let window: Vec<AgentTurn> = window_raw
                .iter()
                .map(|(role, c)| AgentTurn {
                    r: *role,
                    c: r.redact(c),
                })
                .collect();
            (
                r.redact(prompt),
                window,
                system.map(|s| r.redact(&s)),
                memory_raw.map(|m| r.redact(&m)),
            )
        };
        {
            if let Some(s) = self.agent_sessions.write().await.get_mut(&session_id) {
                s.awaiting_response = true;
            }
        }
        let result = self
            .dispatch_private_query(PrivateQuery {
                prompt: prompt_r,
                window,
                memory,
                system: system_r,
                target_channel: None,
                session_token: None,
                prefer_large: plan.summary,
                target: QueryTarget::Session(session_id),
            })
            .await;
        if let Err(e) = &result {
            self.push_session_error(session_id, e.clone()).await;
        }
        result.map(|_| ())
    }

    pub async fn ask_mesh(
        &self,
        prompt: &str,
        target_channel: Option<String>,
        session_token: Option<[u8; 32]>,
        deadline: Duration,
    ) -> Result<String, String> {
        let mut redactor = ai_engine::redaction::Redactor::with_terms(
            crate::nepheral_config::load_config()
                .map(|c| c.redact_terms)
                .unwrap_or_default(),
        );
        let redacted = redactor.redact(prompt);
        let (tx, rx) = oneshot::channel();
        let msg_id = self
            .dispatch_private_query(PrivateQuery {
                prompt: redacted,
                window: Vec::new(),
                memory: None,
                system: None,
                target_channel,
                session_token,
                prefer_large: false,
                target: QueryTarget::Oneshot(tx),
            })
            .await?;
        match tokio::time::timeout(deadline, rx).await {
            Ok(Ok(Ok(text))) => Ok(redactor.restore(&text)),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => Err("query abandoned".to_string()),
            Err(_) => {
                self.pending_queries.write().await.remove(&msg_id);
                Err("mesh response timeout".to_string())
            }
        }
    }

    pub async fn eligible_candidates(&self, target_channel: Option<&str>) -> Vec<(PeerId, u8)> {
        let me = *self.my_peer_id.read().await;
        let now = now_ms();
        let chain = self.chain.read().await;
        let height = chain.head_height();
        // Every node known to be alive, reached directly or through relays:
        // a node behind NAT answers as well as any other.
        let caps = self.known_peer_caps().await;
        let candidates: Vec<(PeerId, core_crypto::pallets::reputation::Quality)> = caps
            .iter()
            .filter(|(pid, c)| c.has_ai && **pid != me)
            .map(|(pid, _)| *pid)
            .filter(|pid| chain.contributions.is_lottery_eligible_at(pid, Some(now)))
            .filter(|pid| match target_channel {
                Some(ch) => chain.authority.serves_channel(pid, ch),
                None => true,
            })
            .map(|pid| (pid, chain.reputation.quality(&pid, height)))
            .collect();
        drop(caps);
        // Fresh keys are not handed questions while nodes with a history are
        // there to answer them (sybil.rs).
        let mut candidates = crate::sybil::prefer_tenured(candidates, |(pid, _)| {
            crate::sybil::has_tenure(&chain, pid, now)
        });
        {
            let slow = self.slow_responders.read().await;
            let is_slow = |pid: &PeerId| slow.get(pid).is_some_and(|at| at.elapsed() < SLOW_MEMORY);
            if candidates.iter().any(|(pid, _)| !is_slow(pid)) {
                candidates.retain(|(pid, _)| !is_slow(pid));
            }
        }
        let recent = self.recent_declines.read().await;
        let stats = self.peer_stats.read().await;
        candidates.sort_unstable_by_key(|(pid, _)| *pid);
        candidates.dedup_by(|a, b| a.0 == b.0);
        if candidates.iter().any(|(_, q)| !q.excluded()) {
            candidates.retain(|(_, q)| !q.excluded());
        }
        candidates.sort_unstable_by(|a, b| b.1.percent.cmp(&a.1.percent).then(a.0.cmp(&b.0)));
        candidates.truncate(MAX_CANDIDATES);
        candidates
            .into_iter()
            .map(|(pid, q)| {
                let declined = recent
                    .get(&pid)
                    .is_some_and(|at| at.elapsed() < DECLINE_MEMORY);
                let busy = stats.get(&pid).is_some_and(|s| s.queue_depth > 0);
                (pid, availability_weight(q.routing_weight(), declined, busy))
            })
            .collect()
    }

    pub async fn dispatch_private_query(&self, q: PrivateQuery) -> Result<u64, String> {
        self.dispatch_private_query_attempt(q, 0).await
    }

    async fn dispatch_private_query_attempt(
        &self,
        q: PrivateQuery,
        attempt: u8,
    ) -> Result<u64, String> {
        use ed25519_dalek::Signer;
        use rand::RngCore;

        // Taken before the fields below are moved out of `q`.
        let payload = q.payload();

        let me = *self.my_peer_id.read().await;
        let mut ranked = self.eligible_candidates(q.target_channel.as_deref()).await;
        if q.prefer_large {
            let caps = self.known_peer_caps().await;
            let large: Vec<(PeerId, u8)> = ranked
                .iter()
                .filter(|(pid, _)| {
                    caps.get(pid)
                        .and_then(|c| c.model.as_ref())
                        .is_some_and(|m| m.params_b >= LARGE_MODEL_B)
                })
                .copied()
                .collect();
            if !large.is_empty() {
                ranked = large;
            }
        }
        let candidates: Vec<PeerId> = ranked.iter().map(|(p, _)| *p).collect();
        let candidate_weights: Vec<u8> = ranked.iter().map(|(_, w)| *w).collect();
        if candidates.is_empty() {
            return Err(match &q.target_channel {
                Some(ch) => format!("No node currently serves the '{ch}' channel."),
                None => {
                    "No AI-capable peer is reachable right now — try again shortly.".to_string()
                }
            });
        }
        let mut room_salt = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut room_salt);
        let inputs = LotteryInputs {
            hour_epoch: core_crypto::current_hour_epoch(),
            plaintext_len: q.prompt.len() as u64,
            n_engines: candidates.len() as u64,
            room_salt,
        };
        let wrapped_for: Vec<PeerId> = core_crypto::lottery::rank_candidates_weighted(
            &candidates,
            &candidate_weights,
            &inputs.digest(),
            MAX_CLAIM_RANK as usize + 1,
        )
        .into_iter()
        .copied()
        .collect();

        let query = AgentQuery {
            version: AgentQuery::VERSION,
            prompt: q.prompt,
            window: q.window,
            memory: q.memory,
            system: q.system,
            // Rooms still keep a memory written by the answering node.
            // Conversations choose their context on the device and never
            // ask for one (docs/CONVERSATIONS.md).
            want_memory: matches!(q.target, QueryTarget::Room(_)),
        };
        let mut query_bytes = query.encode();
        let bundle = EncryptedBundle::encrypt(&query_bytes, &wrapped_for)
            .map_err(|e| format!("encrypt: {e:?}"));
        query_bytes.zeroize();
        let bundle = bundle?;
        let bundle_digest = bundle.digest();
        let bundle_bytes = bundle.to_bytes();

        let custodians = self.custodian_set(&wrapped_for).await;
        if custodians.is_empty() {
            return Err("No mesh peers connected — cannot dispatch.".to_string());
        }
        let cfg = core_crypto::ShamirConfig::dynamic_for(custodians.len());
        let shares = core_crypto::ShamirSSS::new(cfg.clone())
            .split_secret(&bundle_bytes)
            .map_err(|e| format!("shamir: {e}"))?;
        let message_id = mesh_network::protocol::random_message_id();
        let vrf_seed = core_crypto::lottery::vrf_seed(message_id, &me, inputs.hour_epoch);
        let sent_at_ms = now_ms();

        self.pending_queries.write().await.insert(
            message_id,
            PendingQuery {
                target: q.target,
                wrapped_for: wrapped_for.clone(),
                bundle_digest,
                sent_at: Instant::now(),
                payload,
                attempt,
                custodians: custodians.clone(),
                claimed: None,
            },
        );

        let min_maturity = *self.composer_min_maturity.read().await;
        let make_push = |share: &core_crypto::ShamirShare| {
            let mut sp = SharePush {
                version: SharePush::VERSION,
                message_id,
                sender_pid: me,
                inputs: inputs.clone(),
                candidates: candidates.clone(),
                candidate_weights: candidate_weights.clone(),
                share_index: share.index,
                total_shares: cfg.total_shares,
                threshold: cfg.threshold,
                share_data: share.value.clone(),
                bundle_digest,
                target_channel: q.target_channel.clone(),
                session_token: q.session_token,
                min_responder_maturity: min_maturity,
                vrf_seed,
                sent_at_ms,
                custodians: custodians.clone(),
                signature: [0u8; 64],
            };
            sp.signature = self.signing_key.sign(&sp.signing_digest()).to_bytes();
            sp
        };
        let direct: HashSet<PeerId> = self
            .live_connections()
            .await
            .into_iter()
            .map(|(_, p, _)| p)
            .collect();
        for share in &shares {
            let Some(dest) = (share.index as usize)
                .checked_sub(1)
                .and_then(|i| custodians.get(i))
                .copied()
            else {
                continue;
            };
            let sp = make_push(share);
            let state = self.clone();
            tokio::spawn(async move {
                state
                    .send_to_peer(dest, message_types::SHARE_PUSH, sp.to_bytes(), message_id)
                    .await;
            });
        }
        {
            let mut stats = self.my_stats.write().await;
            stats.shares_sent += shares.len() as u64;
            stats.lotteries_participated += 1;
        }
        tracing::info!(
            message_id,
            custodians = custodians.len(),
            through_relays = custodians.iter().filter(|c| !direct.contains(*c)).count(),
            threshold = cfg.threshold,
            candidates = candidates.len(),
            "Dispatched private query"
        );

        let state = self.clone();
        let watched = wrapped_for.clone();
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + QUERY_TIMEOUT;
            loop {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                // Every candidate said it was busy. Nothing is coming, so draw
                // again rather than sit out the deadline - by now one of them
                // has probably finished whatever it was doing.
                // The asker hung up. Every further draw would occupy a node with
                // an answer nobody receives - measured: a parallel burst whose
                // API calls had given up at 30s kept re-drawing for another 30,
                // and the next caller's first question collided with that work.
                if state
                    .pending_queries
                    .read()
                    .await
                    .get(&message_id)
                    .is_some_and(|p| p.target.abandoned())
                {
                    state.pending_queries.write().await.remove(&message_id);
                    state.forget_declines(me, message_id).await;
                    tracing::debug!(message_id, "Asker stopped waiting - no further draws");
                    return;
                }
                let stalled = state
                    .pending_queries
                    .read()
                    .await
                    .get(&message_id)
                    .and_then(|p| p.claimed)
                    .filter(|(_, at)| at.elapsed() > STALLED_CLAIM);
                if let Some((slow, _)) = stalled {
                    let Some(p) = state.pending_queries.write().await.remove(&message_id) else {
                        return;
                    };
                    state
                        .slow_responders
                        .write()
                        .await
                        .insert(slow, Instant::now());
                    if p.attempt + 1 >= MAX_QUERY_ATTEMPTS {
                        state
                            .deliver_reply(
                                p.target,
                                Err("No response from the mesh in time.".into()),
                                None,
                                None,
                            )
                            .await;
                        return;
                    }
                    tracing::info!(
                        message_id,
                        "The node that took the question stalled - drawing again without it"
                    );
                    let next = PrivateQuery::from_parts(p.payload, p.target);
                    if let Err(e) = redraw(state.clone(), next, p.attempt + 1).await {
                        tracing::debug!(error = %e, "Re-draw after a stall failed");
                    }
                    return;
                }
                if state
                    .all_candidates_declined(me, message_id, &watched)
                    .await
                {
                    let Some(p) = state.pending_queries.write().await.remove(&message_id) else {
                        return;
                    };
                    if p.attempt + 1 >= MAX_QUERY_ATTEMPTS {
                        state
                            .deliver_reply(
                                p.target,
                                Err("Every AI node was busy. Try again shortly.".into()),
                                None,
                                None,
                            )
                            .await;
                        return;
                    }
                    state.forget_declines(me, message_id).await;
                    let attempt = p.attempt + 1;
                    tracing::info!(message_id, attempt, "Every candidate busy - drawing again");
                    tokio::time::sleep(retry_backoff(p.attempt)).await;
                    let next = PrivateQuery::from_parts(p.payload, p.target);
                    if let Err(e) = redraw(state.clone(), next, attempt).await {
                        tracing::debug!(error = %e, attempt, "Re-draw failed");
                    }
                    return;
                }
                tokio::time::sleep(DECLINE_POLL).await;
            }
            let expired = state.pending_queries.write().await.remove(&message_id);
            if let Some(p) = expired {
                state
                    .deliver_reply(
                        p.target,
                        Err("No response from the mesh in time.".into()),
                        None,
                        None,
                    )
                    .await;
            }
        });
        Ok(message_id)
    }

    /// Who holds the shares of a query: every node this one knows to be
    /// alive, with a model or without, reached directly or through relays.
    /// Custody is what keeps any one node from holding a whole query, so it
    /// is spread as wide as the mesh is. The ranked responders come first, so
    /// the cap can never leave out the nodes that must collect.
    async fn custodian_set(&self, ranked: &[PeerId]) -> Vec<PeerId> {
        use rand::seq::SliceRandom;
        let me = *self.my_peer_id.read().await;
        let mut known: HashSet<PeerId> = self.known_peer_caps().await.into_keys().collect();
        known.extend(self.live_connections().await.into_iter().map(|(_, p, _)| p));
        known.remove(&me);
        let ranked: Vec<PeerId> = ranked.iter().copied().filter(|p| known.remove(p)).collect();
        // Fresh keys are capped against established ones (sybil.rs).
        let chain = self.chain.read().await;
        let (mut established, mut fresh): (Vec<PeerId>, Vec<PeerId>) = known
            .into_iter()
            .partition(|p| crate::sybil::is_established(&chain, p));
        let mut rng = rand::thread_rng();
        established.shuffle(&mut rng);
        fresh.shuffle(&mut rng);
        crate::sybil::choose_custodians(&ranked, established, fresh, |p| {
            crate::sybil::is_established(&chain, p)
        })
    }

    /// A claim from a node this one has no direct connection to. Answered
    /// with the asker's signed push rather than a bare share: the answer
    /// crosses relays, and the claimant has to be able to check it.
    pub async fn handle_relayed_claim(&self, claim: AgentClaim) {
        self.observe_claim(&claim).await;
        let claimant = claim.claimant_pid;
        let key = (claim.sender_pid, claim.message_id);
        if self.process_share_claim(claim, claimant).await.is_none() {
            return;
        }
        let Some(push) = self.custody.read().await.get(&key).map(|s| s.push.clone()) else {
            return;
        };
        self.send_to_peer(
            claimant,
            message_types::SHARE_RETURN,
            push.to_bytes(),
            key.1,
        )
        .await;
    }

    /// A custodian's answer to a relayed claim. The asker's signature over
    /// the push is what makes the share trustworthy, whoever carried it.
    pub async fn handle_share_return(&self, sp: SharePush) -> Result<(), String> {
        sp.validate()?;
        self.collect_share_release((sp.sender_pid, sp.message_id), sp.release())
            .await
    }

    async fn deliver_reply(
        &self,
        target: QueryTarget,
        result: Result<String, String>,
        memory: Option<String>,
        rating: Option<RatingRef>,
    ) {
        match target {
            QueryTarget::Session(id) => {
                drop(memory);
                let (text, failed) = match result {
                    Ok(t) => (t, false),
                    Err(e) => (e, true),
                };
                let restored = {
                    let redactors = self.session_redactors.lock().await;
                    match redactors.get(&id) {
                        Some(r) => r.restore(&text),
                        None => ai_engine::redaction::plain_slots(&text),
                    }
                };
                if failed {
                    self.push_session_error(id, restored).await;
                } else {
                    self.push_session_reply(id, false, restored, rating).await;
                }
            }
            QueryTarget::Room(code) => {
                self.deliver_room_agent_reply(&code, result, memory, rating)
                    .await;
            }
            QueryTarget::Oneshot(tx) => {
                let _ = tx.send(result);
            }
        }
    }

    pub async fn process_share_push(&self, sp: SharePush, from_pid: PeerId) -> Result<(), String> {
        sp.validate()?;
        if sp.sent_at_ms.abs_diff(now_ms()) > SHARE_PUSH_MAX_SKEW_MS {
            return Err("SharePush is stale or from the future".into());
        }
        let key = (sp.sender_pid, sp.message_id);
        let fresh = {
            let mut custody = self.custody.write().await;
            if custody.contains_key(&key) {
                false
            } else if custody.len() >= CUSTODY_CAP {
                return Err("custody full".into());
            } else {
                custody.insert(
                    key,
                    StoredAgentShare {
                        push: sp.clone(),
                        stored_at: Instant::now(),
                        releases: Vec::new(),
                    },
                );
                true
            }
        };
        if fresh {
            tracing::debug!(
                from = %hex::encode(&from_pid[..4]),
                sender = %hex::encode(&sp.sender_pid[..4]),
                "Holding a share in custody"
            );
            let early = self.early_claims.write().await.remove(&key);
            if let Some((_, claims)) = early {
                for claim in claims {
                    self.observe_claim(&claim).await;
                }
            }
            self.maybe_start_claim(sp).await;
        }
        Ok(())
    }

    async fn may_serve_channel(&self, sp: &SharePush) -> bool {
        let Some(channel) = sp.target_channel.as_deref() else {
            return true;
        };
        let Some(token) = sp.session_token else {
            return false;
        };
        let me = *self.my_peer_id.read().await;
        let remaining = {
            let chain = self.chain.read().await;
            if !chain.authority.serves_channel(&me, channel) {
                return false;
            }
            if chain.nai.session_expired(&token, now_ms()) {
                return false;
            }
            let Some(session) = chain.nai.session(&token) else {
                return false;
            };
            // Paid by the asker, or by the wallet the asker works for.
            if session.owner != chain.account_of(&sp.sender_pid) && session.owner != sp.sender_pid {
                return false;
            }
            if session.channel.as_deref().is_some_and(|c| c != channel) {
                return false;
            }
            session.inferences_remaining
        };
        let mut served = self.served_sessions.lock().await;
        let used = served.entry(token).or_insert(0);
        if *used >= remaining {
            return false;
        }
        *used += 1;
        if served.len() > CUSTODY_CAP {
            served.clear();
        }
        true
    }

    pub async fn maybe_start_claim(&self, sp: SharePush) {
        let me = *self.my_peer_id.read().await;
        let wrapped_for = sp.wrapped_for();
        let Some(rank) = wrapped_for.iter().position(|p| *p == me) else {
            // Not a candidate for this query at all: nothing to claim or decline.
            return;
        };
        let rank = rank as u8;
        // A candidate that cannot serve says so at once. Returning quietly
        // left the ranks behind it waiting out their delays, and kept the
        // asker from ever seeing the full set of declines it retries on.
        let serveable = self.am_i_ai_capable().await
            && self.may_serve_channel(&sp).await
            && match sp.min_responder_maturity {
                Some(threshold) => {
                    self.chain.read().await.contributions.maturity_of(&me) >= threshold
                }
                None => true,
            };
        if !serveable {
            self.broadcast_decline(sp.sender_pid, sp.message_id, rank)
                .await;
            return;
        }
        let state = self.clone();
        tokio::spawn(async move {
            let key = (sp.sender_pid, sp.message_id);
            let wrapped = sp.wrapped_for();
            let delay = rank_delay(rank);
            if !delay.is_zero() {
                let deadline = tokio::time::Instant::now() + delay;
                loop {
                    if state.claimed_by_lower_rank(key, rank).await {
                        return;
                    }
                    // Nobody ahead is going to answer, so there is nothing
                    // left to wait for.
                    if state.lower_ranks_all_declined(key, rank, &wrapped).await {
                        break;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(DECLINE_POLL).await;
                }
                if state.claimed_by_lower_rank(key, rank).await {
                    return;
                }
            }
            // A claim is a promise: the ranks behind this one stand down the
            // moment they see it. So it takes the slot now, not when inference
            // starts. Without this a node claimed every query it ranked first
            // in, was still reconstructing the first when the second's shares
            // arrived, and then declined the second after nobody was left to
            // pick it up - having already decrypted its plaintext.
            if state.all_workers_busy().await || !state.reserve_slot(key).await {
                state
                    .broadcast_decline(sp.sender_pid, sp.message_id, rank)
                    .await;
                return;
            }
            state.broadcast_claim(sp, rank).await;
        });
    }

    pub async fn broadcast_claim(&self, sp: SharePush, rank: u8) {
        use ed25519_dalek::Signer;
        let me = *self.my_peer_id.read().await;
        let (_, vrf_proof) = core_crypto::lottery::vrf_evaluate(&self.signing_key, &sp.vrf_seed);
        let mut claim = AgentClaim {
            version: AgentClaim::VERSION,
            message_id: sp.message_id,
            sender_pid: sp.sender_pid,
            claimant_pid: me,
            inputs: sp.inputs.clone(),
            rank,
            vrf_proof,
            signature: [0u8; 64],
        };
        claim.signature = self.signing_key.sign(&claim.signing_digest()).to_bytes();
        let key = (sp.sender_pid, sp.message_id);
        {
            let mut releases = self.custody_releases.write().await;
            if releases.len() >= CUSTODY_CAP {
                releases.retain(|_, (at, _)| at.elapsed() <= COLLECTOR_TTL);
            }
            releases.insert(key, (Instant::now(), std::collections::BTreeSet::new()));
        }
        self.agent_claim_collectors.write().await.insert(
            key,
            ClaimCollector {
                push: sp.clone(),
                rank,
                shares: HashMap::new(),
                started_at: Instant::now(),
            },
        );
        let own = self.custody.read().await.get(&key).map(|s| s.push.clone());
        if let Some(own) = own {
            let sr = ShareRelease {
                version: 2,
                message_id: own.message_id,
                sender_pid: own.sender_pid,
                share_index: own.share_index,
                total_shares: own.total_shares,
                threshold: own.threshold,
                share_data: own.share_data,
            };
            let _ = self.collect_share_release(key, sr).await;
        }
        // Custodians with no direct connection are claimed from through the
        // relays, and answer with SHARE_RETURN.
        let live = self.live_connections().await;
        let direct: HashSet<PeerId> = live.iter().map(|(_, p, _)| *p).collect();
        let raw_claim = claim.to_bytes();
        // The asker learns its question was taken: directly through the
        // broadcast below, or through the relays. It holds no share, so it
        // only notes it (`observe_claim`).
        let asker_relayed = (sp.sender_pid != me
            && !direct.contains(&sp.sender_pid)
            && !sp.custodians.contains(&sp.sender_pid))
        .then_some(sp.sender_pid);
        for custodian in sp
            .custodians
            .iter()
            .filter(|c| **c != me && !direct.contains(*c))
            .chain(asker_relayed.iter())
        {
            let state = self.clone();
            let dest = *custodian;
            let bytes = raw_claim.clone();
            let message_id = sp.message_id;
            tokio::spawn(async move {
                state
                    .send_to_peer(dest, message_types::SHARE_CLAIM_RELAYED, bytes, message_id)
                    .await;
            });
        }
        let claim_bytes = Arc::new(frame(message_types::SHARE_CLAIM, &raw_claim));
        for (addr, _, conn) in live {
            let bytes = claim_bytes.clone();
            let state = self.clone();
            tokio::spawn(async move {
                let result: Result<Option<ShareRelease>, String> = async {
                    let (mut send, mut recv) =
                        tokio::time::timeout(Duration::from_secs(5), conn.open_bi())
                            .await
                            .map_err(|_| "open timeout".to_string())?
                            .map_err(|e| e.to_string())?;
                    send.write_all(&bytes).await.map_err(|e| e.to_string())?;
                    send.finish().map_err(|e| e.to_string())?;
                    tokio::time::timeout(Duration::from_secs(30), async {
                        let mut len_buf = [0u8; 4];
                        if recv.read_exact(&mut len_buf).await.is_err() {
                            return Ok(None);
                        }
                        let len = u32::from_be_bytes(len_buf) as usize;
                        if len == 0 || len > mesh_network::protocol::MAX_SHARE_RELEASE_BYTES {
                            return Ok(None);
                        }
                        let mut body = vec![0u8; len];
                        recv.read_exact(&mut body)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok(ShareRelease::from_bytes(&body))
                    })
                    .await
                    .map_err(|_| "release timeout".to_string())?
                }
                .await;
                match result {
                    Ok(Some(sr)) => {
                        let _ = state.collect_share_release(key, sr).await;
                    }
                    Ok(None) => {}
                    Err(e) => tracing::debug!(peer = %addr, error = %e, "claim exchange failed"),
                }
            });
        }
    }

    pub async fn process_share_claim(
        &self,
        claim: AgentClaim,
        from_pid: PeerId,
    ) -> Option<ShareRelease> {
        if claim.version != AgentClaim::VERSION || claim.claimant_pid != from_pid {
            return None;
        }
        if claim.rank > MAX_CLAIM_RANK || !claim.verify_signature() {
            return None;
        }
        let key = (claim.sender_pid, claim.message_id);
        let decliners = self
            .observed_declines
            .read()
            .await
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let mut custody = self.custody.write().await;
        let stored = custody.get_mut(&key)?;
        if stored.stored_at.elapsed() > AGENT_SHARE_TTL || claim.inputs != stored.push.inputs {
            return None;
        }
        let wrapped_for = stored.push.wrapped_for();
        if wrapped_for.get(claim.rank as usize) != Some(&claim.claimant_pid) {
            return None;
        }
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&claim.claimant_pid).ok()?;
        core_crypto::lottery::vrf_verify(&vk, &stored.push.vrf_seed, &claim.vrf_proof)?;
        // The rank delay stops a later rank from jumping the queue. It is
        // waived only when everyone ahead has verifiably stood down - the same
        // test the claimant used to start early. Without the waiver an early
        // claimant got no shares, sat on its reserved slot until the TTL, and
        // the query died with capacity to spare.
        let tolerance = Duration::from_secs(1);
        let ahead_declined = ranks_ahead_declined(&decliners, claim.rank, &wrapped_for);
        if !ahead_declined && stored.stored_at.elapsed() + tolerance < rank_delay(claim.rank) {
            return None;
        }
        if stored.releases.iter().any(|(r, _)| *r == claim.rank) {
            return None;
        }
        if stored
            .releases
            .iter()
            .any(|(r, at)| *r < claim.rank && at.elapsed() < LOWER_RANK_GRACE)
        {
            return None;
        }
        stored.releases.push((claim.rank, Instant::now()));
        let push = stored.push.clone();
        drop(custody);
        self.note_observed_claim(key, claim.rank).await;
        Some(ShareRelease {
            version: 2,
            message_id: push.message_id,
            sender_pid: push.sender_pid,
            share_index: push.share_index,
            total_shares: push.total_shares,
            threshold: push.threshold,
            share_data: push.share_data,
        })
    }

    /// Takes this node's single serving slot for `key`. Each node answers one
    /// mesh query at a time, so a second claim while one is held would be a
    /// promise it cannot keep. A reservation older than `RESERVATION_TTL` is
    /// treated as abandoned, which bounds the damage of any exit that forgets
    /// to release.
    pub async fn reserve_slot(&self, key: (PeerId, u64)) -> bool {
        let workers = self.worker_count().await;
        let local = self
            .local_inferences
            .load(std::sync::atomic::Ordering::Relaxed);
        let mut slots = self.claim_reservation.lock().await;
        slots.retain(|(_, at, started)| {
            at.elapsed()
                < if *started {
                    SERVING_TTL
                } else {
                    RESERVATION_TTL
                }
        });
        if slots.iter().any(|(held, _, _)| *held == key) {
            return true;
        }
        if slots.len() + local >= workers {
            return false;
        }
        slots.push((key, Instant::now(), false));
        true
    }

    /// A worker has started on `key`: its slot no longer lapses after
    /// `RESERVATION_TTL`, only after `SERVING_TTL`.
    async fn mark_slot_started(&self, key: (PeerId, u64)) {
        let mut slots = self.claim_reservation.lock().await;
        if let Some(s) = slots.iter_mut().find(|(held, _, _)| *held == key) {
            s.1 = Instant::now();
            s.2 = true;
        }
    }

    pub async fn release_slot(&self, key: (PeerId, u64)) {
        self.claim_reservation
            .lock()
            .await
            .retain(|(held, _, _)| *held != key);
    }

    /// True when a candidate ahead of `rank` has already claimed the query.
    pub async fn claimed_by_lower_rank(&self, key: (PeerId, u64), rank: u8) -> bool {
        self.observed_agent_claims
            .read()
            .await
            .get(&key)
            .copied()
            .is_some_and(|r| r < rank)
    }

    /// True when every candidate ahead of `rank` has declined, so waiting out
    /// the rest of the rank delay would buy nothing. The claimant and every
    /// custodian ask this same question, so they agree on when the delay may
    /// be skipped.
    pub async fn lower_ranks_all_declined(
        &self,
        key: (PeerId, u64),
        rank: u8,
        wrapped_for: &[PeerId],
    ) -> bool {
        let declines = self.observed_declines.read().await;
        let Some(set) = declines.get(&key) else {
            return false;
        };
        ranks_ahead_declined(set, rank, wrapped_for)
    }

    /// True when every candidate this query was wrapped for has declined.
    pub async fn all_candidates_declined(
        &self,
        me: PeerId,
        message_id: u64,
        wrapped_for: &[PeerId],
    ) -> bool {
        if wrapped_for.is_empty() {
            return false;
        }
        let declines = self.observed_declines.read().await;
        let Some(set) = declines.get(&(me, message_id)) else {
            return false;
        };
        wrapped_for.iter().all(|p| set.contains(p))
    }

    /// Clears a query's declines before it is drawn again, so the stale set
    /// does not immediately satisfy the next round.
    pub async fn forget_declines(&self, me: PeerId, message_id: u64) {
        self.observed_declines
            .write()
            .await
            .remove(&(me, message_id));
    }

    /// Records that `decliner` gave up its place for this query. Callers pass
    /// the authenticated identity of whoever sent the decline, never a field
    /// out of the message.
    pub async fn note_observed_decline(&self, key: (PeerId, u64), decliner: PeerId) {
        tracing::debug!(
            message_id = key.1,
            decliner = %hex::encode(&decliner[..4]),
            "Observed a decline"
        );
        {
            let mut recent = self.recent_declines.write().await;
            if recent.len() >= CUSTODY_CAP {
                recent.retain(|_, at| at.elapsed() < DECLINE_MEMORY);
            }
            recent.insert(decliner, Instant::now());
        }
        let mut declines = self.observed_declines.write().await;
        if declines.len() >= CUSTODY_CAP && !declines.contains_key(&key) {
            return;
        }
        declines.entry(key).or_default().insert(decliner);
    }

    /// Tells the other candidates this node will not be answering. Carries no
    /// share and no plaintext. Receivers attribute it to the TLS peer that
    /// sent it, so a node can give up only its own place in the queue.
    pub async fn broadcast_decline(&self, sender_pid: PeerId, message_id: u64, rank: u8) {
        tracing::info!(message_id, rank, "Declining mesh query");
        // This node is usually also a custodian for the same query, and it
        // does not receive its own broadcast - record it here too.
        let me = *self.my_peer_id.read().await;
        self.note_observed_decline((sender_pid, message_id), me)
            .await;
        let decline = mesh_network::protocol::AgentDecline {
            version: 1,
            sender_pid,
            message_id,
            rank,
        };
        self.broadcast(message_types::AGENT_DECLINE, &decline.to_bytes(), None)
            .await;
        // The broadcast reaches direct connections only. The other ranked
        // candidates, and the asker, get it signed through the relays.
        let Some(wrapped) = self
            .custody
            .read()
            .await
            .get(&(sender_pid, message_id))
            .map(|s| s.push.wrapped_for())
        else {
            return;
        };
        let direct: HashSet<PeerId> = self
            .live_connections()
            .await
            .into_iter()
            .map(|(_, p, _)| p)
            .collect();
        let signed = mesh_network::protocol::SignedDecline::sign(
            &self.signing_key,
            sender_pid,
            message_id,
            rank,
        )
        .to_bytes();
        let mut targets: Vec<PeerId> = wrapped;
        targets.push(sender_pid);
        targets.sort_unstable();
        targets.dedup();
        for dest in targets
            .into_iter()
            .filter(|p| *p != me && !direct.contains(p))
        {
            let state = self.clone();
            let bytes = signed.clone();
            tokio::spawn(async move {
                state
                    .send_to_peer(dest, message_types::SIGNED_DECLINE, bytes, message_id)
                    .await;
            });
        }
    }

    /// A decline that arrived signed, directly or through the relays. It
    /// counts for the candidate that signed it.
    pub async fn handle_signed_decline(&self, d: mesh_network::protocol::SignedDecline) {
        if d.verify() {
            self.note_observed_decline((d.sender_pid, d.message_id), d.decliner)
                .await;
        }
    }

    pub async fn note_observed_claim(&self, key: (PeerId, u64), rank: u8) {
        let mut observed = self.observed_agent_claims.write().await;
        if observed.len() >= CUSTODY_CAP && !observed.contains_key(&key) {
            return;
        }
        observed
            .entry(key)
            .and_modify(|r| *r = (*r).min(rank))
            .or_insert(rank);
    }

    pub async fn observe_claim(&self, claim: &AgentClaim) {
        if claim.rank > MAX_CLAIM_RANK || !claim.verify_signature() {
            return;
        }
        // A claim on this node's own question: it is being answered.
        if claim.sender_pid == *self.my_peer_id.read().await {
            let taken = {
                let mut pending = self.pending_queries.write().await;
                match pending.get_mut(&claim.message_id) {
                    Some(p)
                        if p.claimed.is_none()
                            && p.wrapped_for.get(claim.rank as usize)
                                == Some(&claim.claimant_pid) =>
                    {
                        p.claimed = Some((claim.claimant_pid, Instant::now()));
                        true
                    }
                    _ => false,
                }
            };
            if taken {
                self.agent_session_notify.notify_waiters();
            }
            return;
        }
        let key = (claim.sender_pid, claim.message_id);
        let wrapped = self
            .custody
            .read()
            .await
            .get(&key)
            .map(|s| s.push.wrapped_for());
        match wrapped {
            Some(w) => {
                if w.get(claim.rank as usize) == Some(&claim.claimant_pid) {
                    self.note_observed_claim(key, claim.rank).await;
                }
            }
            // Through the relays a claim can overtake the push it refers to;
            // dropping it left this node waiting for a rank that had already
            // claimed. Kept until the push arrives (process_share_push).
            None => {
                let mut early = self.early_claims.write().await;
                early.retain(|_, (at, _)| at.elapsed() < EARLY_CLAIM_TTL);
                if early.len() < CUSTODY_CAP || early.contains_key(&key) {
                    let (_, claims) = early
                        .entry(key)
                        .or_insert_with(|| (Instant::now(), Vec::new()));
                    if claims.len() < MAX_CLAIM_RANK as usize + 1 {
                        claims.push(claim.clone());
                    }
                }
            }
        }
    }

    pub async fn collect_share_release(
        &self,
        key: (PeerId, u64),
        sr: ShareRelease,
    ) -> Result<(), String> {
        if let Some((_, seen)) = self.custody_releases.write().await.get_mut(&key) {
            if sr.share_index != 0 && sr.share_index <= sr.total_shares {
                seen.insert(sr.share_index);
            }
        }
        let ready = {
            let mut collectors = self.agent_claim_collectors.write().await;
            let Some(c) = collectors.get_mut(&key) else {
                return Ok(());
            };
            if sr.message_id != c.push.message_id
                || sr.sender_pid != c.push.sender_pid
                || sr.total_shares != c.push.total_shares
                || sr.threshold != c.push.threshold
                || sr.share_index == 0
                || sr.share_index > sr.total_shares
            {
                return Ok(());
            }
            c.shares.entry(sr.share_index).or_insert(sr.share_data);
            if c.shares.len() < c.push.threshold as usize {
                return Ok(());
            }
            collectors.remove(&key)
        };
        let Some(collected) = ready else {
            return Ok(());
        };
        // A worker is taken before the bundle is opened: a local chat may
        // have started since the claim, and plaintext this node will not
        // serve should never be decrypted on it.
        let Some((worker, engine)) = self.take_worker().await else {
            self.broadcast_decline(key.0, key.1, collected.rank).await;
            self.release_slot(key).await;
            return Ok(());
        };
        self.mark_slot_started(key).await;
        let me = *self.my_peer_id.read().await;
        let Some(plaintext) = reconstruct_and_open(&collected, &me, &self.signing_key) else {
            tracing::warn!("agent bundle could not be reconstructed from the released shares");
            self.free_worker(worker).await;
            self.release_slot(key).await;
            return Ok(());
        };
        let state = self.clone();
        tokio::spawn(async move {
            state
                .run_inference_then_reply(collected, plaintext, worker, engine)
                .await;
        });
        Ok(())
    }

    /// Serves a reconstructed query and gives the worker and the slot back
    /// afterwards, whichever way serving ends. The body has half a dozen
    /// early exits; releasing in one place after all of them is what keeps a
    /// missed exit from wedging the node.
    async fn run_inference_then_reply(
        &self,
        collected: ClaimCollector,
        plaintext: Vec<u8>,
        worker: usize,
        engine: Arc<ai_engine::InferenceEngine>,
    ) {
        let key = (collected.push.sender_pid, collected.push.message_id);
        self.serve_collected(collected, plaintext, engine).await;
        self.free_worker(worker).await;
        self.broadcast_node_stats().await;
        self.release_slot(key).await;
    }

    async fn serve_collected(
        &self,
        collected: ClaimCollector,
        mut plaintext: Vec<u8>,
        engine: Arc<ai_engine::InferenceEngine>,
    ) {
        let query = AgentQuery::decode(&plaintext);
        plaintext.zeroize();
        let Some(query) = query else {
            tracing::warn!("agent query malformed");
            return;
        };
        self.broadcast_node_stats().await;
        let sp = &collected.push;
        let model_name = self
            .my_model_info()
            .await
            .map(|m| display_model_name(&m.name));
        let mut messages = build_responder_messages(&query, model_name.as_deref());
        // The owner's "think before answering" is for their own chats: an
        // answer for the mesh is written straight away.
        let answer = engine
            .infer_stateless_with(messages.clone(), Some(0.3), Some(false))
            .await;
        let memory = match (&answer, query.want_memory) {
            (Ok(reply), true) => {
                let mut mem_messages = vec![ai_engine::Message {
                    role: ai_engine::MessageRole::System,
                    content: MEMORY_INSTRUCTION.to_string(),
                    timestamp: 0,
                }];
                let previous = query
                    .memory
                    .clone()
                    .unwrap_or_else(|| "(empty)".to_string());
                mem_messages.push(ai_engine::Message {
                    role: ai_engine::MessageRole::User,
                    content: format!(
                        "Previous memory:\n{previous}\n\nLatest user message:\n{}\n\nLatest answer:\n{}",
                        query.prompt, reply.text
                    ),
                    timestamp: 0,
                });
                engine
                    .infer_stateless_with(mem_messages, Some(0.1), Some(false))
                    .await
                    .ok()
                    .map(|r| r.text.chars().take(MEMORY_MAX_CHARS).collect::<String>())
            }
            _ => None,
        };
        for m in messages.iter_mut() {
            m.content.zeroize();
        }
        let mut reply = match answer {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "inference for a mesh query failed");
                return;
            }
        };
        let cleaned = strip_stray_cjk(&reply.text, &query.prompt);
        reply.text.zeroize();
        reply.text = cleaned;

        let mut reply_bytes = core_crypto::wire::encode(&AgentReply {
            text: reply.text.clone(),
            memory,
        });
        let sealed = core_crypto::agent_crypto::seal_to_peer(&sp.sender_pid, &reply_bytes);
        reply_bytes.zeroize();
        let Ok(sealed) = sealed else {
            return;
        };
        let model = self.my_model_info().await;
        let params_b = model.as_ref().map(|m| m.params_b).unwrap_or(1);
        let model_hash = model.as_ref().map(|m| m.hash).unwrap_or([0u8; 32]);
        let service_claim = core_crypto::receipts::ServiceClaim::sign(
            &self.signing_key,
            sp.bundle_digest,
            sp.sender_pid,
            core_crypto::current_hour_epoch(),
            model_hash,
            params_b,
            reply.output_tokens.min(u32::MAX as usize) as u32,
            reply.gen_ms.min(u32::MAX as u64) as u32,
        );
        let me = *self.my_peer_id.read().await;
        // Every custodian whose share reached this node, the late ones
        // included: holding is what is being paid for. Shares that travel
        // through relays can trail the answer when inference is quick, so
        // give them until CUSTODY_GRACE after the claim; a real model takes
        // longer than that on its own.
        let key = (sp.sender_pid, sp.message_id);
        loop {
            let settled = match self.custody_releases.read().await.get(&key) {
                Some((at, seen)) => {
                    seen.len() >= sp.custodians.len() || at.elapsed() >= CUSTODY_GRACE
                }
                None => true,
            };
            if settled {
                break;
            }
            tokio::time::sleep(DECLINE_POLL).await;
        }
        let custodians: Vec<PeerId> = self
            .custody_releases
            .write()
            .await
            .remove(&(sp.sender_pid, sp.message_id))
            .map(|(_, indexes)| {
                indexes
                    .into_iter()
                    .filter_map(|i| sp.custodians.get((i as usize).checked_sub(1)?).copied())
                    .filter(|p| *p != me && *p != sp.sender_pid)
                    .take(core_crypto::receipts::MAX_CUSTODY_PER_RECEIPT)
                    .collect()
            })
            .unwrap_or_default();
        let custody = (!custodians.is_empty()).then(|| {
            core_crypto::receipts::CustodyAttestation::sign(
                &service_claim,
                custodians,
                &self.signing_key,
            )
        });
        let mut resp = AgentResponseDirect {
            version: AgentResponseDirect::VERSION,
            query_message_id: sp.message_id,
            query_digest: sp.bundle_digest,
            responder_pid: me,
            sealed,
            service_claim,
            custody,
            signature: [0u8; 64],
        };
        {
            use ed25519_dalek::Signer;
            resp.signature = self.signing_key.sign(&resp.signing_digest()).to_bytes();
        }
        let bytes = resp.to_bytes();
        match self.connection_for_peer(&sp.sender_pid).await {
            Some((_, conn)) => {
                if let Err(e) =
                    send_frame(&conn, &frame(message_types::AGENT_RESPONSE_DIRECT, &bytes)).await
                {
                    tracing::warn!(error = %e, "agent response send failed");
                }
            }
            None => {
                let sealed_ok = self
                    .known_peer_caps()
                    .await
                    .get(&sp.sender_pid)
                    .is_some_and(|c| c.sealed_replies);
                if sealed_ok {
                    self.relay_send_sealed(
                        sp.sender_pid,
                        message_types::AGENT_RESPONSE_DIRECT,
                        &bytes,
                        sp.message_id,
                    )
                    .await;
                } else {
                    self.relay_send(
                        sp.sender_pid,
                        message_types::AGENT_RESPONSE_DIRECT,
                        bytes,
                        sp.message_id,
                    )
                    .await;
                }
            }
        }
        let mut stats = self.my_stats.write().await;
        stats.lotteries_won += 1;
        stats.lotteries_participated += 1;
        stats.inference_ms_total += reply.gen_ms;
    }

    pub async fn process_agent_response(
        &self,
        resp: AgentResponseDirect,
        raw: &[u8],
        from_addr: SocketAddr,
    ) -> Result<(), String> {
        let me = *self.my_peer_id.read().await;
        let is_mine = self
            .pending_queries
            .read()
            .await
            .contains_key(&resp.query_message_id);
        if !is_mine {
            self.maybe_relay_agent_response(resp.query_message_id, raw, from_addr)
                .await;
            return Ok(());
        }
        resp.verify()?;
        let pending = {
            let mut pending = self.pending_queries.write().await;
            let ok = pending.get(&resp.query_message_id).is_some_and(|p| {
                p.bundle_digest == resp.query_digest && p.wrapped_for.contains(&resp.responder_pid)
            });
            if !ok {
                return Err(
                    "response does not match the query or comes from an unelected node".into(),
                );
            }
            pending.remove(&resp.query_message_id)
        };
        let Some(pending) = pending else {
            return Ok(());
        };
        if resp.service_claim.sender_pid != me {
            return Err("service claim names another sender".into());
        }
        let mut opened = core_crypto::agent_crypto::open_from_peer(&self.signing_key, &resp.sealed)
            .map_err(|e| format!("decrypt failed: {e:?}"))?;
        let reply: Option<AgentReply> = core_crypto::wire::decode(&opened);
        opened.zeroize();
        let reply = reply.ok_or_else(|| "reply malformed".to_string())?;
        tracing::info!(
            latency_ms = pending.sent_at.elapsed().as_millis() as u64,
            responder = %hex::encode(&resp.responder_pid[..4]),
            "Private query answered"
        );
        let rating = RatingRef {
            query_digest: resp.service_claim.query_digest,
            responder: resp.service_claim.responder_pid,
        };
        self.deliver_reply(pending.target, Ok(reply.text), reply.memory, Some(rating))
            .await;
        // From rules v2 the chain refuses a receipt between two nodes of one
        // owner; sending it would only spend a nonce on a rejection.
        let same_owner = {
            let chain = self.chain.read().await;
            chain.rules_version() >= core_crypto::pallets::upgrade::RULES_V2
                && chain.account_of(&resp.service_claim.sender_pid)
                    == chain.account_of(&resp.service_claim.responder_pid)
        };
        if same_owner {
            return Ok(());
        }
        let receipt = core_crypto::receipts::ServiceReceipt::countersign(
            resp.service_claim,
            &self.signing_key,
        );
        use core_crypto::pallets::contributions::ContributionsCall;
        // Credited only if every name on it is a custodian this node chose:
        // the responder cannot pay anyone it likes from this receipt.
        let call = match resp.custody {
            Some(custody)
                if custody
                    .custodians
                    .iter()
                    .all(|p| pending.custodians.contains(p)) =>
            {
                ContributionsCall::SubmitReceiptWithCustody {
                    receipt: Box::new(receipt),
                    custody,
                }
            }
            _ => ContributionsCall::SubmitReceipt {
                receipt: Box::new(receipt),
            },
        };
        self.sign_and_submit(core_crypto::extrinsic::PALLET_CONTRIBUTIONS, &call)
            .await;
        Ok(())
    }

    pub async fn sweep_agent_state(&self) {
        self.custody
            .write()
            .await
            .retain(|_, s| s.stored_at.elapsed() <= AGENT_SHARE_TTL);
        let live: HashSet<(PeerId, u64)> = self.custody.read().await.keys().copied().collect();
        self.observed_agent_claims
            .write()
            .await
            .retain(|k, _| live.contains(k));
        self.agent_claim_collectors
            .write()
            .await
            .retain(|_, c| c.started_at.elapsed() <= COLLECTOR_TTL);
        self.relay_routes
            .write()
            .await
            .retain(|_, r| r.created_at.elapsed() <= RELAY_ROUTE_TTL);
        self.custody_releases
            .write()
            .await
            .retain(|_, (at, _)| at.elapsed() <= COLLECTOR_TTL);
        let stale: Vec<u64> = self
            .pending_queries
            .read()
            .await
            .iter()
            .filter(|(_, q)| q.sent_at.elapsed() > QUERY_TIMEOUT * 2)
            .map(|(id, _)| *id)
            .collect();
        let mut pending = self.pending_queries.write().await;
        for id in stale {
            pending.remove(&id);
        }
    }

    pub async fn run_agent_sweeper(self) {
        loop {
            if self.sleep_or_shutdown(Duration::from_secs(30)).await {
                break;
            }
            self.sweep_agent_state().await;
        }
    }
}

fn build_responder_messages(query: &AgentQuery, model: Option<&str>) -> Vec<ai_engine::Message> {
    use ai_engine::{Message, MessageRole};
    let mut system = RESPONDER_INSTRUCTION.to_string();
    if let Some(m) = model {
        system.push(' ');
        system.push_str(&identity_line(m));
    }
    if carries_placeholders(query) {
        system.push(' ');
        system.push_str(PLACEHOLDER_RULE);
    }
    if let Some(s) = &query.system {
        system.push_str("\n\n");
        system.push_str(s);
    }
    if let Some(m) = &query.memory {
        system.push_str("\n\nConversation memory:\n");
        system.push_str(m);
    }
    let mut out = vec![Message {
        role: MessageRole::System,
        content: system,
        timestamp: 0,
    }];
    for t in &query.window {
        out.push(Message {
            role: if t.r == 'a' {
                MessageRole::Assistant
            } else {
                MessageRole::User
            },
            content: t.c.clone(),
            timestamp: 0,
        });
    }
    out.push(Message {
        role: MessageRole::User,
        content: query.prompt.clone(),
        timestamp: 0,
    });
    out
}

fn reconstruct_and_open(
    collected: &ClaimCollector,
    me: &PeerId,
    key: &ed25519_dalek::SigningKey,
) -> Option<Vec<u8>> {
    let k = collected.push.threshold as usize;
    let cfg =
        core_crypto::ShamirConfig::try_new(collected.push.total_shares, collected.push.threshold)
            .ok()?;
    let shamir = core_crypto::ShamirSSS::new(cfg);
    let all: Vec<core_crypto::ShamirShare> = collected
        .shares
        .iter()
        .map(|(idx, data)| core_crypto::ShamirShare {
            index: *idx,
            value: data.clone(),
        })
        .collect();
    for combo in combinations(all.len(), k) {
        let subset: Vec<core_crypto::ShamirShare> = combo.iter().map(|i| all[*i].clone()).collect();
        let Ok(mut bundle_bytes) = shamir.reconstruct_secret(&subset) else {
            continue;
        };
        let bundle = EncryptedBundle::from_bytes(&bundle_bytes);
        bundle_bytes.zeroize();
        let Some(bundle) = bundle else { continue };
        if bundle.digest() != collected.push.bundle_digest {
            continue;
        }
        if let Ok(pt) = bundle.decrypt(me, key) {
            return Some(pt);
        }
    }
    None
}

fn combinations(n: usize, k: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    if k == 0 || k > n {
        return out;
    }
    let mut idx: Vec<usize> = (0..k).collect();
    loop {
        out.push(idx.clone());
        if out.len() >= MAX_RECONSTRUCT_ATTEMPTS {
            return out;
        }
        let mut i = k;
        loop {
            if i == 0 {
                return out;
            }
            i -= 1;
            if idx[i] != i + n - k {
                break;
            }
            if i == 0 {
                return out;
            }
        }
        idx[i] += 1;
        for j in i + 1..k {
            idx[j] = idx[j - 1] + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combinations_enumerate_subsets_in_order() {
        assert_eq!(
            combinations(4, 2),
            vec![
                vec![0, 1],
                vec![0, 2],
                vec![0, 3],
                vec![1, 2],
                vec![1, 3],
                vec![2, 3]
            ]
        );
        assert_eq!(combinations(3, 3), vec![vec![0, 1, 2]]);
        assert!(combinations(2, 3).is_empty());
    }

    #[test]
    fn every_error_a_conversation_shows_has_a_code() {
        assert_eq!(
            error_code("No AI-capable peer is reachable right now — try again shortly."),
            "no_ai"
        );
        assert_eq!(
            error_code("No mesh peers connected — cannot dispatch."),
            "offline"
        );
        assert_eq!(
            error_code("No node currently serves the 'chat' channel."),
            "no_channel"
        );
        assert_eq!(
            error_code("Every AI node was busy. Try again shortly."),
            "busy"
        );
        assert_eq!(error_code("No response from the mesh in time."), "timeout");
        assert_eq!(
            error_code("Local engine not initialized."),
            "local_not_ready"
        );
        assert_eq!(
            error_code("Local Agent busy — wait for the current turn."),
            "local_busy"
        );
        assert_eq!(
            error_code("Local engine error: out of memory"),
            "local_error"
        );
        assert_eq!(error_code("encrypt: Foo"), "generic");
    }

    #[test]
    fn a_query_roundtrips_and_bounds_its_window() {
        let q = AgentQuery {
            version: AgentQuery::VERSION,
            prompt: "hola [[NAME_1]]".into(),
            window: vec![AgentTurn {
                r: 'u',
                c: "x".into(),
            }],
            memory: Some("- likes tea".into()),
            system: None,
            want_memory: true,
        };
        let back = AgentQuery::decode(&q.encode()).unwrap();
        assert_eq!(back.prompt, q.prompt);
        let mut big = q.clone();
        big.window = (0..64)
            .map(|_| AgentTurn {
                r: 'u',
                c: "y".into(),
            })
            .collect();
        assert!(AgentQuery::decode(&big.encode()).is_none());
    }

    #[test]
    fn the_responder_sees_memory_and_window_but_nothing_else() {
        let q = AgentQuery {
            version: AgentQuery::VERSION,
            prompt: "now?".into(),
            window: vec![
                AgentTurn {
                    r: 'u',
                    c: "before".into(),
                },
                AgentTurn {
                    r: 'a',
                    c: "reply".into(),
                },
            ],
            memory: Some("- fact".into()),
            system: Some("be brief".into()),
            want_memory: true,
        };
        let msgs = build_responder_messages(&q, Some("Qwen2.5 3B Instruct"));
        assert_eq!(msgs.len(), 4);
        assert!(msgs[0].content.contains("- fact"));
        assert!(msgs[0].content.contains("be brief"));
        assert!(msgs[0]
            .content
            .contains("Qwen2.5 3B Instruct through the Nepheral ecosystem"));
        assert_eq!(msgs[3].content, "now?");
    }

    #[test]
    fn the_placeholder_rule_goes_only_with_placeholders() {
        let mut q = AgentQuery {
            version: AgentQuery::VERSION,
            prompt: "Plan a day in Lisbon".into(),
            window: Vec::new(),
            memory: None,
            system: None,
            want_memory: false,
        };
        let plain = build_responder_messages(&q, None);
        assert!(!plain[0].content.contains("[["));
        q.prompt = "Write to [[NAME_1]] about Friday".into();
        let marked = build_responder_messages(&q, None);
        assert!(marked[0].content.contains("copy those through"));
    }

    #[test]
    fn model_names_read_as_a_person_says_them() {
        assert_eq!(
            display_model_name("Qwen2.5-3B-Instruct-Q4_K_M"),
            "Qwen2.5 3B Instruct"
        );
        assert_eq!(
            display_model_name("qwen2.5-0.5b-instruct-q8_0"),
            "qwen2.5 0.5b instruct"
        );
        assert_eq!(
            display_model_name("Huihui-Qwen3.5-9B-abliterated.Q6_K"),
            "Huihui Qwen3.5 9B abliterated"
        );
        assert_eq!(
            display_model_name("Llama-3.2-1B-Instruct-f16"),
            "Llama 3.2 1B Instruct"
        );
        assert_eq!(display_model_name("gemma-2-2b-it-IQ4_XS"), "gemma 2 2b it");
        assert_eq!(display_model_name("model"), "model");
    }

    #[test]
    fn a_stray_chinese_character_at_the_end_goes() {
        assert_eq!(
            trim_stray_cjk("A rainbow forms when sunlight passes through rain.\u{4e86}"),
            "A rainbow forms when sunlight passes through rain."
        );
        assert_eq!(
            trim_stray_cjk("Hola, \u{bf}como estas? Todo bien por aqui \u{3002}\n"),
            "Hola, \u{bf}como estas? Todo bien por aqui"
        );
        let chinese = "\u{4f60}\u{597d}\u{ff0c}\u{6211}\u{5f88}\u{597d}\u{3002}";
        assert_eq!(trim_stray_cjk(chinese), chinese);
        let japanese = "Thank you in Japanese is \u{3042}\u{308a}\u{304c}\u{3068}\u{3046}";
        assert_eq!(trim_stray_cjk(japanese), japanese);
        assert_eq!(trim_stray_cjk("Plain answer."), "Plain answer.");
    }

    #[test]
    fn stray_cjk_in_the_middle_of_an_answer_is_taken_out() {
        let answer = "Sound \u{4f20}\u{64ad} through the air, and it needs a material medium \
                      to travel from one point to another.";
        assert_eq!(
            strip_stray_cjk(answer, "How does sound travel?"),
            "Sound through the air, and it needs a material medium \
             to travel from one point to another."
        );
    }

    #[test]
    fn cjk_the_question_asks_about_is_kept() {
        let answer =
            "Se dice \u{4f60}\u{597d} (n\u{1d0}h\u{1ce}o), y es el saludo m\u{e1}s com\u{fa}n en \
                      cualquier conversaci\u{f3}n informal.";
        assert_eq!(
            strip_stray_cjk(answer, "\u{bf}C\u{f3}mo se dice hola en chino?"),
            answer
        );
        let asked_in_cjk = "It means hello, and people use it in every informal \
                            conversation: \u{4f60}\u{597d}.";
        assert_eq!(
            strip_stray_cjk(asked_in_cjk, "What does \u{4f60}\u{597d} mean?"),
            asked_in_cjk
        );
    }

    #[test]
    fn answers_in_cjk_or_short_ones_are_left_alone() {
        let chinese = "\u{5f69}\u{8679}\u{662f}\u{9633}\u{5149}\u{7a7f}\u{8fc7}\u{96e8}\u{6ef4}\u{5f62}\u{6210}\u{7684}\u{3002}";
        assert_eq!(strip_stray_cjk(chinese, "rainbow"), chinese);
        assert_eq!(
            strip_stray_cjk("Short \u{4e86} one", "q"),
            "Short \u{4e86} one"
        );
        let many = "A long answer with \u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94} five characters in a row, \
                    which is more than a stray word.";
        assert_eq!(strip_stray_cjk(many, "q"), many);
    }

    #[test]
    fn an_occupied_node_weighs_less_but_never_drops_out() {
        assert_eq!(availability_weight(40, false, false), 40);
        assert_eq!(availability_weight(40, true, false), 10);
        assert_eq!(availability_weight(40, false, true), 20);
        assert_eq!(availability_weight(40, true, true), 5);
        assert_eq!(availability_weight(1, true, true), 1);
    }

    #[test]
    fn only_the_node_holding_a_place_can_give_it_up() {
        let (a, b, c) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let order = [a, b, c];
        let set = |ps: &[[u8; 32]]| {
            ps.iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
        };

        // Rank 0 has nobody ahead of it: nothing to skip.
        assert!(!ranks_ahead_declined(&set(&[a, b, c]), 0, &order));
        // Rank 1 may start once rank 0 has declined...
        assert!(ranks_ahead_declined(&set(&[a]), 1, &order));
        // ...but not on a decline from anyone else. This is the forgery case:
        // rank 2 declining cannot hand rank 1 - or itself - an early start.
        assert!(!ranks_ahead_declined(&set(&[c]), 1, &order));
        assert!(!ranks_ahead_declined(&set(&[b]), 1, &order));
        // Rank 2 needs both places ahead of it given up.
        assert!(!ranks_ahead_declined(&set(&[a]), 2, &order));
        assert!(ranks_ahead_declined(&set(&[a, b]), 2, &order));
        // A rank past the end of the list is never satisfied.
        assert!(!ranks_ahead_declined(&set(&[a, b, c]), 4, &order));
    }

    #[test]
    fn rank_delays_grow_with_rank() {
        assert_eq!(rank_delay(0), Duration::ZERO);
        assert!(rank_delay(1) < rank_delay(2));
        assert_eq!(rank_delay(9), rank_delay(2));
    }
}
