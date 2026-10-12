# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Every entry is written when the change is merged, under `Unreleased`, and
moves to a version heading when that version is released.

## [Unreleased]

### Added

- RFC-0001, knowledge maps: packages of passages from openly licensed books
  and documentation that a node searches before it answers. On a 28-question
  test, a 4B model goes from 26 to 40 of 56 with the first map.
- ADR-0019: maps are searched on the node that answers, installed from a
  Maps panel, pinned by hash in each release, and ignored by the lottery.
- ADR-0020: the licences accepted for map content, which add CC BY-SA, the
  GFDL and the GPL for maps only. ADR-0018 is unchanged for everything else.
- ADR-0021: inference contexts are reused and zeroed after every reply; only
  the owner's local chat and local API keep a prefix cache between turns.

### Changed

- CONTRIBUTING.md states the licence rule for map content, and no longer
  says the terms for outside contributions are pending: the Contributor
  License Agreement has applied since 0.5.4.

## [0.5.4] - 2026-10-11

First public release of the source code. Everything below is what this
version does. The licence covers this version and every earlier one
from 0.1.0, including versions released only as binaries.

### Network and transport

- Peer-to-peer mesh over QUIC with TLS 1.3 and mutual authentication. Each
  node's certificate carries its Ed25519 identity, and every handler acts
  on the identity proven by TLS, never on a self-declared field.
- Wire protocol encoded with postcard, with a size cap per message type.
  Protocol version 5, with a transition window so that nodes one version
  apart keep talking, and capability bits announced in `HELLO`.
- Named networks. Testnet is the default and mainnet is a configuration
  change. The network fixes the chain id, the ALPN, the bootstrap nodes,
  the mDNS service and the data directory, so the two never mix.
- Default mesh port 443/UDP with a fallback to 4433, and local discovery
  over mDNS.
- Presence: every node passes on the newest signed `HELLO` of every node
  it has heard of, so nodes behind NAT learn about each other.
- NAT traversal:
  - hole punching introduced by public nodes;
  - router port mapping through UPnP-IGD, NAT-PMP or PCP;
  - blind relays as the fallback. Any reachable node can relay, and frames
    are sealed to their destination.
- Golden tests that freeze the wire bytes of the core messages.

### Private queries

- Personal data is redacted on the sender with stable placeholders and
  restored locally in the answer. Extra terms can be added with
  `redact_terms`.
- One lottery per message, weighted by answer ratings, with no
  conversation affinity. The session key is wrapped only for the three
  highest-ranked candidates, through X25519 derived from their Ed25519
  identities.
- The query bundle is encrypted with XChaCha20-Poly1305 and split into
  Shamir shares, with a threshold of max(2, ceil(n/2)) over GF(2^8), one
  share per custodian. Every node can hold shares.
- Shares are released by rank (immediately, after 8 s, after 16 s) so that
  a single winner answers. A claimant must be the TLS-authenticated peer
  named at that rank.
- Blind memory: the responder returns an updated summary sealed to the
  sender, and no node keeps conversation state.
- Answers are sealed to the asker, so a relay carrying one sees nothing of
  it.
- Signed service claims, countersigned by the sender, become on-chain
  receipts.
- Identities with no on-chain history have limited access to custody slots
  and lottery ranks until they are established.
- A placeholder that a model invents in its answer, and that the device
  never issued, is shown as a plain slot such as `[name]` rather than as
  the internal token.
- A stray Chinese, Japanese or Korean word that slips into an answer in
  another script is removed, unless the question is about those languages.

### Rooms

- End-to-end encrypted group chats identified by a 16-character code.
  Envelopes are sealed and signed by the sender, flooded with a TTL and
  deduplicated, and relayed by nodes that cannot read them.
- A shared agent answers `@nai` mentions through the private lottery, using
  the room's recent context.

### Chain and consensus

