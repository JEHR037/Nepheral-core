// SPDX-License-Identifier: BUSL-1.1
use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::types::{FinishReason, Message, ModelConfig, StreamChunk};

pub type InferenceError = Box<dyn std::error::Error + Send + Sync>;

#[async_trait]
pub trait InferenceBackend: Send + Sync {
    async fn health_check(&self) -> bool;
    async fn chat(
        &self,
        messages: &[Message],
        config: &ModelConfig,
    ) -> Result<(String, usize, u64), InferenceError>;

    async fn chat_stream(
        &self,
        messages: &[Message],
        config: &ModelConfig,
    ) -> Result<UnboundedReceiver<StreamChunk>, InferenceError> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        match self.chat(messages, config).await {
            Ok((text, tokens, time_ms)) => {
                let _ = tx.send(StreamChunk::Response(text));
                let _ = tx.send(StreamChunk::Done {
                    tokens_used: tokens,
                    processing_time_ms: time_ms,
                    finish_reason: FinishReason::Other,
                });
            }
            Err(e) => {
                let _ = tx.send(StreamChunk::Error(e.to_string()));
            }
        }
        Ok(rx)
    }

    fn backend_name(&self) -> &str;

    /// Stops any generation in progress at the next point it can be
    /// interrupted. A backend with nothing it can stop leaves this alone.
    fn cancel(&self) {}
}
