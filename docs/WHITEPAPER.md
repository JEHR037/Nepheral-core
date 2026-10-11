# Nepheral — Content-Private Distributed AI with a Compute-Anchored Credit Economy

**Design snapshot, release 0.5.0.**

This document describes Nepheral as implemented in the 0.5.0 reference
code. Constants and interfaces that are still expected to move are tagged
as such. The reference implementation is linked throughout.

---

## 0. Scope of the privacy claim

Nepheral guarantees **content privacy against intermediate parties**:

- Before anything leaves the machine, the sender replaces personal data
  (emails, phones, IBANs, card numbers, IPs, UUIDs, secrets, user paths,
  URLs with credentials and user-configured names) with stable
  per-conversation placeholders, and restores them locally in the answer.
- The redacted prompt is XChaCha20-Poly1305 encrypted under a session key.
- The session key is X25519-wrapped only for the top three candidates of
  a per-message lottery.
- The whole bundle is Shamir-split into k-of-n shares before transit.
- No custodian, relay, or authority can reconstruct the plaintext
  below the Shamir threshold.

What Nepheral does **not** guarantee:

- **The responder reads the redacted plaintext.** The peer elected by the
  lottery decrypts to run inference. It sees the redacted prompt, a short
  window of recent messages and a blind memory summary, never the real
  values behind the placeholders. Because the lottery runs for every
  message, no single responder sees a whole conversation. This is the same trust model as Tor exit
  nodes — privacy is by structural routing, not homomorphic compute.
  TEE-mediated inference (Blind AI) is the planned long-term close.
- **Network metadata leaks.** QUIC TLS hides content. It does not hide
  who connected to whom (IP-layer observation), packet timing, or
  payload size. For workflows where the threat model includes a passive
  network adversary, run Nepheral behind Tor or a mixnet (Loopix, Nym).
- **Finality needs validators.** Blocks are final once two thirds plus
  one of the founding nodes sign them. With fewer than 4 founding nodes
  finality tolerates no faults; it lags, but production continues.

## 1. Motivation

The current hosted AI stack has three coupled failures:

1. **Privacy is policy, not architecture.** Operators see every
   prompt and response. Trust is contractual.
2. **Price discovery is opaque.** Inference is priced for
   hyperscaler-scale infrastructure regardless of the buyer's
   workload.
3. **Participation is closed.** Compute providers are a
   pre-selected enterprise tier; end users have no contribution
   path.

Nepheral targets the intersection of those three. The thesis: an
inference protocol whose privacy is structural, whose price floats
toward the marginal cost of compute, and whose contribution path
is open from day one, will produce a different network than any
hosted equivalent.

---

## 2. System overview

Nepheral is a peer-to-peer network where every install is
simultaneously a client, a relay, and (when AI-capable) an
inference responder. The network has three layers:

- **Transport** — QUIC with TLS 1.3 and mutual authentication. Each
  node's certificate carries its Ed25519 identity, so every stream is
  bound to a verified peer id. The ALPN is `nepheral/3/<network>`, which
  keeps testnet and mainnet nodes from ever talking to each other. Default
  port 443/UDP with a fallback to 4433. Messages are postcard-encoded
  frames with per-type size caps.
- **Privacy** — every chat message is AEAD-encrypted under a
  session key (XChaCha20-Poly1305), the session key is wrapped
  per recipient via X25519 derived from the ed25519 identity, and
  the resulting EncryptedBundle is split into Shamir shares
  (threshold max(2, ⌈n/2⌉) over GF(2⁸)) before transit. No single peer
  ever sees the full ciphertext or the wrapped key.
- **Settlement** — a mini-Substrate-style chain produces signed
  blocks every 2 seconds (early-block trigger above 8 pending
  extrinsics) and records contribution, consumption, lottery
  outcomes, and treasury balances. Its pallets — System,
  Balances, Contributions, Nai, Identity, Authority, Economy,
  Market, Governance, Reputation, Finality — total ~3,500 LOC.

The product surface is a desktop client (Tauri 2 webview over the shared
node runtime), a headless node for self-hosted servers with native
llama.cpp inference, and a browser thin client for the local API daemon.

---

## 3. The private query protocol

