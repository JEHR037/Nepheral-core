// SPDX-License-Identifier: BUSL-1.1
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::backend::{
    BackendError, BackendEvent, ChatMessage, ChatRequest, GenerationOptions, MeshBackend,
};

#[derive(Debug, Deserialize)]
pub struct OpenAIChatRequest {
    pub model: String,
    pub messages: Vec<OpenAIRequestMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// The newer name for `max_tokens`.
    #[serde(default)]
    pub max_completion_tokens: Option<u32>,
    #[serde(default)]
    pub stop: Option<Stop>,
}

/// One stop sequence or several, as OpenAI accepts both.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Stop {
    One(String),
    Many(Vec<String>),
}

/// A message as clients send it: the content as a string, or as a list of
/// parts (the shape used for images, and by some clients for plain text).
#[derive(Debug, Deserialize)]
pub struct OpenAIRequestMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<Content>,
    #[serde(default)]
    pub private: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Deserialize)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
}

impl Content {
    /// The text of the message. Parts of any other kind (images, audio)
    /// are refused: the network carries text only for now.
    fn text(&self) -> Result<String, String> {
        match self {
            Content::Text(t) => Ok(t.clone()),
            Content::Parts(parts) => {
                let mut out = Vec::new();
                for p in parts {
                    match (p.kind.as_str(), &p.text) {
                        ("text", Some(t)) => out.push(t.as_str()),
                        (kind, _) => {
                            return Err(format!(
                                "content of type '{kind}' is not supported yet: send text"
                            ))
                        }
                    }
                }
                Ok(out.join("\n"))
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAIMessage {
    pub role: String,
    pub content: String,
    /// A private message (a note the caller keeps in its history) is never
    /// sent anywhere: it is dropped before the question leaves the API.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
}

#[derive(Debug, Serialize)]
pub struct OpenAIChatResponse {
    pub id: String,
    pub object: &'static str,
    pub created: u64,
    pub model: String,
    pub choices: Vec<OpenAIChoice>,
    pub usage: OpenAIUsage,
}

#[derive(Debug, Serialize)]
pub struct OpenAIChoice {
    pub index: u32,
    pub message: OpenAIMessage,
    pub finish_reason: &'static str,
}

#[derive(Debug, Serialize, Default)]
pub struct OpenAIUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Serialize)]
pub struct OpenAIStreamChunk {
    pub id: String,
    pub object: &'static str,
    pub created: u64,
    pub model: String,
    pub choices: Vec<OpenAIStreamChoice>,
}

#[derive(Debug, Serialize)]
pub struct OpenAIStreamChoice {
    pub index: u32,
    pub delta: OpenAIStreamDelta,
    pub finish_reason: Option<&'static str>,
}

#[derive(Debug, Serialize, Default)]
pub struct OpenAIStreamDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OpenAIModelsResponse {
    pub object: &'static str,
    pub data: Vec<OpenAIModel>,
}

#[derive(Debug, Serialize)]
pub struct OpenAIModel {
    pub id: String,
    pub object: &'static str,
    pub created: u64,
    pub owned_by: &'static str,
    pub description: String,
    pub ready: bool,
}

#[derive(Debug, Serialize)]
pub struct OpenAIErrorBody {
    pub error: OpenAIError,
}

#[derive(Debug, Serialize)]
pub struct OpenAIError {
    pub message: String,
    pub r#type: &'static str,
    pub code: &'static str,
}

#[derive(Debug, Deserialize)]
pub struct OpenSessionRequest {
    #[serde(default)]
    pub channel: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OpenSessionResponse {
    pub session_token: String,
    pub channel: Option<String>,
}

pub async fn open_session<B: MeshBackend>(
    State(backend): State<Arc<B>>,
    body: Option<Json<OpenSessionRequest>>,
) -> Response {
    let channel = body.and_then(|Json(b)| b.channel);
    match backend.open_session(channel.clone()).await {
        Ok(session_token) => (
            StatusCode::CREATED,
            Json(OpenSessionResponse {
                session_token,
                channel,
            }),
        )
            .into_response(),
        Err(e) => backend_error_to_response(e),
    }
}

pub async fn get_routing<B: MeshBackend>(
    State(backend): State<Arc<B>>,
) -> Json<crate::creds::CredsMode> {
    Json(backend.get_routing().await)
}

pub async fn put_routing<B: MeshBackend>(
    State(backend): State<Arc<B>>,
    Json(mode): Json<crate::creds::CredsMode>,
) -> Response {
    match backend.set_routing(mode.clone()).await {
        Ok(()) => (StatusCode::OK, Json(mode)).into_response(),
        Err(e) => backend_error_to_response(e),
    }
}

pub async fn list_models<B: MeshBackend>(
    State(backend): State<Arc<B>>,
) -> Json<OpenAIModelsResponse> {
    let models = backend.list_models().await;
    let now = epoch_secs();
    let data = models
        .into_iter()
        .map(|m| OpenAIModel {
            id: m.id,
            object: "model",
            created: now,
            owned_by: "nepheral",
            description: m.description,
            ready: m.ready,
        })
        .collect();
    Json(OpenAIModelsResponse {
        object: "list",
        data,
    })
}

pub async fn chat_completions<B: MeshBackend>(
    State(backend): State<Arc<B>>,
    headers: HeaderMap,
    payload: Result<Json<OpenAIChatRequest>, JsonRejection>,
) -> Response {
    let req = match payload {
        Ok(Json(req)) => req,
        Err(rejection) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                &rejection.body_text(),
                "invalid_request_error",
                "invalid_json",
            )
        }
    };
    if req.messages.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "messages array is empty",
            "invalid_request_error",
            "messages_empty",
        );
    }

    let session_token = headers
        .get("x-nepheral-session-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer ").map(str::to_string))
        });
    let mut messages = Vec::new();
    for m in req.messages.iter().filter(|m| !m.private) {
        let content = match m.content.as_ref().map(Content::text).transpose() {
            Ok(c) => c.unwrap_or_default(),
            Err(e) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    &e,
                    "invalid_request_error",
                    "unsupported_content",
                )
            }
        };
        messages.push(ChatMessage {
            role: m.role.clone(),
            content,
        });
    }
    let stop = match req.stop {
        Some(Stop::One(s)) => vec![s],
        Some(Stop::Many(v)) => v,
        None => Vec::new(),
    };
    let chat_req = ChatRequest {
        model: req.model.clone(),
        messages,
        stream: req.stream,
        session_token,
        options: GenerationOptions {
            temperature: req.temperature,
            max_tokens: req.max_completion_tokens.or(req.max_tokens),
            stop: stop.clone(),
        },
    };

    let stream = match backend.submit(chat_req).await {
        Ok(s) => crate::backend::stop_at(s, stop),
        Err(e) => return backend_error_to_response(e),
    };

    if req.stream {
        return streaming_response(stream, req.model);
    }

    aggregated_response(stream, req.model).await
}

