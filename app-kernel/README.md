# app-kernel

Node runtime: wires together the QUIC mesh, peer discovery and gossip, the
chain and block producer, private mesh queries (agent sessions), encrypted
rooms, model distribution, and the local API daemon. Built both as the
headless `nepheral-node` binary and as a library for `nepheral-desktop`.

## Features / binary

| Feature | Pulls | Notes |
|---|---|---|
| `node_runtime` | none | The mesh runtime (QUIC accept loop, wire-protocol dispatch, gossip, discovery). Implied by `node_only` and `client`; no HTTP stack |
| `node_only` | `node_runtime` | Headless node, no inference |
| `node_only_local` | `node_only` + `ai-engine/local-inference` | Headless node + CPU inference |
| `node_only_vulkan` / `node_only_cuda` | `node_only` + the matching `ai-engine` backend | Headless node + GPU inference |
| `node_only_gpu_auto` | `node_only` + `ai-engine/local-inference-auto` | Headless node, runtime GPU pick |
| `client` | `node_runtime` + `local-api-server` | Everything the desktop client needs, no local inference |
| `client-cpu` / `client-vulkan` / `client-cuda` | `client` + the matching `ai-engine` backend | Built by `nepheral-desktop`'s `cpu` / `vulkan` / `cuda` features |
| `client-gpu-auto` | `client` + `ai-engine/local-inference-auto` | Built by `nepheral-desktop`'s `gpu-auto` feature |
| `local-api-server` | axum, tower-http, `nepheral-api` | The OpenAI/Ollama-compatible HTTP daemon |

```sh
cargo build --release -p app-kernel --features node_only_local --bin nepheral-node
```

The only binary is `nepheral-node` (`src/main.rs`, requires `node_only`).
The desktop client starts the same runtime through `client_node::start()`;
see the root [BUILD.md](../BUILD.md).

## Module layout

| Module | Contents |
|---|---|
| `boot.rs` | Shared startup: `select_network()`, `tls_identity()`, `bind_mesh(port)` (falls back to port 4433 when the requested port cannot be bound) |
| `nepheral_config.rs` | `~/.nepheral/config.json` (`NepheralConfig`), `DEFAULT_MESH_PORT` (443), `FALLBACK_MESH_PORT` (4433), `configured_network()`, `network_dir()`, `load_or_create_identity_mnemonic()` |
| `wallet.rs` | The wallet apart from the node: `WalletKey` in memory only while unlocked, `unlock_wallet`/`lock_wallet`, auto-lock, `sign_and_submit_as_wallet`, the `Confirm` hook every payment, trade and vote waits for, the delegation to the wallet (pallet 4), the API token's hash |
| `control.rs`, `cli_wallet.rs` | (bin) the headless node's control socket and its `unlock`, `lock`, `status`, `wallet init/restore/password/send` commands |
| `vault.rs` | The desktop's password vault `~/.nepheral/vault.bin`: `Secrets` (mnemonic, API token, system prompt) sealed with Argon2id + XChaCha20-Poly1305; `migrate_legacy()` moves a plain `identity.mnemonic` in and wipes it, `restore()` replaces it from the 24 words |
| `state.rs` | `AppState` (shared state), connection registry, `HELLO` handling, peer discovery/gossip helpers, `send_frame` |
| `node_runtime.rs` | QUIC accept loop, `handle_stream_with_state()` dispatch, periodic mesh tasks |
| `agent.rs` | Private mesh queries: agent sessions, `ask_mesh`, share push/claim/release, responder inference, service receipts, relaying |
| `rooms.rs` | Joining, sending to and sweeping encrypted rooms; the room assistant |
| `chain_node.rs` | Extrinsic submission and mempool, slot-based block producer for any key in the on-chain authority set, block sync with fork resolution, chain persistence, authority bootstrap announcements |
| `finality.rs` | Finality voter: votes every 16 blocks with each validator key the node holds, collects votes, submits justifications and equivocation reports, remembers its last vote on disk |
| `governance.rs` | Proposing, voting and enacting governance proposals; founder 0 resignation |
| `ratings.rs` | Rating mesh answers in chat sessions and rooms |
| `models.rs` | Local model resolution, model sync (peer transfer, then verified download), AI engine start-up, system prompt |
| `client_node.rs` | `start()` / `ClientNode` for the desktop client (feature `client`) |
| `api_backend.rs` | `AppStateBackend` for `nepheral-api`, `ensure_api_token()` (feature `local-api-server`) |
| `cli_setup.rs` | `nepheral-node` first-run wizard and environment resolution (binary only) |

## Startup

1. `boot::select_network()` fixes the process-wide network (testnet or
   mainnet); chain ids, ALPN, signing digests and room keys all depend on it.
