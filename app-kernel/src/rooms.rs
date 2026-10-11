// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_crypto::identity::PeerId;
use mesh_network::protocol::{frame, message_types};
use mesh_network::rooms::{
    animal_for, mentions_agent, RoomCode, RoomEnvelope, RoomKeys, RoomPayload, AI_CODENAME,
};
use zeroize::Zeroize;

use crate::agent::{AgentTurn, PrivateQuery, QueryTarget, CONTEXT_WINDOW_MESSAGES};
use crate::state::{now_ms, spawn_send, AppState};

pub const PRESENCE_INTERVAL: Duration = Duration::from_secs(20);
pub const MEMBER_TIMEOUT: Duration = Duration::from_secs(70);
pub const NO_COMPUTE_GRACE: Duration = Duration::from_secs(20);
pub const ROOM_CHAT_CAP: usize = 500;
pub const MAX_ROOMS: usize = 32;
pub const MAX_ROOM_MEMBERS: usize = 256;

#[derive(Debug, Clone)]
pub struct RoomChatEntry {
    pub id: u64,
    pub codename: String,
    pub content: String,
    pub is_ai: bool,
    pub timestamp: Instant,
    pub rating: Option<crate::agent::RatingRef>,
    pub rated: Option<bool>,
}

impl Drop for RoomChatEntry {
    fn drop(&mut self) {
        self.content.zeroize();
    }
}

#[derive(Debug, Clone)]
pub struct RoomMember {
    pub codename: String,
    pub last_seen: Instant,
    pub contributes: bool,
}

#[derive(Debug, Clone)]
pub struct RoomState {
    pub code: RoomCode,
    pub keys: RoomKeys,
    pub members: HashMap<PeerId, RoomMember>,
    pub chat: Vec<RoomChatEntry>,
    pub ai_thinking: bool,
    pub memory: Option<String>,
    pub no_compute_since: Option<Instant>,
    pub created_at: Instant,
    pub next_message_id: u64,
}

impl Drop for RoomState {
    fn drop(&mut self) {
        if let Some(m) = self.memory.as_mut() {
            m.zeroize();
        }
    }
}

impl RoomState {
    fn new(code: RoomCode) -> Self {
        Self {
            keys: code.keys(),
            code,
            members: HashMap::new(),
            chat: Vec::new(),
            next_message_id: 1,
            ai_thinking: false,
            memory: None,
            no_compute_since: None,
            created_at: Instant::now(),
        }
    }

    fn push_chat(&mut self, mut entry: RoomChatEntry) {
        if self.chat.len() >= ROOM_CHAT_CAP {
            self.chat.remove(0);
        }
        entry.id = self.next_message_id;
        self.next_message_id += 1;
        self.chat.push(entry);
    }

    pub fn grace_remaining(&self) -> Option<Duration> {
        self.no_compute_since
            .map(|since| NO_COMPUTE_GRACE.saturating_sub(since.elapsed()))
    }
}

impl AppState {
    pub async fn i_contribute_compute(&self) -> bool {
        if !self.am_i_ai_capable().await {
            return false;
        }
        let me = *self.my_peer_id.read().await;
        self.chain
            .read()
            .await
            .contributions
            .is_lottery_eligible_at(&me, Some(now_ms()))
    }

    async fn seal_and_flood(
        &self,
        keys: &RoomKeys,
        payload: RoomPayload,
    ) -> Result<[u8; 16], String> {
        let env = RoomEnvelope::seal(keys, &self.signing_key, &payload, now_ms())?;
        self.room_seen.lock().await.insert(env.msg_id);
        self.broadcast(message_types::ROOM_ENVELOPE, &env.to_bytes(), None)
            .await;
        Ok(env.msg_id)
    }

    pub async fn join_room(&self, code: RoomCode) -> Result<String, String> {
        let key = code.as_str().to_string();
        let keys = {
            let mut rooms = self.rooms.write().await;
            if !rooms.contains_key(&key) && rooms.len() >= MAX_ROOMS {
                return Err(format!("at most {MAX_ROOMS} rooms may be open at once"));
            }
            rooms
                .entry(key.clone())
                .or_insert_with(|| RoomState::new(code.clone()))
                .keys
                .clone()
        };
        let terms = crate::nepheral_config::load_config()
            .map(|c| c.redact_terms)
            .unwrap_or_default();
        self.room_redactors
            .lock()
            .await
            .entry(key.clone())
            .or_insert_with(|| ai_engine::redaction::Redactor::with_terms(terms));
        let contributes = self.i_contribute_compute().await;
        self.seal_and_flood(&keys, RoomPayload::Join { contributes })
            .await?;
        self.room_notify.notify_waiters();
        Ok(key)
    }