fn streaming_response(mut stream: crate::backend::EventStream, model: String) -> Response {
    let id = chat_id();
    let now = epoch_secs();
    let model_for_stream = model.clone();
    let id_for_stream = id.clone();

    let sse_stream = async_stream::stream! {
        let mut sent_role = false;
        while let Some(ev) = stream.next().await {
            match ev {
                BackendEvent::Status(msg) => {
                    let comment = format!(": {}", msg);
                    yield Ok::<_, std::convert::Infallible>(Event::default().comment(comment));
                }
                BackendEvent::Token(chunk) => {
                    let delta = if !sent_role {
                        sent_role = true;
                        OpenAIStreamDelta {
                            role: Some("assistant".to_string()),
                            content: Some(chunk),
                        }
                    } else {
                        OpenAIStreamDelta {
                            role: None,
                            content: Some(chunk),
                        }
                    };
                    let payload = OpenAIStreamChunk {
                        id: id_for_stream.clone(),
                        object: "chat.completion.chunk",
                        created: now,
                        model: model_for_stream.clone(),
                        choices: vec![OpenAIStreamChoice {
                            index: 0,
                            delta,
                            finish_reason: None,
                        }],
                    };
                    if let Ok(s) = serde_json::to_string(&payload) {
                        yield Ok(Event::default().data(s));
                    }
                }
                BackendEvent::Done => {
                    let payload = OpenAIStreamChunk {
                        id: id_for_stream.clone(),
                        object: "chat.completion.chunk",
                        created: now,
                        model: model_for_stream.clone(),
                        choices: vec![OpenAIStreamChoice {
                            index: 0,
                            delta: OpenAIStreamDelta::default(),
                            finish_reason: Some("stop"),
                        }],
                    };
                    if let Ok(s) = serde_json::to_string(&payload) {
                        yield Ok(Event::default().data(s));
                    }
                    yield Ok(Event::default().data("[DONE]"));
                    break;
                }
                BackendEvent::Error(msg) => {
                    let comment = format!(": error: {}", msg);
                    yield Ok(Event::default().comment(comment));
                    yield Ok(Event::default().data("[DONE]"));
                    break;
                }
            }
        }
    };

    Sse::new(sse_stream).into_response()
}

async fn aggregated_response(mut stream: crate::backend::EventStream, model: String) -> Response {
    let mut buf = String::new();
    let mut err: Option<String> = None;
    while let Some(ev) = stream.next().await {
        match ev {
            BackendEvent::Token(t) => buf.push_str(&t),
            BackendEvent::Status(_) => {}
            BackendEvent::Done => break,
            BackendEvent::Error(e) => {
                err = Some(e);
                break;
            }
        }
    }
    if let Some(e) = err {
        return error_response(
            StatusCode::BAD_GATEWAY,
            &e,
            "mesh_backend_error",
            "mesh_failed",
        );
    }
    let resp = OpenAIChatResponse {
        id: chat_id(),
        object: "chat.completion",
        created: epoch_secs(),
        model,
        choices: vec![OpenAIChoice {
            index: 0,
            message: OpenAIMessage {
                role: "assistant".to_string(),
                content: buf,
                private: false,
            },
            finish_reason: "stop",
        }],
        usage: OpenAIUsage::default(),
    };
    (StatusCode::OK, Json(resp)).into_response()
}

fn backend_error_to_response(e: BackendError) -> Response {
    let (status, kind, code) = match &e {
        BackendError::UnknownModel(_) => {
            (StatusCode::NOT_FOUND, "model_not_found", "unknown_model")
        }
        BackendError::BadRequest(_) => (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "bad_request",
        ),
        BackendError::NotImplemented(_) => (
            StatusCode::NOT_IMPLEMENTED,
            "not_implemented",
            "mode_pending",
        ),
        BackendError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized", "no_session"),
        BackendError::Internal(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "internal",
        ),
    };
    error_response(status, &e.to_string(), kind, code)
}

fn error_response(
    status: StatusCode,
    message: &str,
    kind: &'static str,
    code: &'static str,
) -> Response {
    let body = OpenAIErrorBody {
        error: OpenAIError {
            message: message.to_string(),
            r#type: kind,
            code,
        },
    };
    (status, Json(body)).into_response()
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn chat_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "chatcmpl-nepheral-{:016x}",
        n.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    )
}
