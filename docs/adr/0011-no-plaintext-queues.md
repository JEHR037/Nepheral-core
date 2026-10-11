# ADR-0011: No plaintext queues on responders

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

A responder can only answer so many questions at once. The usual answer to
saturation is a queue. On a responder, a queued question is a decrypted, or
decryptable, question sitting outside inference, waiting: more material
exposed for longer on a machine the asker does not control.

## Options considered

### Option 1: a queue on each responder

- Benefits: no question is turned away; standard technique.
- Drawbacks: plaintext waits on a stranger's machine; one slow node holds
  many questions.

### Option 2: refuse fast and let the sender draw again

- Benefits: nothing waits in the clear; load spreads across the network.
- Drawbacks: a little latency when the winner is busy; more messages.

## Decision

Responders never queue private questions. A node claims a question only
when it can reserve a slot for it (one per worker; one per GPU in per-GPU
mode). A node with no free slot declines at once with a signed decline, and
the next rank takes the question. A recent decline lowers that node's weight
in the next draws for a short while, and when every candidate has declined,
the sender draws again after a short backoff.

### Key points

- **Reservation, not waiting:** a claim reserves a slot or is not made.
- **Declines are fast and signed,** so ranks behind NAT learn about them
  through the relays.

## Consequences

### Positive

- A question is decrypted only when a worker is ready to answer it.
- Saturation spreads across the network instead of piling up on one node.

### Tradeoffs

- Under heavy load an asker sees retries instead of a waiting position.
- Throughput per node is capped by its workers; the network scales by
  adding nodes.

## Implementation

### Where it lives

- `app-kernel/src/agent.rs`: slot reservation, declines, claims.
- `app-kernel/src/state.rs`: `SlotReservation`, per-GPU workers.
- `mesh-network/src/protocol.rs`: `AGENT_DECLINE`, `SignedDecline`.

## Related decisions

- ADR-0004: the ranks a declined question falls back to.

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