- A compact chain built from pallets: System, Balances, Contributions, Nai,
  Identity, Authority, Economy, Market, Governance, Reputation, Finality and
  Upgrade.
- Two-second slots, with early blocks under load. Block producers are read
  from chain state, and an offline producer only leaves its slots empty.
- Fork choice is the longest valid chain, with reorganisations capped at 64
  blocks.
- Every block is verified on receipt: the roots are recomputed from the
  body, the state root is checked after applying it, and any failure
  restores the previous state in full.
- Finality: validators sign every 16 blocks, and two thirds plus one form a
  justification recorded on chain. A proven equivocation removes the
  validator that committed it.
- Rule changes are scheduled on chain with a minimum notice through the
  Upgrade pallet. A node that does not implement a scheduled version stops
  following the chain and says why, instead of forking.
- Light sync: a verified snapshot that follows every validator-set change
  from genesis, served by full nodes.
- Storage: every node keeps and serves the full chain. The chain file is
  append-only and fsynced, quarantined if unreadable, and synced in
  batches.

### Economy

- N-AI credits enter circulation through contribution receipts and the
  `Redeem` formula, with a shares-per-credit ratio that adjusts to network
  activity.
- Holding shares for other nodes earns credit, proven by custody
  attestations.
- Asking the network is free. Credits are spent only on Top-Tier sessions,
  and a session inference is charged only when the first token of the
  answer arrives.
- Direct sale against Litecoin: an `OP_RETURN` payment memo names the
  receiving wallet, and the sale oracle, a separate service authorised on
  chain, credits it.
- A market for earned credits:
  - asks are held in chain escrow;
  - bids are advertisements only;
  - settlement is one-directional and driven by claims. The chain never
    holds Litecoin.
  - trading requires standing (recorded contribution and tenure).

### Identity, wallet and keys

- The node key is separate from the wallet, and the Identity pallet links
  them. What a node earns lands in its wallet.
- The node key opens unattended so the node can serve. On Windows it is
  sealed with DPAPI and, where present, with a key held in the TPM; it can
  be restored from the vault if the TPM is cleared. On other systems it is
  owner-only.
- The wallet's 24-word recovery phrase and the local API token live in a
  password vault (Argon2id, then XChaCha20-Poly1305). They stay in memory
  only while the wallet is unlocked, and the wallet locks itself after a
  period of inactivity.
- The recovery phrase can be exported as a password-protected PDF.
- Each node declares the hardware that seals its key (TPM chip, firmware
  TPM or Pluton), and node lists show it.
- Transfers, sessions and market actions ask for confirmation before they
  are signed.

### Governance and reputation

- Founder 0 admits and removes the founding nodes. Founder 0's
  resignation ends every founder title and hands control to governance.
- Proposals and votes weighted by recorded contribution, with fixed voting
  and enactment delays.
- Answer ratings: a like, or a dislike with a reason. Ratings are tied to
  the service receipt, weighted by the rater's contribution and fade over
  about a week. Poorly rated nodes are asked less.

### Inference

- Local inference through llama.cpp, with CPU, Vulkan, CUDA and
  automatically selected (gpu-auto) builds.
- Device choice skips integrated GPUs that are slower than the CPU. With
  several GPUs, a node either splits one model across them or runs a copy
  on each, answering several questions at once.
- External OpenAI-compatible HTTP backends.
- A model catalog from Hugging Face with SHA-256 verification. A node
  accepts model transfers only for a model its user requested, whose hash
  is published in the catalog.
- When a GGUF file fails to load, the node explains why.
- A node advertises the name its model carries and the model's real size,
  counted from the weights in the GGUF file, instead of trusting the file
  name: a file called `model.gguf` no longer passes for a 1B model.
- Reasoning models: reasoning is off unless requested, and the reply budget
  is kept for the answer.
- When asked, the responder says which model answered, through Nepheral.

### Local API

