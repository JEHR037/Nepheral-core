# Third-party notices

Nepheral's own code is under BUSL-1.1 (`LICENSE` at the root of the
repository). This folder holds everything else: what Nepheral builds on,
under the licences its authors chose, and the texts those licences require
to travel with it.

Nepheral builds on third-party software distributed under its own
licences. The full text of every dependency's licence, and which crates
use it, is in `THIRD-PARTY-LICENSES.html` in this folder, generated from
`Cargo.lock` before each release with
`cargo about generate about.hbs -o legal/THIRD-PARTY-LICENSES.html`.

## llama.cpp

Local inference links llama.cpp through the `llama-cpp-2` and
`llama-cpp-sys-2` crates.

Copyright (c) 2023-2026 The ggml authors. Licensed under the MIT License.

## NVIDIA CUDA runtime

CUDA builds may redistribute NVIDIA CUDA runtime libraries (for example
`cudart` and `cublas`). They are distributed under the NVIDIA CUDA
Toolkit End User License Agreement and remain the property of NVIDIA
Corporation.

## Model weights

Language models (GGUF) are downloaded by the user from their original
publishers and remain under the licence each publisher chose. Nepheral
does not redistribute them.

The one exception is the model behind NAI's private memory
(`nai-memory-v1.safetensors`, served from Nepheral's download server):

- **Granite Embedding 107M Multilingual** (`ibm-granite/granite-embedding-107m-multilingual`),
  Copyright IBM Corp. Licensed under the Apache License, Version 2.0.
- Modified by Nepheral: the word-embedding table is stored as 8-bit
  integers with one scale per row, and the tokenizer and configuration are
  stored inside the same file (`recall/examples/pack.rs` makes it). The
  other weights are as published.
- The licence text and this notice travel with the file:
  `Apache-2.0.txt` and `nai-memory-v1.NOTICE.txt` in this folder,
  and next to the file on the download server.

## candle and tokenizers

The memory model runs in-process with Hugging Face's `candle` (MIT or
Apache-2.0) and `tokenizers` (Apache-2.0) crates.
