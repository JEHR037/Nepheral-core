# Building Nepheral

Nepheral ships in two binaries from a single Rust workspace, each with
several Cargo features that switch backends and bundle scope. This document
maps the feature matrix to concrete build commands per platform.

---

## Binaries

| Binary | Built from | What it does |
|---|---|---|
| `nepheral-node` | `app-kernel/src/main.rs` | Headless authority / responder / bootstrap relay. Joins mesh, optionally serves inference, signs blocks if it has the authority key. **This is what runs on the authority and bootstrap servers.** |
| `nepheral-desktop` | `nepheral-desktop/` (Tauri 2 webview shell over `app-kernel/src/client_node.rs`) | Desktop client. Joins mesh as a peer, includes local API daemon (`localhost:11434`) speaking OpenAI shape. **This is what runs on your machine.** |

Build:

```powershell
cargo build --release -p nepheral-desktop
# needs WebView2 (preinstalled on Windows 10/11); on Linux: webkit2gtk
```

`nepheral-desktop`'s default feature (`cpu`) enables in-process llama-cpp
CPU inference (`app-kernel/client-cpu`), which needs cmake + clang/LLVM on
the build host — see the feature matrix and per-platform commands below.
Build with `--no-default-features` for a fast, toolchain-free client that
dispatches all inference to mesh peers instead of running it locally.


---

## Feature matrix

Defined in `app-kernel/Cargo.toml`. Combine with the binary name to build.

| Feature | Pulls | Notes |
|---|---|---|
| `node_runtime` | mesh runtime only | Implied by `node_only` and `client`; never selected directly. Gates `app-kernel/src/node_runtime.rs`, the single QUIC/wire-protocol implementation both the headless binary and the desktop client run. Pulls no HTTP stack. |
| `node_only` | `node_runtime` | No inference, no UI. Mesh + chain only. |
| `node_only_local` | `node_only` + `ai-engine/local-inference` | Headless + llama-cpp CPU inference. **The authority server uses this.** |
| `client` (app-kernel) | `node_runtime` + nepheral-api + `local-api-server` | Full desktop AppState (wallet, sessions, rooms) + local API daemon. No local inference. This is what `nepheral-desktop --no-default-features` builds against. |
| `client-cpu` (app-kernel) | `client` + `ai-engine/local-inference` | `client` + CPU inference. Requires LLVM/clang + cmake. This is `nepheral-desktop`'s default (`cpu`) feature. |

`local-inference*` is the family of features that pulls `llama-cpp-2`, which
compiles native C++ via `llama-cpp-sys-2`. Those builds need a C++ toolchain
on the build host.

---

## Build commands per platform

### Linux server / authority node

```bash
cargo build --release -p app-kernel \
  --features node_only_local \
  --bin nepheral-node
```

