// SPDX-License-Identifier: BUSL-1.1
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::backend::{BackendEvent, ChatMessage, ChatRequest, GenerationOptions, MeshBackend};

#[derive(Debug, Deserialize)]
pub struct OllamaChatRequest {
    pub model: String,
    pub messages: Vec<OllamaMessage>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub options: Option<OllamaOptions>,
}

/// The part of Ollama's `options` that applies here.
#[derive(Debug, Deserialize, Default)]
pub struct OllamaOptions {
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Ollama's maximum reply length; -1 or absent means no limit.
    #[serde(default)]
    pub num_predict: Option<i64>,
    #[serde(default)]
    pub stop: Option<Vec<String>>,
}

impl OllamaOptions {
    fn generation(o: Option<&Self>) -> GenerationOptions {
        let Some(o) = o else {
            return GenerationOptions::default();
        };
        GenerationOptions {
            temperature: o.temperature,
            max_tokens: o
                .num_predict
                .filter(|n| *n > 0)
                .map(|n| n.min(u32::MAX as i64) as u32),
            stop: o.stop.clone().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct OllamaMessage {
    pub role: String,
    pub content: String,
    /// A private message (a note the caller keeps in its history) is never
    /// sent anywhere: it is dropped before the question leaves the API.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
}

#[derive(Debug, Deserialize)]
pub struct OllamaGenerateRequest {
    pub model: String,
    pub prompt: String,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub options: Option<OllamaOptions>,
}

#[derive(Debug, Serialize)]
pub struct OllamaChatResponse {
    pub model: String,
    pub created_at: String,
    pub message: OllamaMessage,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_reason: Option<&'static str>,
    pub total_duration: u64,
    pub eval_count: u32,
}

#[derive(Debug, Serialize)]
pub struct OllamaGenerateResponse {
    pub model: String,
    pub created_at: String,
    pub response: String,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_reason: Option<&'static str>,
    pub total_duration: u64,
    pub eval_count: u32,
}

#[derive(Debug, Serialize)]
pub struct OllamaTagsResponse {
    pub models: Vec<OllamaTagsModel>,
}

#[derive(Debug, Serialize)]
pub struct OllamaTagsModel {
    pub name: String,
    pub model: String,
    pub modified_at: String,
    pub size: u64,
    pub digest: String,
    pub details: OllamaTagsModelDetails,
}

#[derive(Debug, Serialize)]
pub struct OllamaTagsModelDetails {
    pub format: &'static str,
    pub family: &'static str,
    pub families: Vec<String>,
    pub parameter_size: &'static str,
    pub quantization_level: &'static str,
}

#[derive(Debug, Serialize)]
pub struct OllamaVersionResponse {
    pub version: &'static str,
}

pub async fn chat<B: MeshBackend>(
    State(backend): State<Arc<B>>,
    headers: HeaderMap,
    Json(req): Json<OllamaChatRequest>,
) -> Response {
    if req.messages.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "messages array is empty");
    }
    let stream_flag = req.stream.unwrap_or(true);
    let session_token = extract_session_token(&headers);

    let chat_req = ChatRequest {
        model: req.model.clone(),
        messages: req
            .messages
            .iter()
            .filter(|m| !m.private)
            .map(|m| ChatMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect(),
        stream: false,
        session_token,
        options: OllamaOptions::generation(req.options.as_ref()),
    };
    let stops = chat_req.options.stop.clone();

    let mut stream = match backend.submit(chat_req).await {
        Ok(s) => crate::backend::stop_at(s, stops),
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
    };

    let mut buf = String::new();
    while let Some(ev) = stream.next().await {
        match ev {
            BackendEvent::Token(t) => buf.push_str(&t),
            BackendEvent::Status(_) => {}
            BackendEvent::Done => break,
            BackendEvent::Error(e) => {
                return error_response(StatusCode::BAD_GATEWAY, &e);
            }
        }
    }

    let response_message = OllamaMessage {
        role: "assistant".to_string(),
        content: buf,
        private: false,
    };

    if stream_flag {
        // Emit as NDJSON: a single content chunk + a done chunk
        let chunk = OllamaChatResponse {
            model: req.model.clone(),
            created_at: now_rfc3339(),
            message: response_message.clone(),
            done: false,
            done_reason: None,
            total_duration: 0,
            eval_count: estimate_tokens(&response_message.content) as u32,
        };
        let final_chunk = OllamaChatResponse {
            model: req.model,
            created_at: now_rfc3339(),
            message: OllamaMessage {
                role: "assistant".to_string(),
                content: String::new(),
                private: false,
            },
            done: true,
            done_reason: Some("stop"),
            total_duration: 0,
            eval_count: 0,
        };
        let body = format!(
            "{}\n{}\n",
            serde_json::to_string(&chunk).unwrap_or_default(),
            serde_json::to_string(&final_chunk).unwrap_or_default(),
        );
        (
            StatusCode::OK,
            [("content-type", "application/x-ndjson")],
            body,
        )
            .into_response()
    } else {
        Json(OllamaChatResponse {
            model: req.model,
            created_at: now_rfc3339(),
            message: response_message.clone(),
            done: true,
            done_reason: Some("stop"),
            total_duration: 0,
            eval_count: estimate_tokens(&response_message.content) as u32,
        })
        .into_response()
    }
}

pub async fn generate<B: MeshBackend>(
    State(backend): State<Arc<B>>,
    headers: HeaderMap,
    Json(req): Json<OllamaGenerateRequest>,
) -> Response {
    if req.prompt.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "prompt is empty");
    }
    let stream_flag = req.stream.unwrap_or(true);
    let session_token = extract_session_token(&headers);

