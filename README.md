# Nepheral

A Rust mesh where every install is a full node: local llama.cpp inference, a
mutually authenticated QUIC transport, a mini-Substrate-style chain, and a
privacy layer. Each message is redacted on the sender, encrypted, split into
Shamir shares across independent peers, and answered by the winner of a
fresh per-message lottery. End-to-end encrypted rooms share an agent that
answers `@nai` mentions through the same private path.

## How a query flows

```
┌──────────────┐  redact + seal + Shamir split    ┌─────────────┐
│    Sender    │ ───── N shares, N peers ─────▶ │ Custodians  │
└──────┬───────┘                                └──────┬──────┘
       │                                               │ SHARE_CLAIM
       │                                               ▼ (TLS peer id + rank)
       │                                        ┌─────────────┐
       │      AGENT_RESPONSE_DIRECT (AEAD)      │   Lottery   │
       └◀────────────────────────────────────── │   winner    │
                                                │ reconstruct │
                                                │  + inference│
                                                └─────────────┘
```

| Layer | Crate | Responsibility |
|---|---|---|
| Cryptography + chain | [core-crypto](core-crypto/README.md) | Shamir SSS over GF(2^8), hybrid ECIES, domain-separated signatures, pallets, ranked lottery, service receipts |
| Transport | [mesh-network](mesh-network/README.md) | Mutual-TLS QUIC, postcard wire protocol, encrypted rooms |
| Inference | [ai-engine](ai-engine/README.md) | Native llama-cpp + external HTTP backends, sender-side redaction, hardware tiering |
| API daemon | nepheral-api | OpenAI/Ollama-compatible HTTP surface + 4 topological modes |
| Orchestration | [app-kernel](app-kernel/README.md) | Client-node runtime, wallet, block producer — both binaries |
| Private memory | recall | Conversation memory decided on the device: which earlier messages a question carries, with a small multilingual embedding model |
| Desktop shell | nepheral-desktop | Tauri 2 webview client over the shared node runtime |

The Android app is distributed separately and is not part of this repository.
The protocol it speaks (light sync, custody and the wire format) is.

## Build

Two binaries, one workspace. See [BUILD.md](BUILD.md) for the full feature
matrix, per-platform toolchains, and troubleshooting.

```sh
# Desktop client with CPU inference (needs cmake + clang/LLVM)
cargo build --release -p nepheral-desktop

# Desktop client without local inference (no toolchain needed; dispatches
# every query to other mesh peers instead of running a model locally)
cargo build --release -p nepheral-desktop --no-default-features

# Headless authority / responder node
cargo build --release -p app-kernel --features node_only_local --bin nepheral-node
```

## Networks

The same binary runs on testnet or mainnet. Testnet is the default.
Switching is a configuration change and a restart:

```sh
# one-off
NEPHERAL_NETWORK=mainnet nepheral-node
# persistent: set "network": "mainnet" in ~/.nepheral/config.json
```

The network selects the chain id (and therefore every signature domain),
the ALPN, the default bootstraps, the mDNS service and the data directory
`~/.nepheral/<network>/`. Nodes on different networks cannot connect or
accept each other's blocks. Builds can change the default with
`NEPHERAL_DEFAULT_NETWORK=mainnet` at compile time.

## Local API

`nepheral-desktop` serves an OpenAI- and Ollama-compatible daemon on
`127.0.0.1:11434`, protected by a bearer token generated on first run and
kept in the wallet's vault (shown in the app's API tab once unlocked); the
API keeps answering while the wallet is locked. Model names select
the routing tier: `nepheral-standard`, `nepheral-top-tier-*` (requires an
on-chain session), and the topological modes `nepheral-review-*`,
`nepheral-doc-*`, `nepheral-research-*`, `nepheral-dev-*`. Quick-start
examples in [BUILD.md](BUILD.md#api-quick-start-once-running).

## What Nepheral does NOT protect against

Stated up front, because unconditional privacy claims are how projects lose
credibility: a network observer can correlate who sends and who responds
(no traffic-analysis defense); the lottery winner reads the redacted query it
wins; redaction is pattern-based; recipient-side wallet compromise decrypts
past captured bundles addressed to that wallet; a node's own key opens
without a password so it can serve unattended (DPAPI-sealed on Windows,
owner-only elsewhere) and holds no funds, while the wallet's 24 words stay in
a password vault (`~/.nepheral/vault.bin`, Argon2id + XChaCha20-Poly1305) and
in memory only while unlocked. The full threat model
is in [docs/WHITEPAPER.md](docs/WHITEPAPER.md).

## Contributing

Changes follow a document-first path: a proposal as an
[RFC](docs/rfcs/README.md) when it needs discussion, the decision as an
[ADR](docs/adr/README.md), and only then the code. No change may add a
dependency whose licence is incompatible with the project's licence model
([ADR-0018](docs/adr/0018-dependency-licences.md)).

Start with [CONTRIBUTING.md](CONTRIBUTING.md). Contributions are accepted
under the [Contributor License Agreement](CLA.md). The engineering
standards are in [docs/standards](docs/standards/README.md); why the system
is built the way it is, in the
[architecture decision records](docs/adr/README.md); proposals under
discussion, in [docs/rfcs](docs/rfcs/README.md). What changed in each
version is in [CHANGELOG.md](CHANGELOG.md).

```sh
just check    # format check, clippy with warnings as errors, tests
```

## License

Business Source License 1.1 (BUSL-1.1). Each version converts to the Apache
License 2.0 four years after that version is first made publicly available.
Operating nodes on the Nepheral network — including for profit and at
commercial scale — is expressly permitted; offering Nepheral as a managed
service to third parties, or building a competing network on it, requires a
commercial license. Full terms in [LICENSE](LICENSE); vulnerability
disclosure in [SECURITY.md](SECURITY.md); third-party licences in
[legal/THIRD-PARTY-NOTICES.md](legal/THIRD-PARTY-NOTICES.md). How control moves from the
founder to a vote is in [docs/GOVERNANCE.md](docs/GOVERNANCE.md); how answers
are rated in [docs/REPUTATION.md](docs/REPUTATION.md); how credits are earned
and spent in [docs/ECONOMY.md](docs/ECONOMY.md).
