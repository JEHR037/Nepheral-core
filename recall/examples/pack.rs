// SPDX-License-Identifier: BUSL-1.1
//! Packs a Hugging Face XLM-RoBERTa embedding model into the one file the
//! app downloads: the word table as 8-bit integers with one scale per row,
//! every other tensor as published, and `config.json` and `tokenizer.json`
//! inside as bytes. Prints the size and SHA-256 to pin in the app.
//!
//! cargo run --release -p nepheral-recall --example pack -- <model dir> <out file>

use std::collections::HashMap;
use std::path::PathBuf;

use nepheral_recall::embed::{PACKED_CONFIG, PACKED_TOKENIZER, WORDS, WORD_SCALES};
use safetensors::tensor::TensorView;
use safetensors::{Dtype, SafeTensors};
use sha2::{Digest, Sha256};

fn to_f32(dtype: Dtype, raw: &[u8]) -> Vec<f32> {
    match dtype {
        Dtype::BF16 => raw
            .chunks_exact(2)
            .map(|c| half::bf16::from_le_bytes([c[0], c[1]]).to_f32())
            .collect(),
        Dtype::F16 => raw
            .chunks_exact(2)
            .map(|c| half::f16::from_le_bytes([c[0], c[1]]).to_f32())
            .collect(),
        Dtype::F32 => raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        other => panic!("word table in {other:?}"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("model dir"));
    let out = PathBuf::from(args.next().expect("out file"));
    let weights = std::fs::read(dir.join("model.safetensors")).unwrap();
    let config = std::fs::read(dir.join("config.json")).unwrap();
    let tokenizer = std::fs::read(dir.join("tokenizer.json")).unwrap();
    let st = SafeTensors::deserialize(&weights).unwrap();

    let mut tensors: Vec<(String, Dtype, Vec<usize>, Vec<u8>)> = Vec::new();
    let mut worst = 0f32;
    for (name, view) in st.tensors() {
        if name != WORDS {
            tensors.push((
                name,
                view.dtype(),
                view.shape().to_vec(),
                view.data().to_vec(),
            ));
            continue;
        }
        let (rows, width) = (view.shape()[0], view.shape()[1]);
        let values = to_f32(view.dtype(), view.data());
        let mut q = Vec::with_capacity(rows * width);
        let mut scales = Vec::with_capacity(rows * 4);
        for row in values.chunks_exact(width) {
            let max = row.iter().fold(0f32, |m, x| m.max(x.abs()));
            let scale = if max > 0.0 { max / 127.0 } else { 1.0 };
            for &x in row {
                let v = (x / scale).round().clamp(-127.0, 127.0);
                worst = worst.max((v * scale - x).abs());
                q.push(v as i8 as u8);
            }
            scales.extend_from_slice(&scale.to_le_bytes());
        }
        tensors.push((name, Dtype::I8, vec![rows, width], q));
        tensors.push((WORD_SCALES.to_string(), Dtype::F32, vec![rows], scales));
    }
    tensors.push((
        PACKED_CONFIG.to_string(),
        Dtype::U8,
        vec![config.len()],
        config,
    ));
    tensors.push((
        PACKED_TOKENIZER.to_string(),
        Dtype::U8,
        vec![tokenizer.len()],
        tokenizer,
    ));

    let meta = HashMap::from([
        ("format".to_string(), "nepheral-recall".to_string()),
        ("version".to_string(), "1".to_string()),
        (
            "source".to_string(),
            "ibm-granite/granite-embedding-107m-multilingual (Apache-2.0)".to_string(),
        ),
    ]);
    let views = tensors
        .iter()
        .map(|(n, d, s, b)| (n.clone(), TensorView::new(*d, s.clone(), b).unwrap()));
    safetensors::serialize_to_file(views, Some(meta), &out).unwrap();

    let bytes = std::fs::read(&out).unwrap();
    let hash = Sha256::digest(&bytes);
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    println!(
        "{} bytes, sha256 {hex}, worst word-table error {worst:.6}",
        bytes.len()
    );
}
