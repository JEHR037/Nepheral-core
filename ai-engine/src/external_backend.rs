// SPDX-License-Identifier: BUSL-1.1
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Instant;

use crate::backend::{InferenceBackend, InferenceError};
use crate::types::{Message, MessageRole, ModelConfig};

fn looks_like_unclosed_thinking(s: &str) -> bool {
    if s.contains("</think>") {
        return false;
    }
    let lower = s.trim_start().to_lowercase();
    const TELLS: &[&str] = &[
        "okay, the user",
        "okay so the user",
        "okay, so the user",
        "the user is asking",
        "the user asked",
        "let me think",
        "let me check",
        "let me see",
        "let me analyze",
        "first, i should",
        "first, let me",
        "wait, ",
        "hmm, ",
        "alright, the user",
    ];
    TELLS.iter().any(|t| lower.starts_with(t))
}

fn strip_emojis(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            let cp = c as u32;
            !matches!(
                cp,
                0x2300..=0x23FF
                | 0x2600..=0x27BF
                | 0x2B00..=0x2BFF
                | 0xFE00..=0xFE0F
                | 0x200D
                | 0x1F000..=0x1FFFF
            )
        })
        .collect()
}

fn strip_thinking(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</think>") {
            Some(end) => {
                rest = &rest[start + end + "</think>".len()..];
            }
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);

    if let Some(pos) = out.rfind("</think>") {
        out = out[pos + "</think>".len()..].to_string();
    }

    let trimmed = out.trim();

    if looks_like_unclosed_thinking(trimmed) {
        return String::new();
    }

    if trimmed.is_empty() {
        let raw_trim = raw.trim();
        if looks_like_unclosed_thinking(raw_trim) {
            String::new()
        } else {
            raw_trim.to_string()
        }
    } else {
        trimmed.to_string()
    }
}

pub struct ExternalHttpBackend {
    client: Client,
    base_url: String,
    peer_id: Option<[u8; 32]>,
}

impl ExternalHttpBackend {
    pub fn new(base_url: &str) -> Self {
        Self {
            client: crate::http_client()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .expect("Failed to create HTTP client"),
            base_url: base_url.trim_end_matches('/').to_string(),
            peer_id: None,
        }
    }

    pub fn new_authenticated(base_url: &str, peer_id: [u8; 32]) -> Self {
        let mut backend = Self::new(base_url);
        backend.peer_id = Some(peer_id);
        backend
    }

    pub async fn generate(
        &self,
        prompt: &str,
        config: &ModelConfig,
    ) -> Result<(String, usize, u64), InferenceError> {
        let start_time = Instant::now();

        let request = GenerateRequest {
            model: config.model_name.clone(),
            prompt: prompt.to_string(),
            stream: false,
            think: Some(false),
            options: Some(GenerationOptions {
                temperature: config.temperature,
                num_predict: config.max_tokens,
            }),
        };

        let mut req = self
            .client
            .post(format!("{}/api/generate", self.base_url))
            .json(&request);

        if let Some(pid) = self.peer_id {
            req = req.header("X-Peer-Id", hex::encode(pid));
        }

        let response = req.send().await?;

        if !response.status().is_success() {
            let status = response.status();
            return Err(format!("Backend error: {}", status).into());
        }

        let body: GenerateResponse = response.json().await?;
        let elapsed = start_time.elapsed().as_millis() as u64;

        let cleaned = strip_emojis(&strip_thinking(&body.response));
        let tokens_used = cleaned.len().div_ceil(4);

        Ok((cleaned, tokens_used, elapsed))
    }

    pub async fn has_model(&self, model_name: &str) -> Result<bool, InferenceError> {
        let response = self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await?;

        if !response.status().is_success() {
            return Ok(false);
        }

        let models: ModelsResponse = response.json().await?;
        Ok(models
            .models
            .iter()
            .any(|m| m.name == model_name || m.name.starts_with(model_name)))
    }
}

