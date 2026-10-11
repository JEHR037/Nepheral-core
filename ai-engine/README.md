# ai-engine

Inference engine, model management and sender-side redaction for Nepheral.
It runs a model in-process (native `llama-cpp-2`) or talks to an external
`/api/chat`-compatible endpoint, sizes the model to the machine, and
provides the `Redactor` that `app-kernel` applies to a prompt before it
leaves the sender's node.

## Privacy model

Redaction happens on the **sender**, not in the engine. `app-kernel`
(`agent.rs`, `rooms.rs`) runs every outgoing prompt, context window and
system prompt through a `Redactor` before encrypting and dispatching it to
the mesh, and restores the placeholders in the reply it gets back. The
responding node's `InferenceEngine` sees placeholders in place of the
detected values:

```
User text  --Redactor::redact-->  "Mail [[EMAIL_1]] from [[IP_1]]"
                                        |  (encrypted, Shamir-split, answered remotely)
Reply      <--Redactor::restore--  "...sent to [[EMAIL_1]]..."
```

## Modules

### `redaction` — `Redactor`

Deterministic, per-conversation placeholder map. The same value always maps
to the same placeholder within one `Redactor`, so a multi-turn conversation
stays coherent. Desktop agent sessions and rooms each keep one `Redactor`
for their lifetime (`AppState::session_redactors` / `room_redactors`).

| Item | Description |
|---|---|
| `Redactor::new()` | Empty redactor (pattern detection only) |
| `Redactor::with_terms(terms)` | Also redacts user-supplied terms (case-insensitive, whole-word), e.g. names from the `redact_terms` config key |
| `redact(&mut self, text) -> String` | Replaces detected entities with `[[LABEL_N]]` placeholders |
| `restore(&self, text) -> String` | Maps known placeholders back to the originals; unknown ones are left as-is |
| `entity_count()` | Number of distinct values redacted so far |
| `EntityKind` | `Email`, `Phone`, `Iban`, `Card`, `Ipv4`, `Ipv6`, `Uuid`, `Secret`, `Path`, `Url`, `Term` |

Placeholder labels: `EMAIL`, `PHONE`, `IBAN`, `CARD`, `IP` (v4 and v6), `ID`
(UUIDs), `SECRET`, `PATH`, `URL`, `NAME` (custom terms).

Detection covers email addresses, phone numbers, IBANs (checksum-verified),
card numbers (Luhn-verified), IPv4/IPv6 addresses, UUIDs, secrets (known
API-key prefixes, JWT-shaped tokens, long hex or mixed-case base64-like
strings), home-directory paths (`/home/...`, `/Users/...`, `C:\Users\...`)
and URLs carrying `user:password@` credentials. The map is zeroized on drop.

### `inference_engine` — `InferenceEngine`

Top-level orchestrator over one `InferenceBackend`.

- `new(config)` loads the local GGUF from `model_path` when the
  `local-inference` feature is on and the file exists; otherwise it uses
  `ExternalHttpBackend` at `backend_url`. A local model that fails to load
  is **not** silently replaced by the external backend: the load error is
  kept and returned by `initialize()`. With neither a model nor a backend
  URL, `initialize()` reports that no model is configured.
- `new_with_backend(config, backend)` injects any backend (used by tests).
- State machine (`EngineState`): `Uninitialized -> Connecting -> Ready`,
  `Ready <-> Processing`, or `Error(reason)`.

| Method | Description |
|---|---|
| `initialize()` | Health-checks the backend and moves to `Ready` |
| `infer_stateless(messages, temperature) -> StatelessReply` | One-shot inference over a caller-supplied message list. Keeps no session history and zeroizes the messages afterwards. Used for mesh queries, room replies and the local API |
| `chat_message(session_id, text)` / `process_request(req)` | Non-streaming chat with per-session history |
| `chat_message_stream(session_id, text)` | Streaming chat; yields `StreamChunk::{Thinking, Response, Done, Error}` |
| `record_assistant_turn()` | Appends a streamed reply to the session history |
| `set_system_prompt()` / `get_system_prompt()` | System prompt prepended to every session request |
| `decay_sessions()` / `active_sessions()` / `terminate_session()` | Ephemeral session bookkeeping (`EphemeralConversation`) |
| `get_state()` / `get_config()` / `set_config()` | Accessors |
| `shutdown()` | Cancels generation and zeroizes histories and the system prompt |

`StatelessReply { text, output_tokens, gen_ms, model_name }` carries the
figures a responder puts into its signed `ServiceClaim` (see
`core-crypto/src/receipts.rs`).

Only one inference runs at a time (an internal slot lock). History is
trimmed to fit the context window, leaving room for the reply.

### Backends (`backend`, `local_backend`, `external_backend`)

`InferenceBackend` trait: `health_check`, `chat`, `chat_stream` (default:
wraps `chat`), `backend_name`, `cancel`.

**`LlamaLocalBackend`** (feature `local-inference`) — loads a GGUF via
`llama-cpp-2` and runs in-process. Streams tokens, splits
`<think>…</think>` into `Thinking` chunks, and takes `context_window` /
`n_ubatch` / `max_tokens` from `ModelConfig`.

