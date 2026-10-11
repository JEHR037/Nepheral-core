// SPDX-License-Identifier: BUSL-1.1
//! An XLM-RoBERTa encoder that leaves its vocabulary on disk.
//!
//! A multilingual vocabulary is most of a small model: 250 000 words of 384
//! numbers, ~190 MB stored and ~380 MB once in memory, against ~45 MB for the
//! six layers that do the work. The word table stays in the mapped weights
//! file and only the rows a text uses are read, so the model costs the
//! phone little more than its layers. The math is the same as
//! `candle_transformers::models::xlm_roberta`, which the bench checks.

use std::path::Path;

use candle_core::{DType, Device, Module, Tensor};
use candle_nn::{
    layer_norm, linear, ops::softmax_last_dim, Activation, LayerNorm, Linear, VarBuilder,
};
use memmap2::Mmap;
use safetensors::{Dtype, SafeTensors};

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Config {
    pub hidden_size: usize,
    pub num_attention_heads: usize,
    pub intermediate_size: usize,
    pub hidden_act: Activation,
    pub num_hidden_layers: usize,
    pub max_position_embeddings: usize,
    pub type_vocab_size: usize,
    pub layer_norm_eps: f64,
    pub pad_token_id: u32,
}

pub const WORDS: &str = "embeddings.word_embeddings.weight";
/// Per-row scales of a word table stored as 8-bit integers (`pack`).
pub const WORD_SCALES: &str = "embeddings.word_embeddings.scale";

/// The word table, read row by row from the mapped file.
struct WordRows {
    map: Mmap,
    offset: usize,
    rows: usize,
    width: usize,
    dtype: Dtype,
    /// For 8-bit rows: where the f32 scales start.
    scales: Option<usize>,
}

impl WordRows {
    fn open(path: &Path, hidden: usize) -> candle_core::Result<Self> {
        let file = std::fs::File::open(path)?;
        // SAFETY: the app's own copy of the weights, only read, and replaced
        // by renaming a new file into place, never by writing over it.
        let map = unsafe { Mmap::map(&file)? };
        let (offset, rows, dtype, scales) = {
            let st = SafeTensors::deserialize(&map).map_err(candle_core::Error::wrap)?;
            let view = st.tensor(WORDS).map_err(candle_core::Error::wrap)?;
            let start = |data: &[u8]| data.as_ptr() as usize - map.as_ptr() as usize;
            if view.shape().len() != 2 || view.shape()[1] != hidden {
                candle_core::bail!("{WORDS}: shape {:?}", view.shape());
            }
            let rows = view.shape()[0];
            let scales = match view.dtype() {
                Dtype::BF16 | Dtype::F16 | Dtype::F32 => None,
                Dtype::I8 => {
                    let s = st.tensor(WORD_SCALES).map_err(candle_core::Error::wrap)?;
                    if s.dtype() != Dtype::F32 || s.shape() != [rows] {
                        candle_core::bail!("{WORD_SCALES}: {:?} {:?}", s.dtype(), s.shape());
                    }
                    Some(start(s.data()))
                }
                other => candle_core::bail!("{WORDS}: dtype {other:?}"),
            };
            (start(view.data()), rows, view.dtype(), scales)
        };
        Ok(Self {
            map,
            offset,
            rows,
            width: hidden,
            dtype,
            scales,
        })
    }

    fn gather(&self, ids: &[u32], out: &mut Vec<f32>) {
        let size = match self.dtype {
            Dtype::F32 => 4,
            Dtype::I8 => 1,
            _ => 2,
        };
        let bytes = self.width * size;
        for &id in ids {
            let row = (id as usize).min(self.rows - 1);
            let start = self.offset + row * bytes;
            let raw = &self.map[start..start + bytes];
            match (self.dtype, self.scales) {
                (Dtype::I8, Some(scales)) => {
                    let at = scales + row * 4;
                    let s = &self.map[at..at + 4];
                    let scale = f32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                    out.extend(raw.iter().map(|&q| q as i8 as f32 * scale));
                }
                (Dtype::BF16, _) => out.extend(
                    raw.chunks_exact(2)
                        .map(|c| half::bf16::from_le_bytes([c[0], c[1]]).to_f32()),
                ),
                (Dtype::F16, _) => out.extend(
                    raw.chunks_exact(2)
                        .map(|c| half::f16::from_le_bytes([c[0], c[1]]).to_f32()),
                ),
                _ => out.extend(
                    raw.chunks_exact(4)
                        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
                ),
            }
        }
    }
}

struct Layer {
    query: Linear,
    key: Linear,
    value: Linear,
    attention_out: Linear,
    attention_norm: LayerNorm,
    intermediate: Linear,
    act: Activation,
    output: Linear,
    output_norm: LayerNorm,
}