Required: cmake, clang, libssl-dev. The hardcoded `NEPHERAL_TEAM_PUBKEY` in
[core-crypto/src/chain.rs](core-crypto/src/chain.rs) is baked at
compile time; the matching signing-seed comes from the
`NEPHERAL_AUTHORITY_KEY` runtime env var. A lightweight bootstrap/relay
server can be set up with the installer instead — see
[Running a headless node on a Linux server](#running-a-headless-node-on-a-linux-server-bootstraprelay)
below.

### Linux/Mac desktop

```bash
cargo build --release -p nepheral-desktop
```

Required: cmake + clang (default `cpu` feature builds in CPU inference).
CPU inference is slow (~5-10 tok/s on a modern x86), fine for occasional
use, not for production. WebView2-equivalent on Linux is `webkit2gtk`.

### Windows WITHOUT toolchain (fastest path to mesh)

```powershell
cargo build --release -p nepheral-desktop `
  --no-default-features
```

Required: Rust stable. Nothing else. Builds in 2-5 min.

Trade-off: client joins the mesh and the OpenAI API daemon comes up, but
this build does NOT include local inference. The client dispatches all
queries to other mesh peers (e.g. the authority node) and can never win a
lottery itself. Good enough for chatting, integrating with Continue.dev,
or testing the protocol. Not enough to earn credits by serving inference.

### Windows WITH CPU inference

```powershell
$env:LIBCLANG_PATH = "C:\Users\<you>\Documents\LLVM\bin"
cargo build --release -p nepheral-desktop
```

Required: LLVM (clang.dll / libclang.dll) installed and `LIBCLANG_PATH` set,
plus cmake. Download LLVM from <https://github.com/llvm/llvm-project/releases>.
Extract somewhere (e.g. `Documents\LLVM`) and point `LIBCLANG_PATH` at the
`bin/` subdir.

Build takes ~5-15 min the first time (recompiles llama.cpp + ggml).

### GPU builds (Vulkan/CUDA)

`nepheral-desktop` (`--no-default-features --features <name>`):

| Feature | Backend | Requires |
|---|---|---|
| `cpu` (default) | CPU | cmake + clang/LLVM |
| `vulkan` | Vulkan | Vulkan SDK |
| `cuda` | CUDA | CUDA toolkit (NVIDIA GPU) |
| `gpu-auto` | Runtime pick (CUDA, falling back to Vulkan, falling back to CPU) | Both the CUDA toolkit and the Vulkan SDK on the build host |

`nepheral-node` (`--features <name> --bin nepheral-node`, in place of
`node_only_local`):

| Feature | Backend | Requires |
|---|---|---|
| `node_only_local` | CPU | cmake + clang/LLVM |
| `node_only_vulkan` | Vulkan | Vulkan SDK |
| `node_only_cuda` | CUDA | CUDA toolkit (NVIDIA GPU) |
| `node_only_gpu_auto` | Runtime pick | Both the CUDA toolkit and the Vulkan SDK |

Pick exactly one backend feature per binary — they aren't meant to be
combined. A `gpu-auto` build links CUDA and Vulkan as loadable modules
instead of a single backend statically, so it must ship its `backends/`
output directory next to the executable.

---

## Identity & authority bootstrap

Two ed25519 keys matter at runtime:

| Key | Where it lives | What sets it |
|---|---|---|
| `NEPHERAL_TEAM_PUBKEY` (32 bytes) | Hardcoded in `core-crypto/src/chain.rs` as `NEPHERAL_TEAM_PUBKEY` constant. Compile-time env override available. | Authority seat in `ValidatorSet`. Every binary must agree on this value or blocks signed by the authority won't validate. |
| `NEPHERAL_AUTHORITY_KEY` (32 bytes hex seed) | Runtime env var on the authority server (e.g. a systemd `Environment=` line, or a secrets manager). NOT in code. | The signing seed for the authority node. `SigningKey::from_bytes(seed).verifying_key()` MUST equal `NEPHERAL_TEAM_PUBKEY`. |

To rotate (recover from a compromise):

1. Generate a new BIP39 mnemonic on a fresh nepheral-desktop install.
2. Derive both halves: `cargo run --example derive_pubkey --release -p core-crypto -- "<24 words>"`. That prints both the pubkey hex (for `NEPHERAL_TEAM_PUBKEY`) and the seed hex (for `NEPHERAL_AUTHORITY_KEY`).
3. Edit the `NEPHERAL_TEAM_PUBKEY` constant in `core-crypto/src/chain.rs` with the new pubkey.
4. Update `NEPHERAL_AUTHORITY_KEY` on the authority server and restart the service.
5. Rebuild and redeploy both binaries.
6. Existing chain blocks signed by the old authority are now invalid; new state starts from the next block.

---

## Verifying the chain is alive

### Headless server side

```bash
journalctl -u nepheral-node -f | grep "Block sealed"
```

Should produce lines like `Block sealed height=N ext=0 lot=0` every ~60s
(heartbeat), and `ext=N lot=N` lines when real traffic is moving.

### Desktop client side

Check the chain height / block count in the app's status bar. After a
fresh start the client backfills from genesis via `BLOCK_SYNC_REQUEST`
(proactive on connect, reactive on gap) and should reach the current head
within ~10 s even if the mesh is at thousands of blocks.

---

## Common build failures

| Error | Meaning | Fix |
|---|---|---|
| `error: failed to run custom build command for llama-cpp-sys-2` | C++ toolchain missing. | Install cmake + clang/LLVM. Or build `nepheral-desktop --no-default-features` if you don't need local inference. |
| `Unable to find libclang` | LLVM is installed but `LIBCLANG_PATH` not pointing at the right place. | `$env:LIBCLANG_PATH = "C:\…\LLVM\bin"`. |
| `Cannot decompress Edwards point` (at runtime, on `cargo run` or boot) | `NEPHERAL_AUTHORITY_KEY` is set to a value whose 32 bytes are not a valid ed25519 seed (e.g. a sr25519 key from Polkadot). | Use the `derive_pubkey` example to generate a matching pair from a BIP39 mnemonic. |
| `chain status height=0 blocks=0` for >2 min on an authority | Producer alive but never sealing, or `NEPHERAL_AUTHORITY_KEY` not matching the baked pubkey. | Ensure the binary is built from current `main`. Verify the pubkey/seed pair. |
| `Remote block rejected error=BadBlockHeight height=NNN` | Local chain head is behind, no sync handler firing. | Pull latest `main`, rebuild fresh (`cargo clean -p app-kernel`), restart. |
| Stale build cache produces old binary behavior after pulling | `cargo build` sometimes incrementally reuses pre-pull artifacts. | `cargo clean -p app-kernel` before rebuild. |

---

## API quick-start once running

The nepheral-desktop client starts a local OpenAI- and Ollama-compatible
daemon on `http://127.0.0.1:11434`.

**Authentication is required.** The token is generated with the wallet and
kept in its vault; config.json holds only its hash, which is what requests
are checked against - so the API answers while the wallet is locked. Copy
the token from the app's API tab once unlocked (headless:
`nepheral-node status` after `nepheral-node unlock`). Every request (except
`/healthz` and `/api/version`) must send it:

