// SPDX-License-Identifier: BUSL-1.1
//! Conversations kept by the device (docs/CONVERSATIONS.md).
//!
//! Two parts. The memory model ("NAI private memory"): a small multilingual
//! embedding model, downloaded once and checked against a hash pinned here,
//! that chooses which earlier messages a question carries. And the store:
//! the conversations themselves, sealed with a key derived from the wallet's,
//! so only that wallet opens them; it can be switched off.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use nepheral_recall::memory::{self, MessageVectors, Recall, Turn, VECTORS_VERSION};
use nepheral_recall::select::{older_words, Plan, Rules};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::agent::{AgentChatEntry, AgentSession, AgentSessionKind, RatingRef};
use crate::state::{now_ms, AppState};

/// The memory model, as made by `recall/examples/pack.rs` from IBM's Granite
/// Embedding 107M Multilingual (Apache 2.0).
pub const RECALL_FILE: &str = "nai-memory-v1.safetensors";
pub const RECALL_SHA256: &str = "f6b8937e850f9d0be8b9275e80901022e3d1e74bb1c3ff4efeac50cf77a34cd4";
pub const RECALL_SIZE: u64 = 128_081_655;
const RECALL_URLS: &[&str] = &[
    "https://dl.nepheral.net/models/nai-memory-v1.safetensors",
    "https://dl.93.189.89.201.nip.io/models/nai-memory-v1.safetensors",
];

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RecallStatus {
    /// Not on the device; the phone asks before downloading it.
    Missing,
    Downloading {
        done: u64,
        total: u64,
    },
    Loading,
    Ready,
    Failed {
        error: String,
    },
}

/// A question held back until the person confirms the context it carries.
#[derive(Debug, Clone)]
pub struct PendingSend {
    /// The question's position in the conversation.
    pub question: usize,
    pub plan: Plan,
    /// Words of the conversation before the thread, to recompute the share.
    pub older_total: usize,
}

/// What the person sees before confirming.
#[derive(Debug, Clone, Serialize)]
pub struct ContextPreview {
    pub fragments: Vec<PreviewFragment>,
    /// Share of the older conversation's words, 0..1.
    pub share: f32,
    pub summary: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviewFragment {
    pub index: usize,
    pub from_me: bool,
    pub text: String,
    pub sensitive: bool,
}

impl ContextPreview {
    pub fn of(plan: &Plan) -> Self {
        Self {
            fragments: plan
                .older
                .iter()
                .map(|f| PreviewFragment {
                    index: f.index,
                    from_me: f.from_me,
                    text: f.text.clone(),
                    sensitive: f.sensitive,
                })
                .collect(),
            share: plan.older_share,
            summary: plan.summary,
        }
    }
}

/// The `Instant` a wall-clock time corresponds to, for elapsed times shown
/// after a restart.
pub fn instant_at(at_ms: u64) -> Instant {
    let ago = Duration::from_millis(now_ms().saturating_sub(at_ms));
    Instant::now().checked_sub(ago).unwrap_or_else(Instant::now)
}

fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

// ---- the store ----

const STORE_MAGIC: &[u8; 8] = b"NEPHCNV1";

#[derive(Serialize, Deserialize)]
struct StoredEntry {
    from_me: bool,
    content: String,
    at_ms: u64,
    private: bool,
    rating: Option<([u8; 32], [u8; 32])>,
    rated: Option<bool>,
    vectors: Option<MessageVectors>,
    error: bool,
}

#[derive(Serialize, Deserialize)]
struct StoredSession {
    id: u64,
    local: bool,
    label: String,
    created_ms: u64,
    messages: Vec<StoredEntry>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    sessions: Vec<StoredSession>,
}

impl Drop for StoredEntry {
    fn drop(&mut self) {
        self.content.zeroize();
    }
}

fn store_dir() -> PathBuf {
    crate::nepheral_config::nepheral_dir().join("conversations")
}

/// One file per wallet: another wallet restored on this device does not
/// read, or overwrite, this one's.
fn store_path(account: &[u8; 32]) -> PathBuf {
    store_dir().join(format!("{}.sealed", hex::encode(&account[..8])))
}

pub fn keep_conversations() -> bool {
    crate::nepheral_config::load_config().is_none_or(|c| c.keep_conversations)
}

/// Until when questions go without the confirmation card, if that pause is
/// still running.
pub fn confirm_snoozed_until() -> Option<u64> {
    crate::nepheral_config::load_config()
        .and_then(|c| c.confirm_snooze_until_ms)
        .filter(|&until| until > crate::state::now_ms())
}

/// Stops asking before sending older context for `minutes`; 0 asks again
/// from now on. Returns when the pause ends.
pub fn snooze_confirmations(minutes: u32) -> Result<Option<u64>, String> {
    let until = (minutes > 0).then(|| crate::state::now_ms() + u64::from(minutes) * 60_000);
    crate::nepheral_config::update(|c| c.confirm_snooze_until_ms = until)
        .map_err(|e| e.to_string())?;
    Ok(until)
}

impl AppState {
    // ---- memory model ----

