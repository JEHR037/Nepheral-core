// SPDX-License-Identifier: BUSL-1.1
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub model_name: String,
    pub backend_url: String,
    pub model_path: Option<String>,
    pub temperature: f32,
    pub max_tokens: usize,
    pub context_window: usize,
    #[serde(default)]
    pub n_ubatch: u32,
    /// Lets a reasoning model think before it answers. Off by default: a
    /// reasoning block shares the reply budget with the answer and can leave
    /// nothing of it, and a mesh responder is judged on answering.
    #[serde(default)]
    pub allow_reasoning: bool,
    /// The devices to load the model on (llama.cpp's indexes), instead of
    /// the usual choice (`device_choice`): one GPU per worker when a rig
    /// runs a copy of the model on each card.
    #[serde(default)]
    pub devices: Option<Vec<usize>>,
    /// Text that ends a reply where it first appears, asked for by a local
    /// API caller for one reply; never stored.
    #[serde(default, skip_serializing)]
    pub stop: Vec<String>,
}

impl Default for ModelConfig {
    /// Names no model and no backend.
    ///
    /// The default used to be a specific vendor's model at
    /// `http://localhost:11434` — which is the port Nepheral's own API server
    /// listens on, so an unconfigured node opened by talking to itself about a
    /// model it had not been asked for. Nothing about Nepheral is tied to one
    /// model, and the default configuration should not be the first thing to
    /// suggest otherwise.
    fn default() -> Self {
        Self {
            model_name: "local".to_string(),
            backend_url: String::new(),
            model_path: None,
            temperature: 0.7,
            max_tokens: 2048,
            context_window: 0,
            n_ubatch: 0,
            allow_reasoning: false,
            devices: None,
            stop: Vec::new(),
        }
    }
}

/// What a local API caller may ask of one reply; `None` keeps the engine's
/// own setting.
#[derive(Debug, Clone, Default)]
pub struct Sampling {
    pub temperature: Option<f32>,
    pub max_tokens: Option<usize>,
    pub stop: Vec<String>,
}

/// `text` up to the first of `stops`, which is not kept.
pub fn cut_at_stop(mut text: String, stops: &[String]) -> String {
    if let Some(cut) = stops
        .iter()
        .filter(|s| !s.is_empty())
        .filter_map(|s| text.find(s.as_str()))
        .min()
    {
        text.truncate(cut);
    }
    text
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BackendKind {
    Local,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceRequest {
    pub request_id: u64,
    pub prompt: String,
    pub conversation_history: Vec<Message>,
    pub session_id: String,
    pub obfuscation_params: ObfuscationParams,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObfuscationParams {
    pub session_salt: Vec<u8>,
    pub permutation_seed: u64,
    pub obfuscation_level: u8,
}

impl Default for ObfuscationParams {
    fn default() -> Self {
        Self {
            session_salt: vec![0u8; 16],
            permutation_seed: 0,
            obfuscation_level: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Eog,
    MaxTokens,
    StopMarker,
    Other,
}

#[derive(Debug, Clone)]
pub enum StreamChunk {
    Thinking(String),
    Response(String),
    Done {
        tokens_used: usize,
        processing_time_ms: u64,
        finish_reason: FinishReason,
    },
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceResponse {
    pub request_id: u64,
    pub response_text: String,
    pub tokens_used: usize,
    pub processing_time_ms: u64,
    pub model_used: String,
    pub obfuscation_stats: ObfuscationStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObfuscationStats {
    pub salt_hash: String,
    pub permutation_count: usize,
    pub obfuscated_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EngineState {
    Uninitialized,
    Connecting,
    Ready,
    Processing,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestrictedKnowledgeConfig {
    pub context_visibility: u8,
    pub max_visible_fragments: usize,
    pub semantic_permutation: bool,
}

impl Default for RestrictedKnowledgeConfig {
    fn default() -> Self {
        Self {
            context_visibility: 60, // the model sees 60% of the context
            max_visible_fragments: 64,
            semantic_permutation: true,
        }
    }
}

#[derive(Debug, Clone, Zeroize, ZeroizeOnDrop)]
pub struct SensitiveData {
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct EphemeralConversation {
    pub session_id: String,
    pub context_vector: Vec<f32>,
    pub last_activity: std::time::Instant,
    pub temporal_weight: f32,
    pub ttl_seconds: u64,
}

impl EphemeralConversation {
    pub fn new(session_id: String, ttl_seconds: u64) -> Self {
        Self {
            session_id,
            context_vector: Vec::new(),
            last_activity: std::time::Instant::now(),
            temporal_weight: 1.0,
            ttl_seconds,
        }
    }

    pub fn decay(&mut self) -> f32 {
        let elapsed = self.last_activity.elapsed().as_secs_f32();
        let lambda = 1.0 / self.ttl_seconds as f32;
        self.temporal_weight = (-lambda * elapsed).exp();
        self.temporal_weight
    }

    pub fn is_expired(&self) -> bool {
        self.last_activity.elapsed().as_secs() >= self.ttl_seconds
    }

    pub fn touch(&mut self) {
        self.last_activity = std::time::Instant::now();
        self.temporal_weight = self.temporal_weight.max(0.5);
    }
}

impl Drop for EphemeralConversation {
    fn drop(&mut self) {
        self.context_vector.zeroize();
        tracing::info!(
            session_id = %self.session_id,
            "Ephemeral conversation zeroized, final weight: {:.4}",
            self.temporal_weight
        );
    }
}