Every chat message runs its own lottery. There is no conversation
affinity: anchoring a conversation to one responder would hand that
responder the whole conversation, which is exactly what the design is
meant to prevent. Continuity comes from a blind memory instead.

When the user submits a message, the client:

1. **Redacts.** The per-conversation `Redactor` replaces personal data
   with placeholders such as `[[EMAIL_1]]`. The same value always maps to
   the same placeholder within a conversation, so the model can reason
   about it without seeing it.
2. **Builds the query.** Redacted prompt, the last four redacted messages
   and the blind memory (a short summary the previous responder returned,
   itself redacted).
3. **Draws the lottery.** A fresh random room salt per message feeds
   `rank_candidates_weighted`, which ranks the AI-capable candidates
   with lottery weights taken from their answer ratings. The session
   key is wrapped only for ranks 0, 1 and 2.
4. **Encrypts and splits.** The bundle (version 2, postcard) is AEAD
   encrypted and Shamir-split, one share per custodian.
5. **Pushes.** Each custodian receives a signed SHARE_PUSH carrying its
   share, the candidate list, the lottery inputs, the bundle digest and,
   for Top-Tier, the session token.

Custodians release shares by rank: rank 0 immediately, rank 1 after 8 s,
rank 2 after 16 s, at most one release per rank, and lower ranks stop
being served 60 s after a higher rank claimed. The claimant must be the
TLS-authenticated peer named at that rank, and its VRF seed must match
the message. The responder combines shares until the reconstructed
bundle matches the signed digest, decrypts, runs a stateless inference,
and returns the answer plus an updated blind memory sealed to the
sender, together with a signed service claim.

The sender accepts the answer only if the responder was one of the
wrapped recipients and the digest matches. It restores the placeholders
locally, countersigns the service claim, and submits the resulting
receipt on chain (`Contributions::SubmitReceipt`). Receipts are how
contribution is proven: the responder cannot mint work the sender did
not acknowledge.

### 3.1 Rooms

Rooms are end-to-end encrypted group chats with an optional shared agent.

- A room is identified by a 16-character code (about 80 bits). The room
  id and the room key are both derived from the code and the chain id, so
  knowing the room id does not reveal the key.
- Every envelope is sealed with XChaCha20-Poly1305 (the room id, sender,
  message id and timestamp are bound as associated data) and signed by
  the sender. Envelopes are flooded with a TTL of 6 and deduplicated.
  Nodes without the code relay envelopes but cannot read them.
- Members send presence every 20 s and are dropped after 70 s of silence.
- A room lives only while at least one member, including the local
  node, contributes compute. Without compute it closes after a 20 s
  grace.
- A message that mentions `@nai` (older clients type `@nepheral` or
  `@agente`, still recognised) goes through the
  same private lottery as a direct query, using the room's recent
  context; the answer is posted back into the room. Messages without a
  mention spend no inference.

### 3.2 Adversary model

- **A passive network observer** sees TLS-protected QUIC traffic. It can
  still correlate who talks to whom and when; there is no
  traffic-analysis defense.
- **A custodian** holds one share of one message. Below the threshold it
  cannot rebuild the ciphertext or the wrapped keys.
- **A responder** sees one redacted message, a short redacted window and
  the blind memory. It never learns the values behind the placeholders,
  and the next message goes to a different lottery.
- **A peer impersonating another** is stopped by mutual TLS: handlers
  act on the certificate identity, not on self-declared fields.
- **Replay across networks or message types** is stopped by
  domain-separated signatures that include the chain id.
- **Cross-account griefing** via Redeem, Spend or market calls is
  blocked by `signer == account` checks.

Outside the threat model:

- A compromised authority key. Until multi-validator finality, a leaked
  authority key can mint credits; per-transaction caps limit the damage
  but do not prevent it.
- Redaction is pattern-based. Personal data written in free form (for
  example a street address in prose) is only redacted if the user adds
  it to `redact_terms`.

---

## 4. The chain

ChainState owns one Storage struct per pallet plus the block
history. Per-pallet roots concatenate into `state_root` so a
receiver re-running every block produces the same commitment.

### 4.1 Pallets

