// SPDX-License-Identifier: BUSL-1.1
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use zeroize::Zeroize;

use crate::backend::{InferenceBackend, InferenceError};
use crate::external_backend::ExternalHttpBackend;
use crate::types::*;

/// Resets `EngineState::Processing` back to `Ready` on drop, so a request
/// cancelled mid-flight doesn't wedge the engine. No-op if the state already
/// moved on.
struct ProcessingGuard {
    state: Arc<RwLock<EngineState>>,
}

impl Drop for ProcessingGuard {
    fn drop(&mut self) {
        let state = self.state.clone();
        tokio::spawn(async move {
            let mut s = state.write().await;
            if matches!(*s, EngineState::Processing) {
                *s = EngineState::Ready;
            }
        });
    }
}

#[derive(Debug, Clone)]
pub struct StatelessReply {
    pub text: String,
    pub output_tokens: usize,
    pub gen_ms: u64,
    pub model_name: String,
}

pub struct InferenceEngine {
    backend: Arc<dyn InferenceBackend>,
    session_salt_hash: Arc<RwLock<String>>,
    model_config: Arc<RwLock<ModelConfig>>,
    knowledge_config: RestrictedKnowledgeConfig,
    state: Arc<RwLock<EngineState>>,
    ephemeral_sessions: Arc<RwLock<HashMap<String, EphemeralConversation>>>,
    session_histories: Arc<RwLock<HashMap<String, Vec<Message>>>>,
    inference_slot: Arc<Mutex<()>>,
    system_prompt: Arc<RwLock<Option<String>>>,
    /// Why the configured local model could not be loaded, if one was
    /// configured. Held rather than discarded: this is the only place that
    /// knows the real reason, and the buyer of a wrong answer is the user
    /// staring at a chat box.
    local_load_error: Option<String>,
}

impl InferenceEngine {
    /// Builds an engine for `model_config`.
    ///
    /// A configured local model that fails to load is **not** quietly replaced
    /// by the external HTTP backend. Doing that turned "this GGUF cannot be
    /// loaded" into "Backend 'external-http' not available", which named the
    /// wrong component and sent people looking for a server that was never
    /// part of the request. The failure is carried instead, and reported by
    /// [`Self::initialize`] as what it is.
    pub fn new(model_config: ModelConfig) -> Self {
        let mut local_load_error: Option<String> = None;

        #[cfg(feature = "local-inference")]
        if let Some(ref path) = model_config.model_path {
            if std::path::Path::new(path).exists() {
                match crate::local_backend::LlamaLocalBackend::load_on(
                    path,
                    model_config.devices.as_deref(),
                ) {
                    Ok(local) => {
                        tracing::info!(path = %path, "Using local GGUF backend");
                        return Self::new_with_backend(model_config, Arc::new(local));
                    }
                    Err(e) => {
                        tracing::error!(error = %e, path = %path, "Local model failed to load");
                        local_load_error = Some(e.to_string());
                    }
                }
            }
        }

        if local_load_error.is_none() && model_config.backend_url.trim().is_empty() {
            // Nothing local to load and nowhere external to ask. Saying so
            // beats reporting that an unnamed server is unreachable.
            local_load_error = Some(
                concat!(
                    "no model is configured: install a model, ",
                    "or set a backend URL to reach one served elsewhere"
                )
                .to_string(),
            );
        }

        let backend = Arc::new(ExternalHttpBackend::new(&model_config.backend_url));
        let mut engine = Self::new_with_backend(model_config, backend);
        engine.local_load_error = local_load_error;
        engine
    }

    pub fn new_with_backend(model_config: ModelConfig, backend: Arc<dyn InferenceBackend>) -> Self {
        Self {
            backend,
            session_salt_hash: Arc::new(RwLock::new(String::new())),
            model_config: Arc::new(RwLock::new(model_config)),
            knowledge_config: RestrictedKnowledgeConfig::default(),
            state: Arc::new(RwLock::new(EngineState::Uninitialized)),
            ephemeral_sessions: Arc::new(RwLock::new(HashMap::new())),
            inference_slot: Arc::new(Mutex::new(())),
            session_histories: Arc::new(RwLock::new(HashMap::new())),
            system_prompt: Arc::new(RwLock::new(None)),
            local_load_error: None,
        }
    }

