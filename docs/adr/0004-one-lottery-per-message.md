# ADR-0004: One lottery per message, no conversation affinity

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

The node that answers a question has to read it: inference runs on the
plaintext. If one node answered a whole conversation, it would hold all of
it. Conversations still need continuity, and the network needs a fair way to
choose who answers and who is paid.

## Options considered

### Option 1: a sticky responder per conversation

- Benefits: the responder keeps the full context; simplest to build.
- Drawbacks: one stranger reads the entire conversation.

### Option 2: a fresh lottery for every message, with a blind memory

- Benefits: each responder sees one redacted message, a short redacted
  window and a summary; no node keeps conversation state.
- Drawbacks: less context per answer; the quality of consecutive answers
  can vary between responders.

### Option 3: a fixed set of trusted operators

- Benefits: predictable quality.
- Drawbacks: a closed network and a central point of trust.

## Decision

Every message runs its own lottery. The candidates are ranked by a draw
every custodian can verify, weighted by answer ratings. The session key is
wrapped only for the three highest ranks. Custodians release shares by rank,
immediately for rank 0, after 8 seconds for rank 1 and after 16 seconds for
rank 2, so a single winner answers. Continuity comes from the last few
redacted messages and a blind memory: a short summary the responder updates
and returns sealed to the sender.

### Key points

- **No affinity:** the next message goes to a different draw.
- **Rank-ordered fallback:** if the winner is busy or gone, the next rank
  answers after a fixed delay.
- **The claimant is proven:** it must be the TLS-authenticated peer named at
  that rank.

## Consequences

### Positive

- No responder sees a whole conversation.
- Rewards follow the draw, which no single node controls.

### Tradeoffs

- Answers carry only the context the window and the summary hold.
- A fallback to rank 1 or 2 adds 8 or 16 seconds.
- The responder still reads the redacted message; hardware-isolated
  inference is the long-term answer to that.

## Implementation

### Where it lives

- `core-crypto/src/lottery.rs`: the ranked draw.
- `app-kernel/src/agent.rs`: building the query, claims, release by rank,
  the blind memory.
- `app-kernel/src/ratings.rs`, `core-crypto/src/pallets/reputation.rs`:
  the weights.

## Related decisions

- ADR-0005: how the encrypted query reaches the ranks.
- ADR-0006: what is removed from the message before it leaves.
- ADR-0011: what a busy winner does.

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