impl Layer {
    fn load(cfg: &Config, vb: VarBuilder) -> candle_core::Result<Self> {
        let h = cfg.hidden_size;
        let a = vb.pp("attention");
        Ok(Self {
            query: linear(h, h, a.pp("self").pp("query"))?,
            key: linear(h, h, a.pp("self").pp("key"))?,
            value: linear(h, h, a.pp("self").pp("value"))?,
            attention_out: linear(h, h, a.pp("output").pp("dense"))?,
            attention_norm: layer_norm(h, cfg.layer_norm_eps, a.pp("output").pp("LayerNorm"))?,
            intermediate: linear(h, cfg.intermediate_size, vb.pp("intermediate").pp("dense"))?,
            act: cfg.hidden_act,
            output: linear(cfg.intermediate_size, h, vb.pp("output").pp("dense"))?,
            output_norm: layer_norm(h, cfg.layer_norm_eps, vb.pp("output").pp("LayerNorm"))?,
        })
    }

    fn forward(&self, x: &Tensor, mask: &Tensor, heads: usize) -> candle_core::Result<Tensor> {
        let (b, t, h) = x.dims3()?;
        let d = h / heads;
        let split = |y: Tensor| {
            y.reshape((b, t, heads, d))?
                .permute((0, 2, 1, 3))?
                .contiguous()
        };
        let q = split(self.query.forward(x)?)?;
        let k = split(self.key.forward(x)?)?;
        let v = split(self.value.forward(x)?)?;
        let scores = (q.matmul(&k.transpose(2, 3)?)? * (1.0 / (d as f64).sqrt()))?;
        let probs = softmax_last_dim(&scores.broadcast_add(mask)?)?;
        let context = probs
            .matmul(&v)?
            .permute((0, 2, 1, 3))?
            .contiguous()?
            .reshape((b, t, h))?;
        let attended = self
            .attention_norm
            .forward(&(self.attention_out.forward(&context)? + x)?)?;
        let inner = self.act.forward(&self.intermediate.forward(&attended)?)?;
        self.output_norm
            .forward(&(self.output.forward(&inner)? + &attended)?)
    }
}

pub struct Encoder {
    words: WordRows,
    positions: Tensor,
    token_type: Tensor,
    embed_norm: LayerNorm,
    layers: Vec<Layer>,
    cfg: Config,
    device: Device,
}

impl Encoder {
    pub fn load(weights: &Path, cfg: Config) -> candle_core::Result<Self> {
        let device = Device::Cpu;
        // Every tensor but the word table, converted to f32 as it is read.
        // SAFETY: as for `WordRows::open`.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[weights], DType::F32, &device)? };
        let e = vb.pp("embeddings");
        let h = cfg.hidden_size;
        let positions = e
            .pp("position_embeddings")
            .get((cfg.max_position_embeddings, h), "weight")?;
        let token_type = e
            .pp("token_type_embeddings")
            .get((cfg.type_vocab_size, h), "weight")?
            .narrow(0, 0, 1)?
            .reshape((1, 1, h))?;
        let embed_norm = layer_norm(h, cfg.layer_norm_eps, e.pp("LayerNorm"))?;
        let layers = (0..cfg.num_hidden_layers)
            .map(|i| Layer::load(&cfg, vb.pp(format!("encoder.layer.{i}"))))
            .collect::<candle_core::Result<Vec<_>>>()?;
        Ok(Self {
            words: WordRows::open(weights, h)?,
            positions,
            token_type,
            embed_norm,
            layers,
            cfg,
            device,
        })
    }

    pub fn hidden_size(&self) -> usize {
        self.cfg.hidden_size
    }

    /// The first token's vector for each sequence (`ids` padded to one
    /// length, `mask` 1 for real tokens).
    pub fn cls(&self, ids: &[Vec<u32>], mask: &[Vec<u32>]) -> candle_core::Result<Tensor> {
        let b = ids.len();
        let t = ids.first().map_or(0, Vec::len);
        let h = self.cfg.hidden_size;
        let pad = self.cfg.pad_token_id;
        let mut words = Vec::with_capacity(b * t * h);
        let mut positions = Vec::with_capacity(b * t);
        for seq in ids {
            self.words.gather(seq, &mut words);
            // As RoBERTa counts them: real tokens from pad + 1, padding at pad.
            let mut n = 0;
            positions.extend(seq.iter().map(|&id| {
                if id == pad {
                    pad
                } else {
                    n += 1;
                    (pad + n).min(self.cfg.max_position_embeddings as u32 - 1)
                }
            }));
        }
        let words = Tensor::from_vec(words, (b, t, h), &self.device)?;
        let positions = self
            .positions
            .index_select(&Tensor::from_vec(positions, b * t, &self.device)?, 0)?
            .reshape((b, t, h))?;
        let mut x = self
            .embed_norm
            .forward(&(words + positions)?.broadcast_add(&self.token_type)?)?;
        let keep: Vec<f32> = mask.iter().flatten().map(|&m| m as f32).collect();
        // 0 where a token is attended to, a large negative where it is padding.
        let mask = ((Tensor::from_vec(keep, (b, 1, 1, t), &self.device)? - 1.0)? * 1e9)?;
        for layer in &self.layers {
            x = layer.forward(&x, &mask, self.cfg.num_attention_heads)?;
        }
        x.narrow(1, 0, 1)?.squeeze(1)
    }
}
