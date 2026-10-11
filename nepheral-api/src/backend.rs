// SPDX-License-Identifier: BUSL-1.1
use std::pin::Pin;

use async_trait::async_trait;
use futures::{Stream, StreamExt};

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
    pub session_token: Option<String>,
    pub options: GenerationOptions,
}

/// What the caller asked of the reply. Temperature and length reach a
/// model running on this machine; on the mesh the answering node decides
/// them. Stop sequences apply everywhere (`stop_at`).
#[derive(Debug, Clone, Default)]
pub struct GenerationOptions {
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum BackendEvent {
    Status(String),
    Token(String),
    Done,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub description: String,
    pub ready: bool,
}

#[derive(Debug, Clone)]
pub enum BackendError {
    UnknownModel(String),
    BadRequest(String),
    NotImplemented(String),
    Unauthorized(String),
    Internal(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::UnknownModel(m) => write!(f, "unknown model: {m}"),
            BackendError::BadRequest(m) => write!(f, "bad request: {m}"),
            BackendError::NotImplemented(m) => write!(f, "not implemented: {m}"),
            BackendError::Unauthorized(m) => write!(f, "unauthorized: {m}"),
            BackendError::Internal(m) => write!(f, "internal: {m}"),
        }
    }
}

impl std::error::Error for BackendError {}

pub type EventStream = Pin<Box<dyn Stream<Item = BackendEvent> + Send + 'static>>;

/// Ends a reply at the first stop sequence, which is not sent. Text that
/// could be the start of one is held back until the next piece settles it,
/// so a sequence split across two pieces is still caught.
pub fn stop_at(mut stream: EventStream, stops: Vec<String>) -> EventStream {
    let stops: Vec<String> = stops.into_iter().filter(|s| !s.is_empty()).collect();
    if stops.is_empty() {
        return stream;
    }
    let hold = stops.iter().map(String::len).max().unwrap_or(1) - 1;
    Box::pin(async_stream::stream! {
        let mut pending = String::new();
        while let Some(ev) = stream.next().await {
            match ev {
                BackendEvent::Token(t) => {
                    pending.push_str(&t);
                    if let Some(cut) = stops.iter().filter_map(|s| pending.find(s.as_str())).min() {
                        pending.truncate(cut);
                        if !pending.is_empty() {
                            yield BackendEvent::Token(std::mem::take(&mut pending));
                        }
                        yield BackendEvent::Done;
                        return;
                    }
                    let mut split = pending.len().saturating_sub(hold);
                    while !pending.is_char_boundary(split) {
                        split -= 1;
                    }
                    if split > 0 {
                        let rest = pending.split_off(split);
                        yield BackendEvent::Token(std::mem::replace(&mut pending, rest));
                    }
                }
                BackendEvent::Done => {
                    if !pending.is_empty() {
                        yield BackendEvent::Token(std::mem::take(&mut pending));
                    }
                    yield BackendEvent::Done;
                    return;
                }
                other => yield other,
            }
        }
        if !pending.is_empty() {
            yield BackendEvent::Token(pending);
        }
    })
}

#[async_trait]
pub trait MeshBackend: Send + Sync + 'static {
    async fn list_models(&self) -> Vec<ModelInfo>;

    async fn submit(&self, request: ChatRequest) -> Result<EventStream, BackendError>;

    async fn get_routing(&self) -> crate::creds::CredsMode {
        crate::creds::CredsMode::default()
    }

    async fn set_routing(&self, _mode: crate::creds::CredsMode) -> Result<(), BackendError> {
        Err(BackendError::NotImplemented(
            "this backend does not support runtime routing changes".into(),
        ))
    }

    /// Open a paid session (burn credits via Nai::Spend) and return the
    /// resulting 32-byte session token as 64-char hex. Default impl
    /// rejects; AppStateBackend overrides to actually submit the extrinsic.
    async fn open_session(&self, _channel: Option<String>) -> Result<String, BackendError> {
        Err(BackendError::NotImplemented(
            "this backend does not support session opening".into(),
        ))
    }
}