| Idx | Pallet | Calls | Storage size class |
|---|---|---|---|
| 0 | System | (none in v1) | small (nonces + heights) |
| 1 | Balances | Transfer | medium (HashMap<PeerId, u64>) |
| 2 | Contributions | RecordShare, RecordConsumption, Redeem | medium |
| 3 | Nai | DirectSale, SetDirectSaleWallet, Spend, OpenChannelSession, SetChannelSessionCost | medium |
| 4 | Identity | Delegate, Revoke | small (node -> wallet) |
| 5 | Authority | Add, Remove, RotateKey, SetChannels, Dissolve | small |
| 6 | Economy | SetEmissionTarget | medium (sliding window) |
| 7 | Market | PostAsk, CancelAsk, PostBid, CancelBid, ClaimFill, Settle, FailClaim | medium |
| 8 | Governance | Propose, Vote, Enact | small |
| 9 | Reputation | Rate | small (bounded by the rating window) |
| 10 | Finality | Finalize, ReportEquivocation | small |

### 4.2 Genesis state

- 1,000,000 N-AI minted to the authority's wallet (treasury for
  airdrops, partnerships, liquidity).
- `authority.validators = [team_pubkey()]` if non-zero, else
  empty (debug builds only; release panics).
- `nai.direct_sale_wallet = None` (set by SetDirectSaleWallet at
  boot when `NEPHERAL_LTC_DIRECT_SALE_ADDRESS` is configured).
- `economy = EconomyStorage::default()` with
  `current_shares_per_credit = 1500` and
  `target_credits_per_hour = 1000`.

### 4.3 Block production

Authority nodes run a slot-driven producer:

- 2-second slot anchored to `timestamp_ms / SLOT_DURATION_MS`.
- Early-block trigger above `EARLY_BLOCK_EXT_THRESHOLD = 8`
  pending extrinsics OR `EARLY_BLOCK_LOTTERY_THRESHOLD = 4`
  pending lottery results.
- Slot ownership: `authorities[slot % len]`, where `authorities` is
  read from chain state at every block. Each producer signs with its
  own mesh identity; an offline producer leaves its slots empty and
  the chain continues with the others. A block's slot must be later
  than its parent's.
- Fork choice: longest valid chain above the last finalised block,
  never reorganising more than 64 blocks.
- Finality: every 16 blocks each validator signs the block at that
  height; two thirds plus one of the set form a justification that is
  included on chain (`Finality::Finalize`). No node reorganises below
  it. Signing two blocks at one height is an equivocation that anyone
  can prove, and the chain removes the offender (founder 0 is recorded,
  not removed). Nodes keep state checkpoints every 16 blocks to rebuild a
  fork point cheaply and return orphaned transactions to the pool.
- 100 ms producer poll cadence — fast enough to catch early-
  block triggers without busy-looping.

Block validation order at the receiver:

1. Height continuity and parent hash linkage (the genesis parent is the
   chain id, so blocks from another network never link).
2. Timestamp and size caps.
3. Header signature, domain-separated, against the slot's primary in
   the validator set derived from the parent state. An empty set is
   rejected, and the slot must be later than the parent's.
4. Extrinsics root and lottery root recomputed from the body, signatures
   included, so a body cannot be swapped under a signed header.
5. Sequential apply of every extrinsic and lottery result on a snapshot.
6. The post-apply state root (every pallet, authority and economy
   included) must match the header. Any failure restores the snapshot
   in full.

After successful apply, the dispatcher feeds the block's
share-equivalent emission (RecordShare count + Σ lottery
dimensions) into `economy.record_block_activity` and triggers
`recompute_ratio` every `ADJUSTMENT_PERIOD_BLOCKS` (300, ≈10
minutes).

### 4.4 Persistence

Every node keeps the full chain from genesis and persists it to
`~/.nepheral/<network>/chain.bin`: a header (magic and chain generation)
followed by length-prefixed postcard blocks. New blocks are appended and
fsynced; after a reorganisation the file is rewritten to a temporary file,
fsynced and renamed. On startup every block is replayed through the same
validation path; an unreadable file is quarantined rather than deleted.
Nodes catch up with batched block sync (256 blocks per request) and can
always sync the whole chain from any peer.

---

## 5. Economy

### 5.1 Credit unit