    pub async fn leave_room(&self, code: &str) {
        let removed = self.rooms.write().await.remove(code);
        if let Some(room) = removed {
            let _ = self.seal_and_flood(&room.keys, RoomPayload::Leave).await;
        }
        self.room_redactors.lock().await.remove(code);
        self.pending_queries
            .write()
            .await
            .retain(|_, q| !matches!(&q.target, QueryTarget::Room(c) if c == code));
        self.room_notify.notify_waiters();
    }

    pub async fn send_room_message(&self, code: &RoomCode, content: &str) -> Result<(), String> {
        let text = content.trim();
        if text.is_empty() {
            return Err("empty message".to_string());
        }
        if text.len() > mesh_network::rooms::MAX_ROOM_TEXT_BYTES {
            return Err("message too long".to_string());
        }
        let me = *self.my_peer_id.read().await;
        let keys = {
            let mut rooms = self.rooms.write().await;
            let room = rooms
                .get_mut(code.as_str())
                .ok_or_else(|| "not in that room".to_string())?;
            room.push_chat(RoomChatEntry {
                id: 0,
                codename: animal_for(&me, code),
                content: text.to_string(),
                is_ai: false,
                timestamp: Instant::now(),
                rating: None,
                rated: None,
            });
            room.keys.clone()
        };
        self.seal_and_flood(
            &keys,
            RoomPayload::Message {
                text: text.to_string(),
                is_ai: false,
            },
        )
        .await?;
        self.room_notify.notify_waiters();
        if mentions_agent(text) {
            self.ask_room_agent(code.as_str().to_string(), text.to_string())
                .await;
        }
        Ok(())
    }

    async fn ask_room_agent(&self, code: String, prompt: String) {
        let (window_raw, memory) = {
            let mut rooms = self.rooms.write().await;
            let Some(room) = rooms.get_mut(&code) else {
                return;
            };
            if room.ai_thinking {
                return;
            }
            room.ai_thinking = true;
            let prior = room.chat.len().saturating_sub(1);
            let start = prior.saturating_sub(CONTEXT_WINDOW_MESSAGES);
            let window: Vec<(bool, String, String)> = room.chat[start..prior]
                .iter()
                .map(|e| (e.is_ai, e.codename.clone(), e.content.clone()))
                .collect();
            (window, room.memory.clone())
        };
        self.room_notify.notify_waiters();
        let (prompt_r, window) = {
            let mut redactors = self.room_redactors.lock().await;
            let r = redactors
                .entry(code.clone())
                .or_insert_with(ai_engine::redaction::Redactor::new);
            let window: Vec<AgentTurn> = window_raw
                .iter()
                .map(|(is_ai, who, text)| AgentTurn {
                    r: if *is_ai { 'a' } else { 'u' },
                    c: if *is_ai {
                        r.redact(text)
                    } else {
                        format!("{who}: {}", r.redact(text))
                    },
                })
                .collect();
            (r.redact(&prompt), window)
        };
        let result = self
            .dispatch_private_query(PrivateQuery {
                prompt: prompt_r,
                window,
                memory,
                system: Some(
                    "You are the shared assistant of a group chat. Several people write in it; each line starts with the writer's codename. Answer the latest message that mentions you."
                        .to_string(),
                ),
                target_channel: None,
                session_token: None,
                prefer_large: false,
                target: QueryTarget::Room(code.clone()),
            })
            .await;
        if let Err(e) = result {
            if let Some(local) = self.answer_room_locally(&code, &prompt).await {
                self.deliver_room_agent_reply(&code, Ok(local), None, None)
                    .await;
            } else {
                self.deliver_room_agent_reply(&code, Err(e), None, None)
                    .await;
            }
        }
    }

