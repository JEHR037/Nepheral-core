# ADR-0005: Shamir-split custody on every node

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

Between the asker and the winner, the encrypted query has to wait somewhere
while the ranks are resolved. Wherever it waits whole, a single node can
store it and attack it later, or drop it. An early version split each query
only among the asker's direct connections; with most nodes behind NAT, that
put every share on the few public servers.

## Options considered

### Option 1: send the encrypted bundle straight to the ranked candidates

- Benefits: simple; one message per candidate.
- Drawbacks: every candidate holds the whole ciphertext, rank or not.

### Option 2: split the encrypted bundle with Shamir's scheme among custodians

- Benefits: below the threshold a custodian learns nothing; no single node
  can withhold the query.
- Drawbacks: more messages; availability depends on enough custodians
  answering.

### Option 3: keep custody on dedicated servers

- Benefits: predictable availability.
- Drawbacks: concentrates the material on infrastructure one party runs.

## Decision

The query bundle is encrypted with XChaCha20-Poly1305, and the encrypted
bundle is split into Shamir shares over GF(2^8), with a threshold of
max(2, ceil(n/2)), one share per custodian. Every node holds shares, whether
or not it runs a model; only nodes whose model is ready enter the lottery.
Shares for a node out of direct reach travel sealed to it through relays.

### Key points

- **Custody is the backbone:** every install contributes, not only the
  nodes with a GPU.
- **Custody earns:** custodians are credited through custody attestations.
- **Fresh identities are limited:** keys with no on-chain history never
  outnumber established custodians, so they cannot withhold enough shares
  to stop a question.

## Consequences

### Positive

- No custodian, relay or server can rebuild a query below the threshold.
- Load and reward spread across every node.

### Tradeoffs

- Bandwidth: one share per custodian for every message.
- A question fails if fewer than the threshold of custodians release.

## Implementation

### Where it lives

- `core-crypto` (Shamir over GF(2^8), agent cryptography, custody
  attestations in `receipts.rs`).
- `app-kernel/src/agent.rs`: choosing custodians, pushing and releasing
  shares.
- `app-kernel/src/sybil.rs`: limits for identities with no history.

## Related decisions

- ADR-0004: who the shares are released to.
- ADR-0012: how shares reach nodes behind NAT.

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