2. The node key is loaded from `~/.nepheral/identity.mnemonic` (created on
   first run) or taken from `NEPHERAL_NODE_MNEMONIC`; the desktop's comes
   from its OS-sealed node store. The wallet stays locked until
   `nepheral-node unlock` (or the app's unlock) opens the vault.
3. `boot::bind_mesh()` binds IPv4 and IPv6 listeners on the mesh port
   (default 443), falling back to 4433.
4. Chain state is loaded from `~/.nepheral/<network>/chain.bin`, and an
   authority node announces its configured sale wallet, sale oracle and
   Top-Tier nodes.
5. The background tasks below are spawned.

`nepheral-node` in bootstrap mode (`NEPHERAL_IS_BOOTSTRAP` or the wizard's
choice) only relays peer lists, gossip and room envelopes: no chain, no
inference, no local API.

`nepheral-node` resolves its settings in this order: `NEPHERAL_NODE_MNEMONIC`
set means everything comes from the environment; otherwise a saved config
from the wizard; otherwise, on a TTY, a one-time interactive wizard.

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `NEPHERAL_NETWORK` | config `network`, else `testnet` | `testnet` / `mainnet` |
| `NEPHERAL_PORT` | 443 (config `mesh_port` for the desktop client) | Mesh listen port (QUIC/UDP). For `nepheral-node` it is read only in environment mode or before setup has been saved; afterwards the saved `node_port` wins |
| `NEPHERAL_NODE_MNEMONIC` | none | BIP-39 identity phrase, overrides `identity.mnemonic`. For `nepheral-node` it also selects environment mode (no wizard) |
| `NEPHERAL_BOOTSTRAP_PEER` | network default bootstraps (desktop) / none (`nepheral-node`) | Comma-separated `ip:port` peers to dial at startup. `nepheral-node` reads it in environment mode; otherwise it uses the saved `node_bootstrap_peers` |
| `NEPHERAL_BOOTSTRAP` | network default bootstraps | Comma-separated introducers queried for peer lists |
| `NEPHERAL_IS_BOOTSTRAP` | false | `1` or `true`: run `nepheral-node` as a bootstrap/relay node (environment mode) |
| `NEPHERAL_MODEL_PATH` | none | Local GGUF to load; checked before the configured active model |
| `NEPHERAL_INFERENCE_URL` / `NEPHERAL_AI_URL` | none | `nepheral-node`: external `/api/chat`-compatible backend, used when `NEPHERAL_MODEL_PATH` is unset |
| `NEPHERAL_AI_MODEL` | `local` (with a model path) | `nepheral-node`: model name for the local or external backend |
| `NEPHERAL_SYSTEM_PROMPT` | none | System prompt used when config `local_system_prompt` is unset |
| `NEPHERAL_AUTHORITY_KEY` | none | Hex 32-byte ed25519 seed of founder 0. Enables the founder announcements and lets the node produce founder 0's slots. Founding nodes need no extra key: they produce their slots with their own mesh identity once they are in the on-chain authority set |
| `NEPHERAL_CHAIN_BIN_PATH` | `~/.nepheral/<network>/chain.bin` | Chain file location |
| `NEPHERAL_TOP_TIER_NODES` | none | Authority only: `<hex peer id>=<channel>[,<channel>...]` entries separated by `;`, registered with `AuthorityCall::Add` / `SetChannels`. Channels: `chat`, `large_chat`, `frontier` |
| `NEPHERAL_SALE_ORACLE_PUBKEY` | none | Authority only: hex ed25519 key announced with `SetSaleOracle` |
| `NEPHERAL_LTC_DIRECT_SALE_ADDRESS` | none | Authority only: address announced with `SetDirectSaleWallet` |
| `NEPHERAL_DISSOLVE_AUTHORITY` | none | Founder only: set to `dissolve-founder-authority` to submit the irreversible `Authority::Dissolve`; refused until governance has replaced the sale oracle by vote |
| `NEPHERAL_VRAM_MB` | probed | Overrides detected VRAM (read by `ai-engine`) |

`HOME` (or `USERPROFILE` on Windows) locates `~/.nepheral`. Build-time
variables `NEPHERAL_TEAM_PUBKEY` and `NEPHERAL_DEFAULT_NETWORK` are read by
`core-crypto`.

Relevant `config.json` keys: `network`, `mesh_port`, `node_port`,
`node_is_bootstrap`, `node_bootstrap_peers`, `active_model`,
`local_system_prompt`, `redact_terms` (extra terms the sender-side
`Redactor` removes), and `local_api_server_{enabled,host,port,token,allowed_origins}`
(default `127.0.0.1:11434`, a token generated on first start, no CORS
origins).

## Background tasks

Each runs as its own `tokio` task and stops on `AppState::begin_shutdown()`.

| Task | Defined in | Purpose |
|---|---|---|
| `run_node_with_state` | `node_runtime.rs` | Accepts connections, registers and greets them, dispatches streams |
| `run_peer_connection_manager` | `node_runtime.rs` | Dials bootstrap peers |
| `run_peer_discovery` | `node_runtime.rs` | Every 30 s: queries introducers, asks connected peers for peer lists, dials discovered peers |
| `run_gossip_flood` | `node_runtime.rs` | Gossips known peers every 30 s |
| `run_capability_heartbeat` | `node_runtime.rs` | Re-announces capabilities every 30 s |
| `run_stale_peer_cleanup` | `node_runtime.rs` | Prunes dead peers every 60 s |
| `run_stats_broadcast` | `node_runtime.rs` | Sends `NODE_STATS` every 60 s |
| `run_ai_session_decay` | `node_runtime.rs` | Decays AI sessions every 5 s |
| `run_mdns_announce_and_discover` | `node_runtime.rs` | LAN discovery via mDNS (desktop client) |
| `run_ai_startup` | `models.rs` | Starts the inference engine once a model or backend is available |
| `run_model_sync` | `models.rs` | Without a local model: waits up to 30 s for a peer announcing the verified model and fetches it, otherwise downloads it with hash verification (desktop client) |
| `run_chain_status_log` | `chain_node.rs` | Logs chain height every 60 s |
| `run_block_producer` | `chain_node.rs` | Seals blocks in this authority's slots |
| `run_chain_persist_task` | `chain_node.rs` | Writes the chain file on change (and at least every 30 s) |
| `run_agent_sweeper` | `agent.rs` | Expires custody shares, claims, relay routes and stale queries every 30 s |
| `run_room_sweeper` | `rooms.rs` | Expires members, sends presence, closes rooms with no compute contributor |

The local API server is started alongside when `local_api_server_enabled`
is true (not in bootstrap mode).

## Private mesh queries (`agent.rs`)

1. The sender redacts the prompt, context window and system prompt with
   its per-session `Redactor` (`ai_engine::redaction`).
2. Eligible AI-capable peers are ranked with
   `core_crypto::lottery::rank_candidates_weighted` (weights from on-chain answer quality; nodes rated below 25% with enough votes are left out); the query is encrypted as an
   `EncryptedBundle` for the top ranks only.
3. The bundle is Shamir-split with `ShamirConfig::dynamic_for(custodians)`
   and each share is pushed to a connected custodian as a signed `SharePush`.
4. Ranked candidates broadcast a VRF-backed `AgentClaim`; custodians verify
   it (`vrf_verify`) and release their shares to the claimant.
5. The winner reconstructs, decrypts, runs `InferenceEngine::infer_stateless`,
   and returns an `AgentResponseDirect` sealed to the sender with a signed
   `ServiceClaim`, relayed through `RELAY_OPEN` when there is no direct path.
6. The sender verifies the response, restores the placeholders, countersigns
   the claim and submits `ContributionsCall::SubmitReceipt`.

## Rooms (`rooms.rs`)

Rooms use `mesh_network::rooms`: members share a `RoomCode`, traffic is a
signed, encrypted `RoomEnvelope` flooded as `ROOM_ENVELOPE`. Mentioning
`@nai` (or the older `@nepheral` / `@agente`) asks the room assistant: the room's chat window
is redacted with the room's `Redactor` and dispatched as a private mesh
query, answered locally with `infer_stateless` if the mesh cannot. A room
closes when no member has contributed compute for 20 s.

## Wire dispatch

`handle_stream_with_state()` reads the type byte and a length-bounded body
for each message type in `mesh_network::protocol::message_types`
(`HELLO`, `PEER_LIST_REQUEST`, `PEER_LIST`, `GOSSIP_PEER`, `NODE_STATS`,
`ROOM_ENVELOPE`, `CHAIN_TX`, `BLOCK_BROADCAST`, `BLOCK_SYNC_REQUEST`,
`MODEL_ANNOUNCE`, `MODEL_REQUEST`, `MODEL_TRANSFER`, `SHARE_PUSH`,
`SHARE_CLAIM`, `AGENT_RESPONSE_DIRECT`, `RELAY_OPEN`) and rejects any other
type. The peer id is always the one authenticated by mutual TLS. See
[mesh-network](../mesh-network/README.md) for formats and size caps.

## Tests

`tests/mesh_e2e.rs` (feature `node_runtime`) runs local nodes with an echo
inference backend and checks private queries and encrypted rooms end to
end, including closing a room nobody contributes compute to.