- A daemon on `127.0.0.1:11434`, compatible with the OpenAI and Ollama APIs.
- Protected by a bearer token that is checked in constant time, with
  DNS-rebinding and origin checks.
- Routing tiers: standard, Top-Tier sessions, and the modes `review`,
  `doc`, `research` and `dev`, with a routing-policy endpoint.
- Message content as a string or as a list of text parts. Images are
  refused for now, with an OpenAI-shaped error, as is malformed JSON.
- `temperature` and `max_tokens` (Ollama: `options.temperature`,
  `options.num_predict`) apply to models running on the machine. `stop`
  applies to every route, streaming included.

### Conversation memory

- Conversations are kept on the device. A small multilingual embedding
  model (Granite Embedding 107M Multilingual, packed with int8 word
  embeddings) decides which earlier messages a question carries.
- Before an earlier message that looks sensitive is sent, the person is
  asked to confirm. The confirmation can be paused for a chosen time.
- Only the person's own messages are checked. Each one is compared with
  examples of everyday text as well (a pet's health, code, fiction,
  general questions), so they no longer trigger the confirmation.
- A summary of the conversation favours the person's own questions, which
  name its topics, over long answers.
- A lexical measure takes over while the embedding model is not on the
  device.

### Desktop client and headless node

- A Tauri 2 desktop client for Windows and Linux, in English and Spanish:
  chat, rooms, wallet, market, governance, node lists and settings.
- Signed automatic updates, checked per build variant (CPU, Vulkan, CUDA).
  On Windows, an update waits for the app to close before installing.
  This release changes the update signing key: a desktop running a
  private version installs 0.5.4 by hand once, and updates itself after.
- The peers tab lists a dropped connection for a minute only, and never
  shows the address of a node this one was not connected to: those are
  only counted.
- Your own row in the node list shows your hardware seal, and peers learn
  of a new seal at once.
- The app says where credits are explained that asking the network is
  free.
- `nepheral-node`, a headless node with:
  - an interactive first-run setup;
  - wallet commands over an owner-only control socket;
  - a `health` report in JSON for monitoring.

### Security

- No dependency carries a known RustSec vulnerability at release. Among the
  versions this release ships:
  - quinn-proto 0.11.19, which fixes remote memory exhaustion through
    out-of-order stream reassembly;
  - rustls 0.23.45, which fixes TLS 1.3 handshake messages accepted across
    encryption levels;
  - rustls-webpki 0.103.15, which fixes certificate name-constraint checks
    and a panic while parsing revocation lists.
- HTTP calls to the model catalog and to external backends use the same
  TLS stack as the updater (reqwest 0.13, rustls with ring).
- Three unmaintained crates remain; all are compile-time macros with no
  maintained replacement. Each is documented in `deny.toml` with its reason
  and a review date.

### Documentation

- A whitepaper and design documents for the economy, governance, answer
  ratings, conversation memory and the blind tier.
- A build guide, a security policy and third-party notices.
- Every installer and package carries `legal/`, with the full licence text
  of every dependency (`THIRD-PARTY-LICENSES.html`, generated with
  `cargo about`).
- Architecture decision records in `docs/adr` and the RFC process in
  `docs/rfcs`. Changes follow a document-first path: RFC, ADR, then code.
- A contribution guide, engineering standards (`docs/standards`) and a
  contributor licence agreement (`CLA.md`).
- Continuous integration for formatting, lints, tests and dependency
  checks, and a dependency licence policy that keeps every dependency
  compatible with the licence model (ADR-0018).

## Private versions (0.1.0 to 0.5.3)

Before 0.5.4 the source was never published; some of these versions were
released as binaries for testing. Everything they contained is part of
0.5.4 above, and the licence covers every version from 0.1.0 (see
`LICENSE`).

[Unreleased]: https://github.com/JEHR037/Nepheral-core/compare/v0.5.4...HEAD
[0.5.4]: https://github.com/JEHR037/Nepheral-core/releases/tag/v0.5.4