    async fn answer_room_locally(&self, code: &str, prompt: &str) -> Option<String> {
        let engine = self.ai_engine.read().await.clone()?;
        if !self.am_i_ai_capable().await {
            return None;
        }
        let window: Vec<ai_engine::Message> = {
            let rooms = self.rooms.read().await;
            let room = rooms.get(code)?;
            let prior = room.chat.len().saturating_sub(1);
            let start = prior.saturating_sub(CONTEXT_WINDOW_MESSAGES);
            room.chat[start..prior]
                .iter()
                .map(|e| ai_engine::Message {
                    role: if e.is_ai {
                        ai_engine::MessageRole::Assistant
                    } else {
                        ai_engine::MessageRole::User
                    },
                    content: if e.is_ai {
                        e.content.clone()
                    } else {
                        format!("{}: {}", e.codename, e.content)
                    },
                    timestamp: 0,
                })
                .collect()
        };
        let mut messages = window;
        messages.push(ai_engine::Message {
            role: ai_engine::MessageRole::User,
            content: prompt.to_string(),
            timestamp: 0,
        });
        engine
            .infer_stateless(messages, Some(0.3))
            .await
            .ok()
            .map(|r| r.text)
    }

    pub async fn deliver_room_agent_reply(
        &self,
        code: &str,
        result: Result<String, String>,
        memory: Option<String>,
        rating: Option<crate::agent::RatingRef>,
    ) {
        let restored = {
            let redactors = self.room_redactors.lock().await;
            let restore = |t: &str| match redactors.get(code) {
                Some(r) => r.restore(t),
                None => ai_engine::redaction::plain_slots(t),
            };
            result.map(|t| restore(&t))
        };
        let keys = {
            let mut rooms = self.rooms.write().await;
            let Some(room) = rooms.get_mut(code) else {
                return;
            };
            room.ai_thinking = false;
            if let Some(m) = memory {
                room.memory = Some(m);
            }
            match &restored {
                Ok(text) => room.push_chat(RoomChatEntry {
                    id: 0,
                    codename: AI_CODENAME.to_string(),
                    content: text.clone(),
                    is_ai: true,
                    timestamp: Instant::now(),
                    rating,
                    rated: None,
                }),
                Err(e) => room.push_chat(RoomChatEntry {
                    id: 0,
                    codename: "system".to_string(),
                    content: e.clone(),
                    is_ai: false,
                    timestamp: Instant::now(),
                    rating: None,
                    rated: None,
                }),
            }
            room.keys.clone()
        };
        if let Ok(text) = restored {
            let _ = self
                .seal_and_flood(&keys, RoomPayload::Message { text, is_ai: true })
                .await;
        }
        self.room_notify.notify_waiters();
    }