    /// Tokens of conversation that may be sent, leaving room for the reply.
    ///
    /// The prompt and the reply share one window: everything sent counts
    /// against the same `n_ctx` the model will generate into. Filling it with
    /// history leaves nothing to answer with, which is how a conversation
    /// started returning empty replies after a long turn or two.
    fn history_token_budget(context_window: usize, max_tokens: usize) -> usize {
        // A margin for the chat template's own tags and the system prompt,
        // neither of which is counted here.
        const TEMPLATE_MARGIN_TOKENS: usize = 256;
        context_window
            .saturating_sub(max_tokens)
            .saturating_sub(TEMPLATE_MARGIN_TOKENS)
    }

    /// Rough token count. Four bytes per token is the same approximation the
    /// rest of this crate reports usage with; it does not need to be exact,
    /// only never to under-count badly enough to overflow the window.
    fn estimated_tokens(m: &Message) -> usize {
        m.content.len().div_ceil(4)
    }

    /// Drops the oldest turns until the conversation fits `budget`.
    ///
    /// The previous version compared the number of messages against a figure
    /// in tokens — `history.len() > context_window / 2` — so with a 4096
    /// token window it trimmed at two thousand messages and in practice never
    /// trimmed at all. The most recent turn is always kept, however long:
    /// dropping it would answer a question the user can no longer see.
    fn trim_history_to_budget(history: &mut Vec<Message>, budget: usize) {
        let mut total: usize = history.iter().map(Self::estimated_tokens).sum();
        while total > budget && history.len() > 1 {
            let dropped = Self::estimated_tokens(&history.remove(0));
            total = total.saturating_sub(dropped);
        }
        if total > budget {
            tracing::warn!(
                tokens = total,
                budget,
                "A single turn is larger than the context budget; it is kept and the model \
                 will see it truncated"
            );
        }
    }

    /// Stops this engine and erases what it holds about conversations,
    /// ahead of being replaced.
    ///
    /// Called before the next engine is loaded, never after. Replacing it the
    /// other way round left the previous model in memory for as long as a
    /// reply in progress still referenced it, so switching models mid-reply
    /// put two of them in RAM at once — enough to exhaust a 16 GB machine
    /// with a 10 GB model.
    ///
    /// The text is overwritten before it is dropped. Dropping only hands the
    /// memory back to the allocator, content and all.
    pub async fn shutdown(&self) {
        use zeroize::Zeroize;

        self.backend.cancel();

        {
            let mut histories = self.session_histories.write().await;
            for messages in histories.values_mut() {
                for m in messages.iter_mut() {
                    m.content.zeroize();
                }
            }
            histories.clear();
        }
        {
            let mut sessions = self.ephemeral_sessions.write().await;
            for s in sessions.values_mut() {
                s.context_vector.zeroize();
            }
            sessions.clear();
        }
        {
            let mut prompt = self.system_prompt.write().await;
            if let Some(p) = prompt.as_mut() {
                p.zeroize();
            }
            *prompt = None;
        }
    }

    pub async fn set_system_prompt(&self, prompt: Option<String>) {
        *self.system_prompt.write().await = prompt;
    }

    pub async fn get_system_prompt(&self) -> Option<String> {
        self.system_prompt.read().await.clone()
    }

