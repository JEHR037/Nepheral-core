# Code and testing

- **Status:** Accepted
- **Date:** 2026-10-10

## Rust

- Format with `cargo fmt`; the CI check fails on any difference.
- `cargo clippy` with warnings as errors, for every target. Allow a lint
  locally (`#[allow(...)]`) only with a comment that says why.
- Stable toolchain, edition 2021.
- Crates depend only downwards (ADR-0002). `core-crypto` never depends on
  the transport or the runtime.

## Errors and panics

- Return errors for anything that can fail in normal operation: input from
  the network, files on disk, configuration, other processes.
- Never `unwrap` or `expect` on data from a peer or from the disk. They are
  acceptable for invariants that hold by construction; add a short comment
  when that is not obvious.
- A peer must not be able to make a node panic. Size caps are checked
  before decoding (`mesh-network/src/protocol.rs`).

## Unsafe code

- Only where it is unavoidable: foreign functions (llama.cpp, Windows APIs)
  and memory-mapped model files.
- Every `unsafe` block carries a `// SAFETY:` comment that says why it is
  sound.

## Comments

- Explain why, not what. A comment that repeats the code is noise.
- Module comments (`//!`) say what the module is for and which invariants
  it keeps.
- No commented-out code, no references to private documents or tickets.
- English only. Text shown to users lives in the translation files.

## Consensus and protocol changes

- Anything that changes how a block is applied or how state is hashed is
  consensus-critical. It ships behind a rule version (ADR-0009), with tests
  that replay the chain before and after the activation.
- Anything that changes the bytes on the wire updates
  `mesh-network/tests/golden_wire.rs` deliberately, and keeps older peers
  working inside the transition window.

## Tests

- Unit tests sit next to the code, in a `#[cfg(test)] mod tests` block at
  the end of the module.
- Tests that need several crates or several nodes go in the crate's
  `tests/` directory (for example `app-kernel/tests/mesh_e2e.rs`).
- New behaviour comes with tests; a fixed bug comes with the test that
  would have caught it.
- Tests are deterministic: no dependence on the live network, on wall-clock
  timing beyond generous bounds, or on files outside the repository.
- Tests that need a model file or special hardware are marked `#[ignore]`
  with the reason, and documented in the crate's README.
- Review tests as carefully as code. A test that cannot fail is worse than
  no test, because it looks like coverage.