    pub async fn process_room_envelope(
        &self,
        raw: &[u8],
        from_addr: SocketAddr,
    ) -> Result<(), String> {
        let env =
            RoomEnvelope::from_bytes(raw).ok_or_else(|| "room envelope malformed".to_string())?;
        env.verify(now_ms())?;
        if !self.room_seen.lock().await.insert(env.msg_id) {
            return Ok(());
        }
        if env.ttl > 1 {
            let mut relayed = env.clone();
            relayed.ttl -= 1;
            let bytes = Arc::new(frame(message_types::ROOM_ENVELOPE, &relayed.to_bytes()));
            for (addr, _, conn) in self.live_connections().await {
                if addr != from_addr {
                    spawn_send(conn, bytes.clone(), "room relay");
                }
            }
        }
        let code_key = {
            let rooms = self.rooms.read().await;
            rooms
                .iter()
                .find(|(_, r)| r.keys.room_id == env.room_id)
                .map(|(k, _)| k.clone())
        };
        let Some(code_key) = code_key else {
            return Ok(());
        };
        let payload = {
            let rooms = self.rooms.read().await;
            let Some(room) = rooms.get(&code_key) else {
                return Ok(());
            };
            env.open(&room.keys)?
        };
        let me = *self.my_peer_id.read().await;
        if env.sender_pid == me {
            return Ok(());
        }
        let mut answer_presence = false;
        {
            let mut rooms = self.rooms.write().await;
            let Some(room) = rooms.get_mut(&code_key) else {
                return Ok(());
            };
            let codename = animal_for(&env.sender_pid, &room.code);
            match payload {
                RoomPayload::Join { contributes } | RoomPayload::Presence { contributes } => {
                    let is_new = !room.members.contains_key(&env.sender_pid);
                    if is_new && room.members.len() >= MAX_ROOM_MEMBERS {
                        return Ok(());
                    }
                    room.members.insert(
                        env.sender_pid,
                        RoomMember {
                            codename,
                            last_seen: Instant::now(),
                            contributes,
                        },
                    );
                    answer_presence = is_new;
                }
                RoomPayload::Leave => {
                    room.members.remove(&env.sender_pid);
                }
                RoomPayload::Message { text, is_ai } => {
                    let member = room.members.entry(env.sender_pid).or_insert(RoomMember {
                        codename: codename.clone(),
                        last_seen: Instant::now(),
                        contributes: false,
                    });
                    member.last_seen = Instant::now();
                    room.push_chat(RoomChatEntry {
                        id: 0,
                        codename: if is_ai {
                            format!("{AI_CODENAME} · {codename}")
                        } else {
                            codename
                        },
                        content: text,
                        is_ai,
                        timestamp: Instant::now(),
                        rating: None,
                        rated: None,
                    });
                }
            }
        }
        if answer_presence {
            let keys = self
                .rooms
                .read()
                .await
                .get(&code_key)
                .map(|r| r.keys.clone());
            if let Some(keys) = keys {
                let contributes = self.i_contribute_compute().await;
                let _ = self
                    .seal_and_flood(&keys, RoomPayload::Presence { contributes })
                    .await;
            }
        }
        self.room_notify.notify_waiters();
        Ok(())
    }

    pub async fn sweep_rooms(&self, announce_presence: bool) {
        let contributes = self.i_contribute_compute().await;
        let mut announce: Vec<RoomKeys> = Vec::new();
        let mut destroyed: Vec<String> = Vec::new();
        {
            let mut rooms = self.rooms.write().await;
            for (code, room) in rooms.iter_mut() {
                room.members
                    .retain(|_, m| m.last_seen.elapsed() < MEMBER_TIMEOUT);
                let someone_contributes =
                    contributes || room.members.values().any(|m| m.contributes);
                if someone_contributes {
                    room.no_compute_since = None;
                } else {
                    let since = *room.no_compute_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= NO_COMPUTE_GRACE {
                        destroyed.push(code.clone());
                        continue;
                    }
                }
                announce.push(room.keys.clone());
            }
            for code in &destroyed {
                rooms.remove(code);
            }
        }
        for code in &destroyed {
            self.room_redactors.lock().await.remove(code);
            self.log(format!(
                "Room {code} closed: no member contributed compute for {}s",
                NO_COMPUTE_GRACE.as_secs()
            ))
            .await;
        }
        for keys in announce.into_iter().filter(|_| announce_presence) {
            let _ = self
                .seal_and_flood(&keys, RoomPayload::Presence { contributes })
                .await;
        }
        if !destroyed.is_empty() {
            self.room_notify.notify_waiters();
        }
    }

    pub async fn run_room_sweeper(self) {
        let mut last_presence = Instant::now();
        loop {
            if self.sleep_or_shutdown(Duration::from_secs(5)).await {
                break;
            }
            if self.rooms.read().await.is_empty() {
                continue;
            }
            let announce = last_presence.elapsed() >= PRESENCE_INTERVAL;
            if announce {
                last_presence = Instant::now();
            }
            self.sweep_rooms(announce).await;
        }
    }
}

/// Rooms live only in memory (docs/CONVERSATIONS.md): unlike conversations
/// they are never written anywhere, sealed or not. This keeps it so.
#[cfg(test)]
mod ram_only {
    #[test]
    fn rooms_never_touch_the_disk() {
        let src = include_str!("rooms.rs");
        let code = &src[..src.find("mod ram_only").expect("this module")];
        for word in [
            "std::fs",
            "File::",
            "write_private_file",
            "tokio::fs",
            "OpenOptions",
            "nepheral_dir",
        ] {
            assert!(!code.contains(word), "rooms.rs uses {word}");
        }
        let store = include_str!("conversations.rs");
        assert!(
            !store.contains("rooms"),
            "the conversation store must not hold rooms"
        );
    }
}
