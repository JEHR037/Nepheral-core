// SPDX-License-Identifier: BUSL-1.1
//! Text to vectors, with a small multilingual model run in-process.
//!
//! The model is IBM's Granite Embedding 107M Multilingual (Apache 2.0): an
//! XLM-RoBERTa encoder of six layers, chosen with `examples/bench` against
//! multilingual-e5-small, paraphrase-multilingual-MiniLM-L12 and
//! potion-multilingual-128M for retrieval across languages, a clean
//! threshold between related and unrelated messages, and speed on a CPU.
//! The app ships it as one file made by `examples/pack`: the word table in
//! 8 bits per number, the layers as published, and the tokenizer and
//! configuration inside, ~128 MB instead of 223. It also reads the files
//! Hugging Face publishes (`config.json`, `tokenizer.json`,
//! `model.safetensors`). Each text becomes one L2-normalised vector, so the
//! dot product of two is their cosine similarity. Pure Rust, the same on a
//! desktop and a phone.

use std::path::Path;

use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use crate::xlmr::{Config, Encoder};
pub use crate::xlmr::{WORDS, WORD_SCALES};

/// Longer texts are cut to this many tokens; callers split long messages
/// into passages first.
pub const MAX_TOKENS: usize = 256;
/// Texts encoded together.
const BATCH: usize = 16;
/// In a packed file, the configuration and the tokenizer, as bytes.
pub const PACKED_CONFIG: &str = "nai.config";
pub const PACKED_TOKENIZER: &str = "nai.tokenizer";

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("model files: {0}")]
    Files(String),
    #[error("tokenizer: {0}")]
    Tokenizer(String),
    #[error("model: {0}")]
    Model(#[from] candle_core::Error),
}

pub struct Embedder {
    encoder: Encoder,
    tokenizer: Tokenizer,
}

impl Embedder {
    /// A directory as Hugging Face publishes it.
    pub fn load(dir: &Path) -> Result<Self, EmbedError> {
        let read = |name: &str| {
            std::fs::read(dir.join(name)).map_err(|e| EmbedError::Files(format!("{name}: {e}")))
        };
        Self::build(
            &read("config.json")?,
            &read("tokenizer.json")?,
            &dir.join("model.safetensors"),
        )
    }

    /// One packed file.
    pub fn load_packed(path: &Path) -> Result<Self, EmbedError> {
        let (config, tokenizer) = {
            let file = std::fs::File::open(path).map_err(|e| EmbedError::Files(e.to_string()))?;
            // SAFETY: as in `xlmr`: the app's own file, never written in place.
            let map = unsafe { memmap2::Mmap::map(&file) }
                .map_err(|e| EmbedError::Files(e.to_string()))?;
            let st = safetensors::SafeTensors::deserialize(&map)
                .map_err(|e| EmbedError::Files(e.to_string()))?;
            let bytes = |name: &str| {
                st.tensor(name)
                    .map(|t| t.data().to_vec())
                    .map_err(|e| EmbedError::Files(format!("{name}: {e}")))
            };
            (bytes(PACKED_CONFIG)?, bytes(PACKED_TOKENIZER)?)
        };
        Self::build(&config, &tokenizer, path)
    }

    fn build(config: &[u8], tokenizer: &[u8], weights: &Path) -> Result<Self, EmbedError> {
        let cfg: Config =
            serde_json::from_slice(config).map_err(|e| EmbedError::Files(e.to_string()))?;
        let mut tokenizer =
            Tokenizer::from_bytes(tokenizer).map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        let pad_token = tokenizer
            .id_to_token(cfg.pad_token_id)
            .unwrap_or_else(|| "<pad>".into());
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            pad_id: cfg.pad_token_id,
            pad_token,
            ..Default::default()
        }));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        let encoder = Encoder::load(weights, cfg)?;
        Ok(Self { encoder, tokenizer })
    }

    pub fn dim(&self) -> usize {
        self.encoder.hidden_size()
    }

    /// One unit vector per text.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            let encodings = self
                .tokenizer
                .encode_batch(batch.to_vec(), true)
                .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
            let ids: Vec<Vec<u32>> = encodings.iter().map(|e| e.get_ids().to_vec()).collect();
            let mask: Vec<Vec<u32>> = encodings
                .iter()
                .map(|e| e.get_attention_mask().to_vec())
                .collect();
            let pooled = self.encoder.cls(&ids, &mask)?;
            for mut v in pooled.to_vec2::<f32>()? {
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
                v.iter_mut().for_each(|x| *x /= norm);
                out.push(v);
            }
        }
        Ok(out)
    }
}

/// Cosine similarity of two unit vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