**`ExternalHttpBackend`** — speaks the `/api/{tags,generate,chat,pull}`
HTTP shape, so it can point at any compatible endpoint.
`new_authenticated(url, peer_id)` attaches a peer id to requests.

### `gguf_probe`

Reads a GGUF header (`probe(path)` / `parse(bytes)` -> `GgufInfo`) to report
version, tensor count, declared architecture and tensor types.
`GgufInfo::runnable_with(tensor_type_count())` answers `Runnable::{Yes,
No { unsupported }, Unknown}` against the tensor formats compiled into this
build. Also used to check that a model received over the mesh is a real GGUF.
`ai_engine::tensor_type_count()` returns ggml's `GGML_TYPE_COUNT`, or `None`
without `local-inference`.

### `model_catalog`

Browses GGUF repositories on Hugging Face for the desktop model picker.

| Item | Description |
|---|---|
| `search_repos(query, limit)` | GGUF text-generation repos (empty query = most downloaded) |
| `list_files(repo)` | A repo's GGUF files, each with `sha256_hex` from the LFS object id |
| `probe_published_header(url)` | Reads the header of a remote GGUF without downloading it |
| `is_trusted(repo_id)` / `TRUSTED_ORGS` | Built-in trusted-publisher list |
| `parse_params_b()` / `parse_quant()` | Parameter count and quantization from a name |
| `assess(...) -> FitReport` / `Fit` | `Comfortable`, `Tight` or `TooBig` for this machine's RAM/VRAM |

### `model_store`

Downloads and verifies GGUF files under `~/.nepheral/models/`.

| Item | Description |
|---|---|
| `ModelStore::new(base)` / `default_dir()` | Rooted at a base directory (`~/.nepheral` by default) |
| `download_model(url, file_name, expected_sha256_hex, progress)` | Resumable download into `models/<file_name>`, hashed incrementally and checked against the publisher's SHA-256 |
| `installed()` / `models_dir()` | Lists downloaded models (`InstalledModel`) |
| `migrate_legacy_model()` | One-time move of `<base>/model.gguf` into `models/` |
| `local_file_name(path)` | Reduces a published path to a safe flat file name (rejects traversal) |
| `parse_hf_url()` / `decode_sha256_hex()` | URL and checksum helpers |
| `MIN_PLAUSIBLE_MODEL_BYTES` | Size below which a file is not treated as a model (20 MiB) |

### `hw_detect`

Available on every build (no `local-inference` needed for CPU/RAM sizing).

| Item | Description |
|---|---|
| `detect() -> HardwareProfile` | CPU cores, RAM, detected GPU and a recommended `ModelTier` |
| `recommend_tuning(profile, tier) -> InferenceTuning` | `n_ctx` / `n_ubatch` / `max_new_tokens` sized to free memory |
| `tuning_for_model(model_bytes, kv_bytes_per_token, ram_mb)` | Same, for a specific model file |
| `inference_budget_mb(ram_mb)` | Memory available to inference after OS overhead |
| `GpuKind` | `None`, `Vulkan`, `Cuda`, `Metal` |

On a `local-inference` build, GPU detection enumerates the GGML backend
devices actually loaded into the process and reports their real name and
VRAM. Without that feature only a fixed macOS/Metal fallback applies.
`NEPHERAL_VRAM_MB` overrides the probed VRAM.

### `types`

`ModelConfig`, `Message` / `MessageRole`, `StreamChunk`, `FinishReason`,
`EngineState`, `InferenceRequest` / `InferenceResponse`,
`EphemeralConversation`, `RestrictedKnowledgeConfig`.

```rust
pub struct ModelConfig {
    pub model_name: String,         // default "local"
    pub backend_url: String,        // external endpoint; default empty
    pub model_path: Option<String>, // local GGUF path; default None
    pub temperature: f32,           // default 0.7
    pub max_tokens: usize,          // default 2048
    pub context_window: usize,      // n_ctx, set by the hw_detect tuner
    pub n_ubatch: u32,              // set by the hw_detect tuner
}
```

The default configuration names no model and no backend.

## Features

| Feature | Pulls | Notes |
|---|---|---|
| `local-inference` | `llama-cpp-2`, `llama-cpp-sys-2`, `encoding_rs`, `core-crypto` | Enables `LlamaLocalBackend`. Needs cmake + clang/LLVM (bindgen) on the build host |
| `local-inference-cuda` / `-vulkan` / `-metal` | `local-inference` + the matching `llama-cpp-2` backend | One GPU backend, statically linked |
| `dynamic-backends` | `llama-cpp-2/dynamic-backends` | Loads GGML backends as runtime modules |
| `local-inference-auto` | `local-inference` + `dynamic-backends` + cuda + vulkan | One binary, runtime GPU pick, CPU fallback |

## Configuration from `app-kernel`

The headless `nepheral-node` builds its `ModelConfig` from the environment:

```sh
NEPHERAL_MODEL_PATH=~/.nepheral/models/<file>.gguf  # local GGUF (takes precedence)
NEPHERAL_INFERENCE_URL=http://host:port             # external backend (alias: NEPHERAL_AI_URL)
NEPHERAL_AI_MODEL=<name>                            # model name reported for either
```