```bash
TOKEN=<token from the API tab>
AUTH="Authorization: Bearer $TOKEN"
```

### Headless wallet

A headless node runs with its own key and serves the network unattended.
The wallet that receives what it earns is managed at its command line:

```bash
nepheral-node wallet init      # 24 words (shown once) and a password
nepheral-node unlock           # opens it in the running node; links the node on chain
nepheral-node status           # node, wallet, balance, lock state
nepheral-node wallet send <address> <amount>   # shows the detail, asks to confirm
nepheral-node lock
```

It locks itself after `NEPHERAL_AUTO_LOCK_MINUTES` of inactivity (15 by
default, 0 for never). The commands reach the running node over
`~/.nepheral/control.sock` (owner-only), so run them as the node's user with
its HOME.

Because `Authorization` carries the API token, Top-Tier session tokens are
sent in their own header: `X-Nepheral-Session-Token`.

Cross-origin browser calls are blocked by default. To allow a page
served locally, add its origin to `local_api_server_allowed_origins`
in `config.json` (e.g. `["http://127.0.0.1:8080"]`).

Messages may carry `content` as a string or as a list of text parts;
image and audio parts are refused for now with an OpenAI-shaped error.
`temperature` and `max_tokens` (or `max_completion_tokens`; on the Ollama
routes `options.temperature` and `options.num_predict`) apply when the
model runs on this machine (`nepheral-local`); on the mesh the answering
node decides them. `stop` (`options.stop` on the Ollama routes) applies
to every route, streaming included.

Routes:

