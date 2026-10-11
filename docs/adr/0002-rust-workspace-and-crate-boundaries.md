# ADR-0002: One Rust workspace with consensus code at the bottom

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

The same network is run by a desktop client, a headless node on servers and
light clients. All of them must agree byte for byte on the chain rules, the
signatures and the wire format: two implementations that disagree split the
network. At the same time, local inference pulls in a C++ toolchain
(llama.cpp) that a relay or a light client does not need.

## Options considered

### Option 1: one Rust workspace, layered crates, features for inference

- Benefits: every binary links the same consensus code; one lockfile; one
  test run covers the protocol end to end; memory safety without a garbage
  collector on phones and small servers.
- Drawbacks: compile times grow with the workspace; one language for every
  layer.

### Option 2: separate repositories per component

- Benefits: independent release cycles.
- Drawbacks: protocol and chain code duplicated or versioned as a library
  across repositories; easy to ship two clients that disagree.

### Option 3: a different language per layer (for example Go or TypeScript for the client)

- Benefits: faster UI iteration.
- Drawbacks: the consensus rules would exist twice and drift.

## Decision

One Cargo workspace. Crates depend only downwards:

| Crate | Role | Depends on |
|---|---|---|
| `core-crypto` | Chain, pallets, lottery, signatures, Shamir, light sync | none of the others |
| `mesh-network` | QUIC transport and wire protocol | `core-crypto` |
| `ai-engine` | Inference backends and redaction | `core-crypto` |
| `recall` | On-device conversation memory | none of the others |
| `nepheral-api` | OpenAI- and Ollama-compatible HTTP surface | none of the others |
| `app-kernel` | Node runtime (library) and the headless `nepheral-node` | all of the above |
| `nepheral-desktop` | Tauri desktop shell | `app-kernel` and below |
| `utils` | Logging setup | none |

Local inference is opt-in through Cargo features (`local-inference*` in
`ai-engine`, `node_only_local`, `client-cpu` and the GPU variants in
`app-kernel`), so a node without a C++ toolchain can still be built.

### Key points

- **Consensus at the bottom:** `core-crypto` has no dependency on the
  transport or the runtime, so it can be tested and audited alone.
- **Features, not forks:** the backend is chosen at build time instead of
  through separate code bases.

## Consequences

### Positive

- One definition of every rule and message, shared by every binary.
- The protocol can be tested end to end inside one test run.

### Tradeoffs

- Full builds are slow, and builds with local inference need cmake and
  clang.
- Moving a crate out of the workspace later means publishing it as a
  versioned library.

## Implementation

### Where it lives

- `Cargo.toml` (workspace members and shared dependency versions).
- `app-kernel/Cargo.toml` and `nepheral-desktop/Cargo.toml` (feature
  matrix, see `BUILD.md`).

## Related decisions

- ADR-0007: the chain that lives in `core-crypto`.
- ADR-0014: the inference backends behind the features.

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