    let mut messages: Vec<ChatMessage> = Vec::new();
    if let Some(s) = &req.system {
        if !s.trim().is_empty() {
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: s.clone(),
            });
        }
    }
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: req.prompt,
    });

    let chat_req = ChatRequest {
        model: req.model.clone(),
        messages,
        stream: false,
        session_token,
        options: OllamaOptions::generation(req.options.as_ref()),
    };
    let stops = chat_req.options.stop.clone();

    let mut stream = match backend.submit(chat_req).await {
        Ok(s) => crate::backend::stop_at(s, stops),
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
    };

    let mut buf = String::new();
    while let Some(ev) = stream.next().await {
        match ev {
            BackendEvent::Token(t) => buf.push_str(&t),
            BackendEvent::Status(_) => {}
            BackendEvent::Done => break,
            BackendEvent::Error(e) => {
                return error_response(StatusCode::BAD_GATEWAY, &e);
            }
        }
    }

    if stream_flag {
        let chunk = OllamaGenerateResponse {
            model: req.model.clone(),
            created_at: now_rfc3339(),
            response: buf.clone(),
            done: false,
            done_reason: None,
            total_duration: 0,
            eval_count: estimate_tokens(&buf) as u32,
        };
        let final_chunk = OllamaGenerateResponse {
            model: req.model,
            created_at: now_rfc3339(),
            response: String::new(),
            done: true,
            done_reason: Some("stop"),
            total_duration: 0,
            eval_count: 0,
        };
        let body = format!(
            "{}\n{}\n",
            serde_json::to_string(&chunk).unwrap_or_default(),
            serde_json::to_string(&final_chunk).unwrap_or_default(),
        );
        (
            StatusCode::OK,
            [("content-type", "application/x-ndjson")],
            body,
        )
            .into_response()
    } else {
        Json(OllamaGenerateResponse {
            model: req.model,
            created_at: now_rfc3339(),
            response: buf.clone(),
            done: true,
            done_reason: Some("stop"),
            total_duration: 0,
            eval_count: estimate_tokens(&buf) as u32,
        })
        .into_response()
    }
}

pub async fn tags<B: MeshBackend>(State(backend): State<Arc<B>>) -> Json<OllamaTagsResponse> {
    let models = backend.list_models().await;
    let now = now_rfc3339();
    let data = models
        .into_iter()
        .map(|m| OllamaTagsModel {
            name: m.id.clone(),
            model: m.id,
            modified_at: now.clone(),
            size: 0,
            digest: String::new(),
            details: OllamaTagsModelDetails {
                format: "nepheral",
                family: "qwen3",
                families: vec!["qwen3".to_string()],
                parameter_size: "varies",
                quantization_level: "Q4_K_M",
            },
        })
        .collect();
    Json(OllamaTagsResponse { models: data })
}

pub async fn version() -> Json<OllamaVersionResponse> {
    Json(OllamaVersionResponse {
        version: concat!("nepheral-", env!("CARGO_PKG_VERSION")),
    })
}

fn extract_session_token(h: &HeaderMap) -> Option<String> {
    h.get("x-nepheral-session-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| {
            h.get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer ").map(str::to_string))
        })
}

fn error_response(status: StatusCode, message: &str) -> Response {
    #[derive(Serialize)]
    struct E<'a> {
        error: &'a str,
    }
    (status, Json(E { error: message })).into_response()
}

fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let h = (secs / 3_600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Proleptic-Gregorian date from days since 1970-01-01 (Howard Hinnant's
/// `civil_from_days`), correct across leap years — the previous 365-day /
/// 30-day approximation drifted weeks off.
fn civil_from_days(z: i64) -> (i64, u8, u8) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn estimate_tokens(text: &str) -> usize {
    (text.split_whitespace().count() as f64 * 1.3) as usize
}
