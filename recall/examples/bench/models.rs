// SPDX-License-Identifier: BUSL-1.1
//! The candidates, loaded with candle's own models for comparison.
//!
//! Three kinds of model are read, all from the files Hugging Face publishes
//! (`config.json`, `tokenizer.json`, `model.safetensors`): BERT and
//! XLM-RoBERTa encoders, and Model2Vec's static tables. The output is one
//! L2-normalised vector per text, so the dot product of two is their cosine
//! similarity. Pure Rust (candle), the same on a desktop and a phone.

use std::path::Path;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("model files: {0}")]
    Files(String),
    #[error("tokenizer: {0}")]
    Tokenizer(String),
    #[error("model: {0}")]
    Model(#[from] candle_core::Error),
}

/// Whether a text is what is searched for or what is searched through.
/// Some models (the E5 family) are trained with a prefix for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Query,
    Passage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Bert,
    XlmRoberta,
    Static,
}

/// How one vector is made from the per-token ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    Mean,
    /// The first token's (Granite).
    Cls,
}

/// How to read and use a model.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub family: Family,
    pub pooling: Pooling,
    pub query_prefix: &'static str,
    pub passage_prefix: &'static str,
    /// Longer texts are cut to this many tokens.
    pub max_tokens: usize,
}

enum Inner {
    Bert(candle_transformers::models::bert::BertModel),
    XlmRoberta(candle_transformers::models::xlm_roberta::XLMRobertaModel),
    /// Model2Vec: one row per token, averaged.
    Static {
        table: Tensor,
    },
}

pub struct Embedder {
    inner: Inner,
    tokenizer: Tokenizer,
    spec: ModelSpec,
    device: Device,
    dim: usize,
}

impl Embedder {
    pub fn load(dir: &Path, spec: ModelSpec) -> Result<Self, EmbedError> {
        let device = Device::Cpu;
        let read = |name: &str| {
            std::fs::read(dir.join(name)).map_err(|e| EmbedError::Files(format!("{name}: {e}")))
        };
        let mut tokenizer = Tokenizer::from_bytes(read("tokenizer.json")?)
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        let weights = dir.join("model.safetensors");
        // SAFETY: the file is only read, and not changed while mapped: it is
        // the app's own copy, replaced by renaming a new file into place.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[&weights], DType::F32, &device)? };
        let config = read("config.json")?;
        let (inner, dim) = match spec.family {
            Family::Bert => {
                let cfg: candle_transformers::models::bert::Config =
                    serde_json::from_slice(&config)
                        .map_err(|e| EmbedError::Files(e.to_string()))?;
                let dim = cfg.hidden_size;
                (
                    Inner::Bert(candle_transformers::models::bert::BertModel::load(
                        vb, &cfg,
                    )?),
                    dim,
                )
            }
            Family::XlmRoberta => {
                let cfg: candle_transformers::models::xlm_roberta::Config =
                    serde_json::from_slice(&config)
                        .map_err(|e| EmbedError::Files(e.to_string()))?;
                let dim = cfg.hidden_size;
                (
                    Inner::XlmRoberta(
                        candle_transformers::models::xlm_roberta::XLMRobertaModel::new(&cfg, vb)?,
                    ),
                    dim,
                )
            }
            Family::Static => {
                let table = candle_core::safetensors::load(&weights, &device)?
                    .remove("embeddings")
                    .ok_or_else(|| EmbedError::Files("no `embeddings` tensor".into()))?
                    .to_dtype(DType::F32)?;
                let dim = table.dim(1)?;
                (Inner::Static { table }, dim)
            }
        };
        let pad_token = ["<pad>", "[PAD]"]
            .into_iter()
            .find(|t| tokenizer.token_to_id(t).is_some())
            .unwrap_or("<pad>")
            .to_string();
        let pad_id = tokenizer.token_to_id(&pad_token).unwrap_or(0);
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            pad_id,
            pad_token,
            ..Default::default()
        }));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: spec.max_tokens,
                ..Default::default()
            }))
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        Ok(Self {
            inner,
            tokenizer,
            spec,
            device,
            dim,
        })
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    /// One normalised vector per text.
    pub fn embed(&self, texts: &[&str], kind: Kind) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let prefix = match kind {
            Kind::Query => self.spec.query_prefix,
            Kind::Passage => self.spec.passage_prefix,
        };
        let inputs: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
        // A static model averages word rows; its special tokens carry no
        // meaning, so they are left out.
        let special = !matches!(self.inner, Inner::Static { .. });
        let encodings = self
            .tokenizer
            .encode_batch(inputs, special)
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        let ids: Vec<Vec<u32>> = encodings.iter().map(|e| e.get_ids().to_vec()).collect();
        let mask: Vec<Vec<u32>> = encodings
            .iter()
            .map(|e| e.get_attention_mask().to_vec())
            .collect();
        let ids = Tensor::new(ids, &self.device)?;
        let mask_t = Tensor::new(mask, &self.device)?;
        let hidden = match &self.inner {
            Inner::Bert(m) => {
                let types = ids.zeros_like()?;
                m.forward(&ids, &types, Some(&mask_t))?
            }
            Inner::XlmRoberta(m) => {
                let types = ids.zeros_like()?;
                m.forward(&ids, &mask_t, &types, None, None, None)?
            }
            Inner::Static { table } => {
                let (b, t) = ids.dims2()?;
                table
                    .index_select(&ids.flatten_all()?, 0)?
                    .reshape((b, t, self.dim))?
            }
        };
        let pooled = match self.spec.pooling {
            Pooling::Cls => hidden.narrow(1, 0, 1)?.squeeze(1)?,
            Pooling::Mean => {
                // Over the real tokens only.
                let m = mask_t.to_dtype(DType::F32)?.unsqueeze(2)?;
                let summed = hidden.broadcast_mul(&m)?.sum(1)?;
                let counts = m.sum(1)?.clamp(1e-9, f64::MAX)?;
                summed.broadcast_div(&counts)?
            }
        };
        let norm = pooled
            .sqr()?
            .sum_keepdim(1)?
            .sqrt()?
            .clamp(1e-12, f64::MAX)?;
        let unit = pooled.broadcast_div(&norm)?;
        Ok(unit.to_vec2::<f32>()?)
    }
}