#[async_trait]
impl InferenceBackend for ExternalHttpBackend {
    async fn health_check(&self) -> bool {
        self.client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn chat(
        &self,
        messages: &[Message],
        config: &ModelConfig,
    ) -> Result<(String, usize, u64), InferenceError> {
        let start_time = Instant::now();

        let chat_messages: Vec<ChatMessage> = messages
            .iter()
            .map(|m| ChatMessage {
                role: match m.role {
                    MessageRole::User => "user".to_string(),
                    MessageRole::Assistant => "assistant".to_string(),
                    MessageRole::System => "system".to_string(),
                },
                content: m.content.clone(),
                thinking: None,
            })
            .collect();

        let request = ChatRequest {
            model: config.model_name.clone(),
            messages: chat_messages,
            stream: false,
            think: Some(false),
            options: Some(GenerationOptions {
                temperature: config.temperature,
                num_predict: config.max_tokens,
            }),
        };

        let mut req = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&request);

        if let Some(pid) = self.peer_id {
            req = req.header("X-Peer-Id", hex::encode(pid));
        }

        let response = req.send().await?;

        if !response.status().is_success() {
            let status = response.status();
            return Err(format!("Backend error: {}", status).into());
        }

        let body: ChatResponse = response.json().await?;
        let elapsed = start_time.elapsed().as_millis() as u64;

        let text = strip_emojis(&strip_thinking(&body.message.content));
        let tokens_used = text.len().div_ceil(4);

        tracing::info!(
            model = %config.model_name,
            tokens = %tokens_used,
            time_ms = %elapsed,
            "Chat response complete"
        );

        Ok((text, tokens_used, elapsed))
    }

    fn backend_name(&self) -> &str {
        "external-http"
    }
}

#[derive(Serialize)]
struct GenerateRequest {
    model: String,
    prompt: String,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<GenerationOptions>,
}

#[derive(Serialize, Deserialize)]
struct GenerationOptions {
    temperature: f32,
    num_predict: usize,
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<GenerationOptions>,
}

#[derive(Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ModelsResponse {
    models: Vec<ModelInfo>,
}

#[derive(Deserialize)]
struct ModelInfo {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::{strip_emojis, strip_thinking};

    #[test]
    fn paired_tags() {
        assert_eq!(strip_thinking("<think>reason</think>\n\nHola"), "Hola");
    }

    #[test]
    fn orphan_closer_qwen3_style() {
        let raw = "Okay, the user said hi. Let me respond.</think>\n\n¡Hola!";
        assert_eq!(strip_thinking(raw), "¡Hola!");
    }

    #[test]
    fn multiple_paired_blocks() {
        let raw = "<think>a</think>X<think>b</think>Y";
        assert_eq!(strip_thinking(raw), "XY");
    }

    #[test]
    fn no_tags() {
        assert_eq!(strip_thinking("Just a reply"), "Just a reply");
    }

    #[test]
    fn unclosed_thinking_returns_empty() {
        let raw = "Okay, the user is asking \"estamos ready?\" which means \"Are we ready?\" \
in Spanish. They probably want to know if I'm ready to help them. Wait, this is the first \
message. Let me think. Hmm, I should respond in Spanish.";
        assert_eq!(strip_thinking(raw), "");
    }

    #[test]
    fn closed_thinking_with_real_answer_keeps_answer() {
        let raw = "Okay so the user said hi. Let me respond.</think>\n\n¡Hola!";
        assert_eq!(strip_thinking(raw), "¡Hola!");
    }

    #[test]
    fn strip_emojis_removes_green_circle() {
        assert_eq!(
            strip_emojis("Hello! \u{1F7E2} How can I help?"),
            "Hello!  How can I help?"
        );
    }

    #[test]
    fn strip_emojis_keeps_ascii_and_latin() {
        let s = "Hola, ¿cómo estás? Bien, gracias.";
        assert_eq!(strip_emojis(s), s);
    }

    #[test]
    fn strip_emojis_removes_misc_symbols() {
        assert_eq!(strip_emojis("Status \u{2705} OK"), "Status  OK");
    }
}