The on-chain unit is the N-AI credit, an integer count tracked by
Balances. Peg: 1 LTC = NAI_CREDITS_PER_LTC credits (initial 5,000).
At LTC ≈ USD 130, 1 N-AI ≈ USD 0.026.

### 5.2 Emission

Credits enter circulation via two paths:

- **DirectSale** — signed by the sale oracle (or the founder), idempotent on `payment_ref` (an opaque, currency-agnostic reference; Litecoin today),
  capped at `MAX_DIRECT_SALE_CREDITS = 5,000` (= 1 LTC). The
  poller observes confirmed (≥6 conf) Litecoin payments to the
  on-chain `direct_sale_wallet`, parses the OP_RETURN for the
  buyer's 32-byte wallet root, and emits one DirectSale per tx.
- **Contribution Redeem** — peer-signed. Converts accumulated
  custodian shares + lottery dimension into credits at the live
  `current_shares_per_credit` ratio.

### 5.3 Burn

Credits leave circulation via:

- **Spend** — burns `PRIORITY_SESSION_COST = 10` credits per
  standard-tier session, opens an NaiSession with
  `INFERENCES_PER_SESSION = 10` budget over
  `SESSION_TTL_MS = 1 h`.
- **OpenChannelSession** — burns `top_tier_session_costs[channel]`
  credits and opens a session bound to a specific Top-Tier
  channel.

Burned credits do not return to circulation. Supply identity:

```
supply(H) = TREASURY_GENESIS_AMOUNT
          + Σ Nai::DirectSale.credits applied up to H
          + Σ Contributions::Redeem.credits minted up to H
          - Σ Nai::Spend / Nai::OpenChannelSession burns up to H
```

### 5.4 Per-win dimension

Each lottery win carries a `dimension: u64` computed by the
authority at the moment it queues the LotteryResult, signed into
the block header. The formula in
`core-crypto/src/lottery.rs::compute_dimension`:

```
token_log    = ilog2(1 + output_tokens / 100)
model_factor = max(1, model_params_billions)
time_log     = ilog2(1 + gen_time_ms / 100)
dimension    = DIMENSION_BASE (=25)
             * max(1, token_log + 1)
             * model_factor
             + min(4, time_log / 2)
```

Reference values (model_b, output_tokens, gen_time_ms):

| Profile | dimension |
|---|---|
| Qwen3-4B, 500 tokens, 10 s   | ~225 |
| Qwen3-32B, 500 tokens, 5 s    | ~1,800 |
| Qwen3-72B, 500 tokens, 1 s    | ~3,825 |
| Qwen3-72B, 2000 tokens, 1 s   | ~7,650 |
| 405B, 500 tokens, 5 s         | ~21,000 |

The formula is intentionally logarithmic in output tokens so a
responder cannot inflate by padding output; the time bonus is
capped so a slow CPU run cannot out-earn a fast H100.

### 5.5 Redeem formula

```
total_share_equivs = shares + lottery_dimension
credits            = total_share_equivs / current_shares_per_credit
```

`current_shares_per_credit` is the live ratio from
`EconomyStorage`, threaded into ApplyCtx by the dispatcher. The
sub-credit remainder rolls into the next Redeem call — the
pallet refuses a 0-credit Redeem to avoid silently advancing the
`redeemed` counter.

### 5.6 Dynamic emission target

The Economy pallet maintains:

- A sliding window of per-block share-equivalent emission
  (`ADJUSTMENT_WINDOW_BLOCKS = 1,800` ≈ 1 hour).
- A current ratio `current_shares_per_credit`, recomputed every
  `ADJUSTMENT_PERIOD_BLOCKS = 300` blocks ≈ 10 minutes.
- An authority-set `target_credits_per_hour` (default 1,000 ≈
  USD 26 / h).

The recompute formula:

```
total_equivs = Σ window
window_ms    = window_blocks * SLOT_DURATION_MS
target_per_window = target_credits_per_hour * window_ms / 3_600_000
new_ratio    = clamp(MIN..=MAX, total_equivs / target_per_window)
```

Result: total credits emitted per hour stays anchored at
`target_credits_per_hour` regardless of how busy the mesh is. As
the network grows, individual share value drops proportionally.

---

## 6. Top-Tier, the Authority pallet and governance