```bash
# List models the mesh can route
curl -H "$AUTH" http://127.0.0.1:11434/v1/models

# Single passthrough chat
curl http://127.0.0.1:11434/v1/chat/completions \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{
    "model": "nepheral-standard",
    "messages": [{"role":"user","content":"hello"}]
  }'

# Ollama-compatible surface (same daemon, same token)
curl -H "$AUTH" http://127.0.0.1:11434/api/tags
curl http://127.0.0.1:11434/api/chat \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"model":"nepheral-standard","messages":[{"role":"user","content":"hello"}]}'

# Open a Top-Tier session (burns credits, 10 inferences, 1 h TTL).
# Returns {"session_token": "<64 hex>"} once the tx lands in a block.
curl -X POST -H "$AUTH" http://127.0.0.1:11434/v1/sessions

# Use a Top-Tier model with the session token
curl http://127.0.0.1:11434/v1/chat/completions \
  -H "$AUTH" -H 'X-Nepheral-Session-Token: <64-hex-token>' \
  -H 'Content-Type: application/json' \
  -d '{"model":"nepheral-top-tier-chat","messages":[{"role":"user","content":"hello"}]}'

# Multi-lens code audit
curl http://127.0.0.1:11434/v1/chat/completions \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{
    "model": "nepheral-review-security",
    "messages": [{"role":"user","content":"<paste code here>"}]
  }'

# Long-doc summarize
curl http://127.0.0.1:11434/v1/chat/completions \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{
    "model": "nepheral-doc-summarize",
    "messages": [{"role":"user","content":"<paste long doc here>"}]
  }'

# Open-question research with N-angle ensemble
curl http://127.0.0.1:11434/v1/chat/completions \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{
    "model": "nepheral-research-open-question",
    "messages": [{"role":"user","content":"Why does X behave like Y?"}]
  }'

# Multi-file refactor
curl http://127.0.0.1:11434/v1/chat/completions \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{
    "model": "nepheral-dev-refactor",
    "messages": [{"role":"user","content":"instruction: use Result not Option\n\n=== file: src/foo.rs ===\nfn foo() -> Option<String> {...}\n\n=== file: src/bar.rs ===\nfn bar() -> Option<String> {...}"}]
  }'

# Current routing policy (CredsMode)
curl -H "$AUTH" http://127.0.0.1:11434/v1/routing

# Change routing policy
curl -X PUT http://127.0.0.1:11434/v1/routing \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"mode":"standard_only","total_deadline_ms":30000}'
```

For Continue.dev integration (`apiKey` = your API token):

```json
{
  "models": [{
    "title": "Nepheral",
    "provider": "openai",
    "model": "nepheral-standard",
    "apiBase": "http://127.0.0.1:11434/v1",
    "apiKey": "<local_api_server_token>"
  }]
}
```

---

## Project layout

```
core-crypto/    — chain, pallets, lottery, identity, validators, light sync
mesh-network/   — QUIC transport + wire protocol
ai-engine/      — llama-cpp + HTTP external backends
nepheral-api/   — OpenAI-compat daemon + 4 topological modes
app-kernel/     — client-node runtime (lib) + headless binary (nepheral-node)
recall/         — on-device conversation memory
nepheral-desktop/ — Tauri webview shell (the desktop client)
utils/          — logging init
docs/           — design documents (whitepaper, economy, governance, reputation, conversations)
legal/          — third-party notices and licence texts
```

---

## Running a headless node on a Linux server (bootstrap/relay)

For a plain VPS (Ubuntu, Debian, or Arch; x86_64 or arm64) that just needs
to join the mesh — e.g. as an additional bootstrap/relay so peers can find
each other and exchange peer lists without depending on a single node:

```bash
curl -fsSL https://dl.nepheral.net/install.sh -o install.sh
sudo bash install.sh [--accel cpu|vulkan|cuda]
```

Don't pipe it straight into `sudo bash` — the first run asks a few
questions (generate or restore an identity, listen port, whether to act as
a bootstrap relay, which peers to dial on startup) over your terminal, and
a pipe replaces stdin. Save the script first, as shown above.

The installer creates a `nepheral` system user, installs the binary to
`/usr/local/bin/nepheral-node`, runs it once so you can answer the wizard
(answers persist to `~/.nepheral/config.json` — future restarts never ask
again), then installs and starts a systemd service (`nepheral-node`).
Manage it with the usual tools:

```bash
sudo systemctl status nepheral-node
journalctl -u nepheral-node -f
```

The builds it installs run a model (`node_only_local`, `node_only_vulkan`
or `node_only_cuda`). A node built with plain `node_only` never loads one
and only carries mesh and chain traffic.

`nepheral-node health` prints the node's chain progress, finality and peers
as JSON, with nothing private in it, for any monitoring you want to run on
top of it.

