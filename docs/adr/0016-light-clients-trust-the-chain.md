# ADR-0016: Light clients trust the chain, not keys built into the app

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

A phone, or any node that wants to start quickly, cannot replay every block
since genesis. It still has to know that the state it starts from is the one
the validators finalised. The shortcut, shipping the validators' keys inside
the app, breaks the first time the validator set changes and turns the app
into the root of trust.

## Options considered

### Option 1: replay the full chain

- Benefits: no new trust assumptions.
- Drawbacks: slow and heavy on phones; grows without bound.

### Option 2: trust validator keys built into the app

- Benefits: simple verification.
- Drawbacks: every change of the set needs an app update; whoever builds the
  app decides which validators to trust.

### Option 3: follow every validator-set change from genesis

- Benefits: the trust comes from the chain itself; a server cannot get an
  unfinalised state accepted.
- Drawbacks: proofs to build, serve and verify.

## Decision

A light client computes the genesis state itself, with the team key as the
only validator, and follows every change of the validator set. Each change
travels as a handoff: the last finalised state before the change, checked
against its header's state root, then the blocks up to the first checkpoint
after the change, applied with the same `apply_block` every node runs. The
set they produce is the one that must have signed that checkpoint. Last
comes the newest finalised state, finalised by the set the handoffs ended
with. From there the client applies ordinary blocks.

### Key points

- **Same rules as a full node:** the blocks in a handoff are applied with
  the normal block-application code, signatures and state roots included.
- **A dishonest server can only fail or lag:** leaving a change out makes
  verification fail or hands over an older state, which ordinary blocks
  then bring up to date.

## Consequences

### Positive

- Fast start without trusting the app's builder or the serving node.
- Validator changes need no app update.

### Tradeoffs

- Full nodes must keep and serve handoff proofs.
- Verification cost grows with the number of validator-set changes.

## Implementation

### Where it lives

- `core-crypto/src/light.rs`: handoffs and proof verification.
- `app-kernel/src/light_sync.rs`: serving proofs.

## Related decisions

- ADR-0008: the validator set the client follows.
- ADR-0007: the state roots it checks.

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