Top-Tier and business nodes are founding nodes in the authority set.
Founder 0 (the genesis authority) admits and removes them with
`Authority::Add` / `Authority::Remove`, giving a `channels` list and an
`attestation_hash`. A founding node manages only itself: its channels, its
key and its resignation. Every node in the set also produces blocks in its
own slots.

Nobody can remove founder 0. When founder 0 resigns (`Authority::Dissolve`,
one-way, refused until governance has replaced the sale oracle by vote),
every founder title ends at once, while every founding node keeps serving
its channels and producing blocks exactly as before. From then on the
economic levers and the node set move only by vote: identities with at
least 7 days of recorded contribution, weighted by it, a 20,000-block
voting period, a 20,000-block delay, and a two-thirds quorum with 20%
participation. The full rules are in `docs/GOVERNANCE.md`.

Initial channels:

- `large_chat` — 70B class, mid-latency (~500 ms p50)
- `large_realtime` — 70B class, low-latency (<200 ms p50)
- `frontier` — 405B class or larger, throughput-batched

Each channel has its own session cost (set by authority via
`Nai::SetChannelSessionCost`). The lottery filters candidates by
`chain.authority.metadata[peer].channels` when a SharePush
includes `target_channel = Some(name)`.

MVP attestation policy: authority-curated whitelist. The
operator submits specs + endpoint to a team channel (off-chain,
email/Discord), the team audits manually, and on acceptance the
authority emits `Authority::Add` with the SHA-256 of the
approved specs as `attestation_hash` -- a value nothing verifies,
so the field records an audit, not a proof. TPM 2.0 quote
verification is the planned replacement for this manual flow;
see section 10 for why SGX is not a candidate and what each
attestation tier can and cannot establish.

---

## 7. Privacy guarantees

| Property | Mechanism | Status |
|---|---|---|
| Personal data never leaves the sender | Sender-side redaction with local restore | Pattern-based, extendable with `redact_terms` |
| Custodians cannot read the query | XChaCha20-Poly1305 + Shamir below threshold | Yes |
| Only ranked responders can decrypt | Session key wrapped for the top 3 only | Yes |
| No responder sees a whole conversation | Per-message lottery, blind memory | Yes |
| Bad responders fade out | Receipt-bound ratings weighted by contribution, lottery weights | Yes |
| Peers cannot impersonate each other | Mutual TLS bound to Ed25519 identities | Yes |
| Rooms are unreadable without the code | Code-derived key, sealed and signed envelopes | Yes |
| Forward secrecy per message | Ephemeral X25519 per bundle and per response | Yes |
| Traffic analysis | None | Known limitation |
| Plaintext at the responder | The responder reads the redacted query | Known limitation; TEE inference is the long-term path |

---

## 8. Identity

A node needs its key every few seconds - to greet peers, claim lottery
slots, sign answers and receipts, redeem what it earned - so that key has to
open without anyone present. A key like that should not also hold funds.
Two keys therefore do two jobs:

