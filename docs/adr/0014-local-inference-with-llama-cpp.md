# ADR-0014: Local inference with llama.cpp, backend chosen at build time

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

Responders run on whatever their owners have: laptops without a GPU, gaming
PCs with NVIDIA or AMD cards, servers with several GPUs. Models are
distributed openly, mostly as quantised GGUF files. The node has to run them
in process, without asking the owner to install and operate a separate
inference server.

## Options considered

### Option 1: llama.cpp through Rust bindings

- Benefits: runs GGUF models on CPU, Vulkan, CUDA and Metal; the widest
  model coverage; quantisation that fits consumer memory.
- Drawbacks: a C++ dependency compiled with cmake and clang; large
  binaries with GPU backends.

### Option 2: a pure-Rust inference stack

- Benefits: no C++ toolchain.
- Drawbacks: narrower model and quantisation coverage, slower on consumer
  GPUs.

### Option 3: require an external server (Ollama, vLLM)

- Benefits: no inference code in the node.
- Drawbacks: one more program for every operator to install, update and
  secure.

## Decision

Nodes run models in process with llama.cpp through the `llama-cpp-2`
bindings. The backend is chosen at build time with Cargo features: CPU,
Vulkan, CUDA, or `gpu-auto`, which loads CUDA or Vulkan at run time and falls
back to the CPU. Builds without local inference remain possible and dispatch
every question to the mesh. External OpenAI-compatible HTTP backends are
supported as well.

### Key points

- **Device choice:** integrated GPUs that are slower than the CPU are
  skipped, and a card seen twice by different backends is used once.
- **Several GPUs:** split one model across them, or run a copy on each to
  answer several questions at once.
- **Verified models:** a node accepts model transfers only for a model its
  user requested, whose hash is published in the catalog.
- **Reasoning budget:** reasoning is off unless requested, and the reply
  budget is kept for the answer.

## Consequences

### Positive

- Any consumer machine with enough memory can answer.
- Model choice follows the open GGUF ecosystem.

### Tradeoffs

- Release builds need cmake, clang and, for GPU builds, the CUDA toolkit or
  the Vulkan SDK.
- Each backend is a separate binary to build and ship.

## Implementation

### Where it lives

- `ai-engine/src/local_backend.rs`, `device_choice.rs`, `reply_budget.rs`,
  `gguf_probe.rs`, `model_catalog.rs`, `external_backend.rs`.
- `ai-engine/Cargo.toml`: the `local-inference*` features.

## Related decisions

- ADR-0002: features instead of separate code bases.
- ADR-0011: one reserved slot per worker.

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