    fn rotate_salt(session_id: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(session_id.as_bytes());
        hasher.update(chrono::Utc::now().timestamp_millis().to_le_bytes());
        let mut random = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut random);
        hasher.update(random);
        hex::encode(&hasher.finalize()[..8])
    }

    pub async fn initialize(&self) -> Result<(), InferenceError> {
        // A model that was asked for and could not be loaded is the answer,
        // not a reason to go and probe something else.
        if let Some(error) = &self.local_load_error {
            *self.state.write().await = EngineState::Error(error.clone());
            return Err(error.clone().into());
        }

        *self.state.write().await = EngineState::Connecting;

        let model_name = self.model_config.read().await.model_name.clone();
        tracing::info!(
            backend = %self.backend.backend_name(),
            model = %model_name,
            "Initializing AI inference engine"
        );

        if self.backend.health_check().await {
            *self.state.write().await = EngineState::Ready;
            tracing::info!("AI engine initialized successfully");
            Ok(())
        } else {
            let error = format!("Backend '{}' not available", self.backend.backend_name());
            *self.state.write().await = EngineState::Error(error.clone());
            Err(error.into())
        }
    }

    pub async fn process_request(
        &self,
        request: InferenceRequest,
    ) -> Result<InferenceResponse, InferenceError> {
        let _slot = self.inference_slot.lock().await;

        let current_state = self.state.read().await.clone();
        if !matches!(current_state, EngineState::Ready) {
            return Err(format!("Engine not ready: {:?}", current_state).into());
        }

        *self.state.write().await = EngineState::Processing;
        let _processing_guard = ProcessingGuard {
            state: self.state.clone(),
        };

        let start_time = std::time::Instant::now();

        let coded_prompt = request.prompt.clone();
        let coded_history: Vec<Message> = {
            let cap = self.knowledge_config.max_visible_fragments;
            let total = request.conversation_history.len();
            let sys_count = request
                .conversation_history
                .iter()
                .take_while(|m| matches!(m.role, MessageRole::System))
                .count();
            if total <= cap {
                request.conversation_history.clone()
            } else {
                let tail_slots = cap.saturating_sub(sys_count);
                let tail_skip = total.saturating_sub(tail_slots).max(sys_count);
                let mut out: Vec<Message> = request.conversation_history[..sys_count].to_vec();
                out.extend(request.conversation_history.iter().skip(tail_skip).cloned());
                out
            }
        };

        let sys_prompt = self.system_prompt.read().await.clone();
        let mut full_history: Vec<Message> = Vec::new();
        if let Some(sys) = sys_prompt {
            full_history.push(Message {
                role: MessageRole::System,
                content: sys,
                timestamp: 0,
            });
        }
        full_history.extend(coded_history);
        full_history.push(Message {
            role: MessageRole::User,
            content: coded_prompt.clone(),
            timestamp: chrono::Utc::now().timestamp_millis() as u64,
        });

        let model_config_snapshot = self.model_config.read().await.clone();
        Self::fit_to_window(&mut full_history, &model_config_snapshot);
        let (raw_response, tokens_used, _backend_time) = match self
            .backend
            .chat(&full_history, &model_config_snapshot)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                *self.state.write().await = EngineState::Ready;
                return Err(e);
            }
        };

        let response_text = raw_response;
        let entity_count = 0usize;
        let coded_size = coded_prompt.len();

        let elapsed = start_time.elapsed().as_millis() as u64;

        {
            let mut sessions = self.ephemeral_sessions.write().await;
            let session = EphemeralConversation::new(request.session_id.clone(), 1800);
            sessions.insert(request.session_id.clone(), session);
        }

        {
            let mut histories = self.session_histories.write().await;
            let history = histories
                .entry(request.session_id.clone())
                .or_insert_with(Vec::new);

            history.push(Message {
                role: MessageRole::User,
                content: request.prompt.clone(),
                timestamp: chrono::Utc::now().timestamp_millis() as u64,
            });
            history.push(Message {
                role: MessageRole::Assistant,
                content: response_text.clone(),
                timestamp: chrono::Utc::now().timestamp_millis() as u64,
            });

            Self::trim_history_to_budget(
                history,
                Self::history_token_budget(
                    model_config_snapshot.context_window,
                    model_config_snapshot.max_tokens,
                ),
            );
        }

        let salt_hash = self.session_salt_hash.read().await.clone();

        let response = InferenceResponse {
            request_id: request.request_id,
            response_text,
            tokens_used,
            processing_time_ms: elapsed,
            model_used: model_config_snapshot.model_name.clone(),
            obfuscation_stats: ObfuscationStats {
                salt_hash,
                permutation_count: entity_count,
                obfuscated_size: coded_size,
            },
        };

        *self.state.write().await = EngineState::Ready;

        tracing::info!(
            request_id = %request.request_id,
            tokens = %tokens_used,
            time_ms = %elapsed,
            entities_substituted = %entity_count,
            "Inference complete"
        );

        Ok(response)
    }

    fn fit_to_window(messages: &mut Vec<Message>, config: &ModelConfig) {
        let budget = Self::history_token_budget(config.context_window.max(1024), config.max_tokens);
        let lead_system = messages
            .iter()
            .take_while(|m| matches!(m.role, MessageRole::System))
            .count();
        let mut total: usize = messages.iter().map(Self::estimated_tokens).sum();
        while total > budget && messages.len() > lead_system + 1 {
            let dropped = Self::estimated_tokens(&messages.remove(lead_system));
            total = total.saturating_sub(dropped);
        }
    }

    pub async fn infer_stateless(
        &self,
        messages: Vec<Message>,
        temperature: Option<f32>,
    ) -> Result<StatelessReply, InferenceError> {
        self.infer_stateless_with(messages, temperature, None).await
    }

    /// As `infer_stateless`, with reasoning forced on or off for this one
    /// reply whatever the engine's setting (answers for the mesh never
    /// reason: a model thinking on a CPU held questions for minutes).
    pub async fn infer_stateless_with(
        &self,
        messages: Vec<Message>,
        temperature: Option<f32>,
        allow_reasoning: Option<bool>,
    ) -> Result<StatelessReply, InferenceError> {
        let sampling = Sampling {
            temperature,
            ..Sampling::default()
        };
        self.infer_reply(messages, &sampling, allow_reasoning).await
    }

    /// As `infer_stateless`, with what a local API caller asked of this one
    /// reply: temperature, length and stop sequences.
    pub async fn infer_sampled(
        &self,
        messages: Vec<Message>,
        sampling: &Sampling,
    ) -> Result<StatelessReply, InferenceError> {
        self.infer_reply(messages, sampling, None).await
    }

    async fn infer_reply(
        &self,
        mut messages: Vec<Message>,
        sampling: &Sampling,
        allow_reasoning: Option<bool>,
    ) -> Result<StatelessReply, InferenceError> {
        let _slot = self.inference_slot.lock().await;
        let current_state = self.state.read().await.clone();
        if !matches!(current_state, EngineState::Ready) {
            return Err(format!("Engine not ready: {:?}", current_state).into());
        }
        *self.state.write().await = EngineState::Processing;
        let _processing_guard = ProcessingGuard {
            state: self.state.clone(),
        };
        let mut config = self.model_config.read().await.clone();
        if let Some(t) = sampling.temperature {
            config.temperature = t;
        }
        if let Some(n) = sampling.max_tokens {
            config.max_tokens = n.max(1);
        }
        config.stop = sampling.stop.clone();
        if let Some(r) = allow_reasoning {
            config.allow_reasoning = r;
        }
        Self::fit_to_window(&mut messages, &config);
        let started = std::time::Instant::now();
        let result = self.backend.chat(&messages, &config).await;
        for m in messages.iter_mut() {
            m.content.zeroize();
        }
        *self.state.write().await = EngineState::Ready;
        let (text, tokens, _) = result?;
        // External backends do not see the stop sequences; cut here too.
        let text = cut_at_stop(text, &config.stop);
        Ok(StatelessReply {
            text,
            output_tokens: tokens,
            gen_ms: started.elapsed().as_millis() as u64,
            model_name: config.model_name,
        })
    }

    pub async fn chat_message(
        &self,
        session_id: &str,
        user_message: &str,
    ) -> Result<InferenceResponse, InferenceError> {
        *self.session_salt_hash.write().await = Self::rotate_salt(session_id);

        {
            let mut sessions = self.ephemeral_sessions.write().await;
            if let Some(session) = sessions.get_mut(session_id) {
                session.touch();
            }
        }

        let history = {
            let histories = self.session_histories.read().await;
            histories.get(session_id).cloned().unwrap_or_default()
        };

        let request = InferenceRequest {
            request_id: chrono::Utc::now().timestamp_millis() as u64,
            prompt: user_message.to_string(),
            conversation_history: history,
            session_id: session_id.to_string(),
            obfuscation_params: ObfuscationParams::default(),
        };

        self.process_request(request).await
    }

    pub async fn chat_message_stream(
        &self,
        session_id: &str,
        user_message: &str,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<crate::types::StreamChunk>, InferenceError>
    {
        let _slot = self.inference_slot.lock().await;

        let current_state = self.state.read().await.clone();
        if !matches!(current_state, EngineState::Ready) {
            return Err(format!("Engine not ready: {:?}", current_state).into());
        }

        *self.session_salt_hash.write().await = Self::rotate_salt(session_id);
        *self.state.write().await = EngineState::Processing;
        let _processing_guard = ProcessingGuard {
            state: self.state.clone(),
        };

        let history = {
            let histories = self.session_histories.read().await;
            histories.get(session_id).cloned().unwrap_or_default()
        };

        let sys_prompt = self.system_prompt.read().await.clone();
        let mut full_history: Vec<Message> = Vec::new();
        if let Some(sys) = sys_prompt {
            full_history.push(Message {
                role: MessageRole::System,
                content: sys,
                timestamp: 0,
            });
        }
        full_history.extend(history);
        full_history.push(Message {
            role: MessageRole::User,
            content: user_message.to_string(),
            timestamp: chrono::Utc::now().timestamp_millis() as u64,
        });

        let stream_config_snapshot = self.model_config.read().await.clone();
        let rx = match self
            .backend
            .chat_stream(&full_history, &stream_config_snapshot)
            .await
        {
            Ok(rx) => rx,
            Err(e) => {
                *self.state.write().await = EngineState::Ready;
                return Err(e);
            }
        };

        {
            let mut histories = self.session_histories.write().await;
            let h = histories
                .entry(session_id.to_string())
                .or_insert_with(Vec::new);
            h.push(Message {
                role: MessageRole::User,
                content: user_message.to_string(),
                timestamp: chrono::Utc::now().timestamp_millis() as u64,
            });
            if h.len() > stream_config_snapshot.context_window / 2 {
                let drain_to = h.len() - (stream_config_snapshot.context_window / 2);
                h.drain(..drain_to);
            }
        }

        *self.state.write().await = EngineState::Ready;
        Ok(rx)
    }

    pub async fn record_assistant_turn(&self, session_id: &str, response_text: &str) {
        if response_text.trim().is_empty() {
            return;
        }
        let ctx_window = self.model_config.read().await.context_window;
        let mut histories = self.session_histories.write().await;
        let h = histories
            .entry(session_id.to_string())
            .or_insert_with(Vec::new);
        h.push(Message {
            role: MessageRole::Assistant,
            content: response_text.to_string(),
            timestamp: chrono::Utc::now().timestamp_millis() as u64,
        });
        if h.len() > ctx_window / 2 {
            let drain_to = h.len() - (ctx_window / 2);
            h.drain(..drain_to);
        }
    }

    pub async fn decay_sessions(&self) {
        let mut sessions = self.ephemeral_sessions.write().await;
        let mut expired_ids: Vec<String> = Vec::new();

        for (session_id, session) in sessions.iter_mut() {
            let weight = session.decay();
            if weight < 0.01 || session.is_expired() {
                expired_ids.push(session_id.clone());
            }
        }

        for session_id in expired_ids {
            sessions.remove(&session_id);
            tracing::info!(session_id = %session_id, "Ephemeral session expired");

            let mut histories = self.session_histories.write().await;
            if let Some(mut history) = histories.remove(&session_id) {
                for msg in history.iter_mut() {
                    msg.content.zeroize();
                }
            }
        }
    }

    pub async fn get_state(&self) -> EngineState {
        self.state.read().await.clone()
    }

    pub async fn get_config(&self) -> ModelConfig {
        self.model_config.read().await.clone()
    }

    pub async fn set_config(&self, config: ModelConfig) {
        *self.model_config.write().await = config;
    }

    pub async fn update_knowledge_config(&mut self, config: RestrictedKnowledgeConfig) {
        self.knowledge_config = config;
    }

    pub async fn active_sessions(&self) -> Vec<(String, f32)> {
        let sessions = self.ephemeral_sessions.read().await;
        sessions
            .iter()
            .map(|(id, session)| (id.clone(), session.temporal_weight))
            .collect()
    }

    pub async fn terminate_session(&self, session_id: &str) {
        let mut sessions = self.ephemeral_sessions.write().await;
        if let Some(mut session) = sessions.remove(session_id) {
            session.context_vector.zeroize();
            tracing::info!(session_id = %session_id, "Session terminated");
        }

        let mut histories = self.session_histories.write().await;
        if let Some(mut history) = histories.remove(session_id) {
            for msg in history.iter_mut() {
                msg.content.zeroize();
            }
        }
    }

    pub fn generate_request_id() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: MessageRole, chars: usize) -> Message {
        Message {
            role,
            content: "x".repeat(chars),
            timestamp: 0,
        }
    }

    fn turns(n: usize, chars: usize) -> Vec<Message> {
        (0..n)
            .flat_map(|_| {
                [
                    msg(MessageRole::User, chars),
                    msg(MessageRole::Assistant, chars),
                ]
            })
            .collect()
    }

    #[test]
    fn the_budget_leaves_room_for_the_reply() {
        // Prompt and reply share one window. A budget that spends all of it
        // on history leaves nothing to answer with.
        let budget = InferenceEngine::history_token_budget(4096, 512);
        assert!(budget + 512 < 4096, "budget {budget} leaves no margin");
        assert!(budget > 0);
    }

    #[test]
    fn a_window_smaller_than_the_reply_yields_no_budget() {
        assert_eq!(InferenceEngine::history_token_budget(512, 4096), 0);
    }

    #[test]
    fn a_long_conversation_is_trimmed_to_the_budget() {
        // The bug this replaces compared a count of messages against a figure
        // in tokens, so with a 4096-token window it trimmed at two thousand
        // messages and in practice never trimmed at all.
        let mut history = turns(50, 400); // ~100 tokens each, ~10000 total
        let budget = 1000;
        InferenceEngine::trim_history_to_budget(&mut history, budget);
        let total: usize = history.iter().map(InferenceEngine::estimated_tokens).sum();
        assert!(total <= budget, "{total} tokens left, budget {budget}");
        assert!(!history.is_empty());
    }

    #[test]
    fn the_newest_turn_survives_however_long_it_is() {
        // Dropping it would answer a question the user can no longer see.
        let mut history = vec![msg(MessageRole::User, 100_000)];
        InferenceEngine::trim_history_to_budget(&mut history, 10);
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn trimming_drops_the_oldest_first() {
        let mut history = vec![
            msg(MessageRole::User, 4000),
            msg(MessageRole::Assistant, 4000),
            msg(MessageRole::User, 40),
        ];
        InferenceEngine::trim_history_to_budget(&mut history, 100);
        assert_eq!(history.len(), 1);
        assert_eq!(
            history[0].content.len(),
            40,
            "the newest turn is the one kept"
        );
    }

    #[test]
    fn fitting_keeps_the_system_prompt_and_the_newest_turn() {
        let config = ModelConfig {
            context_window: 1024,
            max_tokens: 256,
            ..Default::default()
        };
        let mut messages = vec![msg(MessageRole::System, 400)];
        messages.extend(turns(20, 400));
        messages.push(msg(MessageRole::User, 40));
        InferenceEngine::fit_to_window(&mut messages, &config);
        assert!(matches!(messages[0].role, MessageRole::System));
        assert_eq!(messages.last().unwrap().content.len(), 40);
        let total: usize = messages.iter().map(InferenceEngine::estimated_tokens).sum();
        assert!(total <= InferenceEngine::history_token_budget(1024, 256));
    }

    #[test]
    fn a_conversation_within_budget_is_left_alone() {
        let mut history = turns(3, 40);
        let before = history.len();
        InferenceEngine::trim_history_to_budget(&mut history, 4096);
        assert_eq!(history.len(), before);
    }
}