- **The node key** is the network identity: peer_id == its ed25519
  verifying key, also the key of the node's TLS certificate. The desktop
  keeps it in `~/.nepheral/node.bin`, sealed with DPAPI on Windows (bound
  to the user's login) and owner-only (0600) elsewhere, beside the local
  system prompt. A headless node keeps it in `~/.nepheral/identity.mnemonic`
  (0600) or takes it from `NEPHERAL_NODE_MNEMONIC`. It holds no funds.
- **The wallet key** comes from the 24-word BIP-39 phrase and owns the
  balance. It lives in a password vault, `~/.nepheral/vault.bin` (Argon2id,
  64 MiB, 3 passes; XChaCha20-Poly1305 over a header-authenticated file), and
  is in memory only while its owner has unlocked it - by hand, until locked
  again or after a stretch of inactivity (15 minutes by default).

The Identity pallet (pallet_idx 4) links them. `Delegate { node, node_sig }`
is signed by the wallet and countersigned by the node over a network-bound
digest, so neither side can be claimed without its consent; `Revoke` is
signed by either. Delegations are one level deep, at most eight nodes per
wallet, and the founder's key cannot be delegated. What a delegation changes:

- A delegated node never holds credits: whatever lands on it - its own
  redeems, transfers, refunds, what it held before - moves to its wallet at
  the end of the extrinsic. The node keeps redeeming unattended; the wallet
  is paid.
- A node may use the sessions its wallet paid for (`UseSession`, and the
  responder's check of who may ask on a channel).
- Market standing and voting weight count the wallet together with its
  nodes; a delegated node has neither of its own, so nothing counts twice.

The pallet joins the state root only once it holds a delegation, so the
history sealed before it existed reconciles to the same roots and the chain
needed no new generation.

Locked, a node serves the network and its local API keeps answering whoever
presents the API token: the token is checked against a hash in config.json,
the token itself is kept in the vault. Chats, payments, trades and votes wait
for the unlock, and each payment, trade or vote is confirmed again - in a
native dialog on the desktop, which the page cannot answer, or at the
command line on a headless node (`nepheral-node unlock`, `wallet send`).

The same phrase opens the same wallet on testnet and mainnet; chain and
runtime data are kept per network under `~/.nepheral/<network>/`. Same
phrase on a new machine -> same balance, and that machine's node joins the
wallet with a key of its own.

---

## 9. Wire protocol summary

Protocol version 3. Frames are a one-byte type followed by a postcard
body. Every message that carries authority is signed under its own
domain and the chain id.

| Hex | Name | Note |
|---|---|---|
| 0x05 | HELLO | identity, model info, timestamp |
| 0x10 | PEER_LIST | bounded peer list |
| 0x11 | GOSSIP_PEER | discovered peer |
| 0x12 | PEER_LIST_REQUEST | discovery |
| 0x24 | ROOM_ENVELOPE | sealed, signed room payload |
| 0x41 | NODE_STATS | capability heartbeat |
| 0x43 | MODEL_ANNOUNCE | model hash and size |
| 0x44 | CHAIN_TX | signed extrinsic |
| 0x4A | BLOCK_BROADCAST | signed block |
| 0x4B | BLOCK_SYNC_REQUEST | batched catch-up |
| 0x4C | FINALITY_VOTE | validator vote for a block height |
| 0x50 | MODEL_REQUEST | request a trusted model by hash |
| 0x53 | MODEL_TRANSFER | model bytes, hash verified |
| 0x60 | SHARE_PUSH | one share + lottery inputs |
| 0x61 | SHARE_CLAIM | ranked claim, VRF seed |
| 0x63 | AGENT_RESPONSE_DIRECT | sealed answer + service claim |
| 0x70 | RELAY_OPEN | relays SHARE_PUSH / AGENT_RESPONSE_DIRECT only |

Bodies are decoded with an 8 MB hard cap and per-type limits below it.

---

## 10. Trust tiers

Attestation is an **additive capability, never a gate on participation**. A
node that cannot attest keeps every ability it has today: joining the mesh,
relaying Shamir shares, answering the standard lottery, earning and redeeming
credits, and trading on the NAI board. What attestation unlocks is a narrower
set of channels whose buyers are paying for a stronger claim.

This mirrors the Additional Use Grant in the project licence, which protects
"operating one or more Nepheral nodes... whether or not You operate at
commercial scale" as the intended use. A hardware requirement that silently
became a membership requirement would contradict that.

### 10.1 What the tiers are

| Tier | Hardware | What it proves | Where it applies |
|---|---|---|---|
| Open | any | nothing | everything above — the floor, never gated |
| Measured | fTPM / Intel PTT (any Ryzen or Core desktop since ~2016) | measured boot: this host booted an unmodified image | replaces the manual Top-Tier whitelist |
| Confidential | EPYC + SEV-SNP, Xeon + TDX, optionally NVIDIA GPU CC | isolated execution, memory encrypted against the host | rented server infrastructure only |

The split between Measured and Confidential is not a matter of degree, and
conflating them would be the easiest way to oversell this. A TPM attests what
was *booted*; the operating system remains inside the trust boundary, so the
machine's owner can still read the process's memory. An enclave removes the
host OS and hypervisor from that boundary, which is what actually resists a
memory dump.

### 10.2 Why the Confidential tier is server-only

There is no consumer path to the Confidential tier from either vendor, and
this is a market fact rather than a temporary gap:

- **AMD SEV-SNP** is EPYC-only. It has never shipped on consumer Ryzen.
- **Intel SGX** was deprecated in 11th-generation Core parts and removed from
  12th generation onward. It survives only in Xeon.
- **NVIDIA GPU Confidential Computing** begins at H100. No GeForce part has it.

So the common enthusiast configuration — a Ryzen CPU with a GeForce card, which
is exactly what this project's CUDA and Vulkan backends target — has no enclave
option at all. Gating a tier on enclave hardware would exclude the network's
most capable inference nodes while admitting rented servers with weaker GPUs.
The Confidential tier therefore describes infrastructure the project itself
rents, not something asked of participants.

### 10.3 Prior art

None of the mechanism here is novel, and the design deliberately adopts
existing systems rather than reinventing them:

- **Keylime** (CNCF) already does continuous TPM attestation with periodic
  quotes and revocation on failure.
- **Trustee / Key Broker Service** (Confidential Containers) already does
  attestation-gated secret release against TDX, SEV-SNP, SGX and Arm CCA
  evidence.
- **TCG "TPM 2.0 Keys for Device Identity and Attestation"** already specifies
  the EK→AK flow for binding a non-exportable key to a specific TPM.

What is particular to Nepheral is the composition — Shamir-split prompts with
VRF lottery selection — and the posture: attestation as an opt-in tier inside a
peer-to-peer inference network, with explicit degradation for nodes without it.

### 10.4 Non-goals, stated plainly

- **Attestation does not make prompt extraction impossible.** A responder must
  decrypt to run inference. The Measured tier does not remove the host OS from
  the trust boundary at all; the Confidential tier removes it but not physical
  access.
- **The hardware itself is an active research target.** RMPocalypse
  (CVE-2025-0033) and Heracles both broke SEV-SNP at CCS 2025. TEE.Fail
  extracted ECDSA attestation keys from Intel's Provisioning Certification
  Enclave via a sub-$1,000 DDR5 memory-bus interposer, forging quotes that pass
  DCAP verification at the highest trust level — and the same extracted keys
  bypass NVIDIA GPU CC attestation. Treat every tier as raising cost, not as
  proof.
- **A forged quote is not fixed by re-verifying more often.** TEE.Fail mints
  fresh valid quotes rather than replaying old ones, so a heartbeat cadence
  bounds a stolen *session*, not a stolen *signing key*. Vendor key revocation
  is the mitigation.
- **Attestation does not address content integrity or metadata.** Corpus
  poisoning is answered by signing; who-downloaded-what is answered by not
  authenticating downloads per wallet.

### 10.5 Sybil resistance is the nearer prize

The immediate value of TPM hardware here is not confidentiality but identity.
Minting fresh peer keys is free, which is why the NAI board needs a tenure and
contribution threshold before an account may trade. A key certified as
non-exportable inside a specific TPM makes one machine equal one identity, and
the standard EK→AK flow needs no reference measurements — only a vendor
certificate chain. That is a smaller, better-specified problem than
whole-system measured boot across heterogeneous consumer hardware, and it is
the piece worth building first.

---

## 11. Open issues and roadmap

| Item | Plan |
|---|---|
| Finality | Shipped: two-thirds-plus-one votes of the founding nodes every 16 blocks, on-chain justifications, automatic removal on equivocation |
| GRANDPA-lite finality | After multi-validator |
| Real TPM attestation | Whitelist today; fTPM/PTT quote verification replaces it. SGX is not a candidate — deprecated in 11th-gen Core, gone from 12th on |
| Identity pallet | Shipped: pallet_idx 4 links a node key to the wallet that owns its earnings; the same link lets a node rotate its key without moving funds |
| Mainnet launch | Same binary; switch with `network` in config or `NEPHERAL_NETWORK` once testnet is validated |
| Blind AI (TEE inference) | Long-term — requires TDX or SEV-SNP (both server-only) plus NVIDIA GPU CC; see §10 |
| Founder resignation | Shipped as a one-way call by founder 0 alone, gated on governance having replaced the sale oracle by vote |

---

## 12. References

- Reference implementation: `core-crypto/src/`, `mesh-network/src/`,
  `app-kernel/src/`.
- Pricing math and worked examples: `docs/ECONOMY.md`.

This document is a design snapshot, not a published whitepaper; values,
constants, and unstable interfaces are tagged as such throughout.