    pub fn recall_path() -> PathBuf {
        if let Some(p) = std::env::var_os("NEPHERAL_RECALL_FILE").filter(|p| !p.is_empty()) {
            return PathBuf::from(p);
        }
        crate::nepheral_config::nepheral_dir()
            .join("models")
            .join(RECALL_FILE)
    }

    pub fn recall_status(&self) -> RecallStatus {
        self.recall_status.lock().expect("recall status").clone()
    }

    fn set_recall_status(&self, s: RecallStatus) {
        *self.recall_status.lock().expect("recall status") = s;
    }

    /// At start: loads the memory model if it is on the device, or fetches
    /// it when `download` (the desktop; the phone asks first).
    pub async fn run_recall_setup(self, download: bool) {
        if Self::recall_path().exists() {
            if self.load_recall().await.is_ok() {
                return;
            }
        } else {
            self.set_recall_status(RecallStatus::Missing);
        }
        if download {
            let _ = self.download_recall().await;
        }
    }

    async fn load_recall(&self) -> Result<(), String> {
        self.set_recall_status(RecallStatus::Loading);
        let path = Self::recall_path();
        let pinned = std::env::var_os("NEPHERAL_RECALL_FILE").is_none();
        let loaded = tokio::task::spawn_blocking(move || -> Result<Recall, String> {
            if pinned {
                let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
                let hash = sha256_file(&path).map_err(|e| e.to_string())?;
                if size != RECALL_SIZE || hash != RECALL_SHA256 {
                    let _ = std::fs::remove_file(&path);
                    return Err("the memory model on disk is not the published one".into());
                }
            }
            Recall::load_packed(&path).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);
        match loaded {
            Ok(r) => {
                *self.recall.write().await = Some(Arc::new(r));
                self.set_recall_status(RecallStatus::Ready);
                tracing::info!("NAI private memory ready");
                Ok(())
            }
            Err(e) => {
                tracing::warn!(error = %e, "NAI private memory did not load");
                self.set_recall_status(RecallStatus::Failed { error: e.clone() });
                Err(e)
            }
        }
    }

    /// Downloads the memory model, checks it against the pinned hash and
    /// loads it. One at a time.
    pub async fn download_recall(&self) -> Result<(), String> {
        if matches!(
            self.recall_status(),
            RecallStatus::Downloading { .. } | RecallStatus::Loading | RecallStatus::Ready
        ) {
            return Ok(());
        }
        let store = ai_engine::model_store::ModelStore::new(crate::nepheral_config::nepheral_dir());
        let mut last = String::new();
        for url in RECALL_URLS {
            self.set_recall_status(RecallStatus::Downloading {
                done: 0,
                total: RECALL_SIZE,
            });
            let status = self.recall_status.clone();
            let progress = move |done: u64, total: u64| {
                *status.lock().expect("recall status") = RecallStatus::Downloading {
                    done,
                    total: if total > 0 { total } else { RECALL_SIZE },
                };
            };
            match store
                .download_model(url, RECALL_FILE, Some(RECALL_SHA256), progress)
                .await
            {
                Ok(_) => return self.load_recall().await,
                Err(e) => {
                    tracing::warn!(url, error = %e, "Memory model download failed");
                    last = e.to_string();
                }
            }
        }
        self.set_recall_status(RecallStatus::Failed {
            error: last.clone(),
        });
        Err(last)
    }

    // ---- choosing context ----

    /// The context for the question at the end of `session_id`, chosen on
    /// the device. Missing vectors are computed first (and kept with the
    /// messages). A local conversation never leaves the device, so it takes
    /// more and never asks.
    pub(crate) async fn plan_context(
        &self,
        session_id: u64,
        local: bool,
    ) -> Result<PendingSend, String> {
        let recall = self.recall.read().await.clone();
        let snapshot: Vec<(String, bool, bool, Option<MessageVectors>)> = {
            let sessions = self.agent_sessions.read().await;
            let s = sessions.get(&session_id).ok_or("unknown session")?;
            s.messages
                .iter()
                .map(|m| (m.content.clone(), m.from_me, m.private, m.vectors.clone()))
                .collect()
        };
        if snapshot.is_empty() {
            return Err("empty conversation".into());
        }
        let question = snapshot.len() - 1;
        let snoozed = confirm_snoozed_until().is_some();
        let rules = if local {
            Rules {
                recent: 6,
                max_older: 6,
                older_word_budget: 900,
                confirm_share: f32::INFINITY,
                ..Rules::default()
            }
        } else {
            Rules::default()
        };
        let mut snapshot = snapshot;
        let plan = tokio::task::spawn_blocking(
            move || -> Result<(Plan, Vec<(usize, MessageVectors)>), String> {
                let mut computed = Vec::new();
                if let Some(r) = recall.as_deref() {
                    let missing: Vec<usize> = (0..question)
                        .filter(|&i| {
                            !snapshot[i].2
                                && snapshot[i]
                                    .3
                                    .as_ref()
                                    .is_none_or(|v| v.version != VECTORS_VERSION)
                        })
                        .collect();
                    if !missing.is_empty() {
                        let texts: Vec<&str> =
                            missing.iter().map(|&i| snapshot[i].0.as_str()).collect();
                        let vectors = r.messages(&texts).map_err(|e| e.to_string())?;
                        for (i, v) in missing.into_iter().zip(vectors) {
                            snapshot[i].3 = Some(v.clone());
                            computed.push((i, v));
                        }
                    }
                }
                let turns: Vec<Turn> = snapshot[..question]
                    .iter()
                    .map(|(text, from_me, private, vectors)| Turn {
                        text,
                        from_me: *from_me,
                        private: *private,
                        vectors: vectors.as_ref(),
                    })
                    .collect();
                let mut plan =
                    memory::choose(&turns, &snapshot[question].0, recall.as_deref(), &rules)
                        .map_err(|e| e.to_string())?;
                if local || snoozed {
                    plan.needs_confirmation = false;
                }
                for s in snapshot.iter_mut() {
                    s.0.zeroize();
                }
                Ok((plan, computed))
            },
        )
        .await
        .map_err(|e| e.to_string())??;
        let (plan, computed) = plan;
        let older_total = {
            let mut sessions = self.agent_sessions.write().await;
            let s = sessions.get_mut(&session_id).ok_or("unknown session")?;
            for (i, v) in computed {
                if let Some(m) = s.messages.get_mut(i) {
                    m.vectors = Some(v);
                }
            }
            let infos: Vec<nepheral_recall::select::MessageInfo> = s.messages[..question]
                .iter()
                .map(|m| nepheral_recall::select::MessageInfo {
                    text: &m.content,
                    from_me: m.from_me,
                    private: m.private,
                })
                .collect();
            older_words(&infos, &plan.recent)
        };
        self.conversations_changed();
        Ok(PendingSend {
            question,
            plan,
            older_total,
        })
    }

    // ---- managing conversations ----

    pub async fn rename_agent_session(&self, id: u64, label: &str) -> Result<(), String> {
        let label: String = label.trim().chars().take(80).collect();
        if label.is_empty() {
            return Err("empty name".into());
        }
        let mut sessions = self.agent_sessions.write().await;
        let s = sessions.get_mut(&id).ok_or("unknown session")?;
        s.label = label;
        drop(sessions);
        self.conversations_changed();
        self.agent_session_notify.notify_waiters();
        Ok(())
    }

    /// A private message is never chosen as context and never sent.
    pub async fn set_message_private(
        &self,
        id: u64,
        index: usize,
        private: bool,
    ) -> Result<(), String> {
        let mut sessions = self.agent_sessions.write().await;
        let m = sessions
            .get_mut(&id)
            .and_then(|s| s.messages.get_mut(index))
            .ok_or("unknown message")?;
        m.private = private;
        drop(sessions);
        self.conversations_changed();
        self.agent_session_notify.notify_waiters();
        Ok(())
    }

    // ---- the sealed store ----

    async fn store_key(&self) -> Option<([u8; 32], zeroize::Zeroizing<[u8; 32]>)> {
        let w = self.wallet_key.read().await;
        w.as_ref()
            .map(|k| (k.peer_id, k.derive_key("conversations")))
    }

    /// Writes every conversation, sealed, if keeping them is on and the
    /// wallet is open. With keeping off, removes the file.
    pub async fn save_conversations(&self) -> Result<(), String> {
        let Some((account, key)) = self.store_key().await else {
            return Ok(());
        };
        let path = store_path(&account);
        if !keep_conversations() {
            let _ = std::fs::remove_file(&path);
            return Ok(());
        }
        let gen = self.conversations_gen.load(Ordering::Relaxed);
        let mut stored = {
            let sessions = self.agent_sessions.read().await;
            let mut list: Vec<StoredSession> = sessions
                .values()
                .map(|s| StoredSession {
                    id: s.id,
                    local: s.kind == AgentSessionKind::Local,
                    label: s.label.clone(),
                    created_ms: s.created_ms,
                    messages: s
                        .messages
                        .iter()
                        .map(|m| StoredEntry {
                            from_me: m.from_me,
                            content: m.content.clone(),
                            at_ms: m.at_ms,
                            private: m.private,
                            rating: m.rating.map(|r| (r.query_digest, r.responder)),
                            rated: m.rated,
                            vectors: m.vectors.clone(),
                            error: m.error,
                        })
                        .collect(),
                })
                .collect();
            list.sort_by_key(|s| s.id);
            Stored {
                version: 1,
                sessions: list,
            }
        };
        let sealed = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
            let mut plain = postcard::to_allocvec(&stored).map_err(|e| e.to_string())?;
            stored.sessions.clear();
            let cipher = XChaCha20Poly1305::new((&*key).into());
            let mut nonce = [0u8; 24];
            rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);
            let ct = cipher
                .encrypt(XNonce::from_slice(&nonce), plain.as_slice())
                .map_err(|_| "seal failed".to_string());
            plain.zeroize();
            let mut out = Vec::with_capacity(STORE_MAGIC.len() + 24 + 16);
            out.extend_from_slice(STORE_MAGIC);
            out.extend_from_slice(&nonce);
            out.extend_from_slice(&ct?);
            Ok(out)
        })
        .await
        .map_err(|e| e.to_string())??;
        crate::nepheral_config::ensure_private_dir(&store_dir())?;
        let tmp = path.with_extension("sealed.tmp");
        crate::nepheral_config::write_private_file(&tmp, &sealed)?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))?;
        self.conversations_saved_gen.store(gen, Ordering::Relaxed);
        Ok(())
    }

    /// Reads the sealed conversations, once per run, when the wallet opens.
    pub async fn load_conversations(&self) -> Result<(), String> {
        if self.conversations_loaded.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        let Some((account, key)) = self.store_key().await else {
            self.conversations_loaded.store(false, Ordering::Relaxed);
            return Ok(());
        };
        let path = store_path(&account);
        if !keep_conversations() || !path.exists() {
            return Ok(());
        }
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let stored = tokio::task::spawn_blocking(move || -> Result<Stored, String> {
            if bytes.len() < STORE_MAGIC.len() + 24 || &bytes[..8] != STORE_MAGIC {
                return Err("not a conversations file".into());
            }
            let cipher = XChaCha20Poly1305::new((&*key).into());
            let mut plain = cipher
                .decrypt(XNonce::from_slice(&bytes[8..32]), &bytes[32..])
                .map_err(|_| "conversations file does not open with this wallet".to_string())?;
            let stored: Result<Stored, _> = postcard::from_bytes(&plain);
            plain.zeroize();
            stored.map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??;

        let terms = crate::nepheral_config::load_config()
            .map(|c| c.redact_terms)
            .unwrap_or_default();
        let mut sessions = self.agent_sessions.write().await;
        let mut redactors = self.session_redactors.lock().await;
        let mut next = self.next_agent_session_id.lock().await;
        let mut taken: HashMap<u64, ()> = sessions.keys().map(|k| (*k, ())).collect();
        let mut stored = stored;
        for mut s in stored.sessions.drain(..) {
            // Ids made this run before the wallet opened win; a stored one
            // that collides is given a fresh id.
            let id = if taken.contains_key(&s.id) {
                let v = (*next).max(s.id + 1);
                *next = v + 1;
                v
            } else {
                s.id
            };
            taken.insert(id, ());
            *next = (*next).max(id + 1);
            let messages = s
                .messages
                .drain(..)
                .map(|mut m| AgentChatEntry {
                    from_me: m.from_me,
                    content: std::mem::take(&mut m.content),
                    timestamp: instant_at(m.at_ms),
                    at_ms: m.at_ms,
                    rating: m.rating.map(|(d, r)| RatingRef {
                        query_digest: d,
                        responder: r,
                    }),
                    rated: m.rated,
                    private: m.private,
                    vectors: m.vectors.take(),
                    error: m.error,
                })
                .collect();
            sessions.insert(
                id,
                AgentSession {
                    id,
                    kind: if s.local {
                        AgentSessionKind::Local
                    } else {
                        AgentSessionKind::P2P
                    },
                    label: std::mem::take(&mut s.label),
                    messages,
                    created_at: instant_at(s.created_ms),
                    created_ms: s.created_ms,
                    awaiting_response: false,
                },
            );
            redactors.insert(
                id,
                ai_engine::redaction::Redactor::with_terms(terms.clone()),
            );
        }
        drop((sessions, redactors, next));
        let gen = self.conversations_gen.load(Ordering::Relaxed);
        self.conversations_saved_gen.store(gen, Ordering::Relaxed);
        self.agent_session_notify.notify_waiters();
        Ok(())
    }

    /// Saves a couple of seconds after anything changes, while the wallet
    /// is open.
    pub async fn run_conversation_saver(self) {
        loop {
            if self.sleep_or_shutdown(Duration::from_secs(2)).await {
                break;
            }
            let gen = self.conversations_gen.load(Ordering::Relaxed);
            if gen == self.conversations_saved_gen.load(Ordering::Relaxed) {
                continue;
            }
            if !self.is_unlocked().await {
                continue;
            }
            if let Err(e) = self.save_conversations().await {
                tracing::warn!(error = %e, "Conversations not saved");
            }
        }
    }

    /// Keeping conversations on or off. Off removes the file at once.
    pub async fn set_keep_conversations(&self, keep: bool) -> Result<(), String> {
        crate::nepheral_config::update(|c| c.keep_conversations = keep)
            .map_err(|e| e.to_string())?;
        if keep {
            self.conversations_changed();
        } else if let Some((account, _)) = self.store_key().await {
            let _ = std::fs::remove_file(store_path(&account));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::WalletKey;

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

    fn wallet(seed: u8) -> WalletKey {
        WalletKey::from_signing_key(ed25519_dalek::SigningKey::from_bytes(&[seed; 32]))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn conversations_reopen_only_with_their_wallet() {
        let home = crate::nepheral_config::test_support::TempHome::new("conversations");
        let a = state();
        a.unlock_wallet(wallet(7), None).await;
        let id = a.create_agent_session(AgentSessionKind::P2P).await;
        a.push_session_message(id, true, "mi gato se llama Tomasito".into(), None, false)
            .await;
        a.push_session_message(id, true, "nota que no sale".into(), None, true)
            .await;
        a.rename_agent_session(id, "Gatos").await.unwrap();
        a.save_conversations().await.unwrap();

        let files: Vec<_> = std::fs::read_dir(home.path().join("conversations"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(files.len(), 1);
        let sealed = std::fs::read(files[0].path()).unwrap();
        assert!(sealed.starts_with(STORE_MAGIC));
        assert!(!sealed
            .windows(9)
            .any(|w| w == b"Tomasito\0" || w.starts_with(b"Tomasito")));

        // The same wallet, in a later run, finds it as it was.
        let b = state();
        b.unlock_wallet(wallet(7), None).await;
        {
            let sessions = b.agent_sessions.read().await;
            assert_eq!(sessions.len(), 1);
            let s = sessions.values().next().unwrap();
            assert_eq!(s.label, "Gatos");
            assert_eq!(s.messages.len(), 2);
            assert!(!s.messages[0].private && s.messages[1].private);
            assert_eq!(s.messages[0].content, "mi gato se llama Tomasito");
        }
        // A new conversation does not take a stored one's id.
        let next = b.create_agent_session(AgentSessionKind::Local).await;
        assert_ne!(next, id);

        // Another wallet on the same device reads nothing.
        let c = state();
        c.unlock_wallet(wallet(8), None).await;
        assert!(c.agent_sessions.read().await.is_empty());

        // Switching keeping off removes the file.
        b.set_keep_conversations(false).await.unwrap();
        assert!(!store_path(&wallet(7).peer_id).exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_large_share_of_older_context_waits_for_confirmation() {
        let _home = crate::nepheral_config::test_support::TempHome::new("plan");
        let a = state();
        let id = a.create_agent_session(AgentSessionKind::P2P).await;
        let turns = [
            (true, "¿Cómo hago masa madre para pan casero con harina integral?"),
            (false, "Mezcla harina integral y agua a partes iguales y alimenta la masa madre cada día durante una semana."),
            (true, "Mi contraseña del banco es tal, guárdala"),
            (true, "¿Qué tiempo hace en Lisboa?"),
            (false, "Templado y con algo de viento."),
            (true, "¿Cuánta harina integral lleva la masa madre del pan?"),
        ];
        for (me, text) in turns {
            a.push_session_message(id, me, text.into(), None, false)
                .await;
        }
        a.set_message_private(id, 2, true).await.unwrap();
        let p = a.plan_context(id, false).await.unwrap();
        assert_eq!(p.question, 5);
        assert_eq!(p.plan.recent, vec![3, 4]);
        let chosen: Vec<usize> = p.plan.older.iter().map(|f| f.index).collect();
        assert!(chosen.contains(&0) || chosen.contains(&1), "{chosen:?}");
        assert!(!chosen.contains(&2), "a private message is never chosen");
        assert!(p.plan.needs_confirmation, "share {}", p.plan.older_share);

        // The same conversation with the local model never asks.
        let local = a.plan_context(id, true).await.unwrap();
        assert!(!local.plan.needs_confirmation);
    }
}
