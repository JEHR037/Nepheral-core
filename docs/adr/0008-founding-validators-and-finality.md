# ADR-0008: Founding validators, slot production and finality

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

A young network has few operators anyone can hold accountable. Open
proof-of-stake needs a valuable stake and many independent validators;
proof-of-work wastes the compute the network exists to share. The chain
still has to keep running when the founder is offline, and has to reach a
point beyond which history cannot be rewritten.

## Options considered

### Option 1: a single authority

- Benefits: simplest.
- Drawbacks: one key can stop or rewrite the chain.

### Option 2: open proof-of-stake from day one

- Benefits: permissionless validation.
- Drawbacks: no meaningful stake exists yet; a cheap stake is a cheap
  attack.

### Option 3: a set of founding nodes, read from chain state, with finality votes

- Benefits: production continues while any founding node is online;
  history becomes final once two thirds plus one agree; the set can change
  on chain.
- Drawbacks: the set is admitted by the founder until governance takes
  over.

## Decision

Block producers are the authority set recorded in chain state. Slot `s`
belongs to `authorities[s % len]`; an offline producer only leaves its slots
empty. Fork choice is the longest valid chain above the last finalised
block, with reorganisations capped at 64 blocks. Every 16 blocks each
validator signs the block at that height, and two thirds plus one form a
justification that is recorded on chain; no node reorganises below it. A
validator that signs two blocks at one height can be proven to have done so
and is removed.

### Key points

- **Founder 0** admits and removes founding nodes. Its resignation is one
  way and ends every founder title; from then on the set moves only by
  governance vote.
- **Finality never stops production:** without enough votes it lags, and
  blocks keep coming.

## Consequences

### Positive

- The chain survives the founder going offline.
- Finalised history cannot be reorganised.

### Tradeoffs

- Finality tolerates faults only from four founding nodes upwards
  (3f + 1). With fewer, one offline validator delays finality.
- Until resignation, the founder controls who validates.

## Implementation

### Where it lives

- `core-crypto/src/finality.rs`, `validators.rs`,
  `pallets/authority.rs`, `pallets/governance.rs`.
- `app-kernel/src/finality.rs`, `chain_node.rs`.

## Related decisions

- ADR-0007: the chain.
- ADR-0016: light clients follow changes of this set.

## References

- `docs/GOVERNANCE.md`.

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
