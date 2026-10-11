# ADR-0009: Rule changes are scheduled on chain; old nodes halt instead of forking

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

When the chain's rules change, a node that has not updated computes a
different state from the same blocks. If it keeps producing, the network
splits into two chains that both look valid to their own nodes. Coordinating
every operator to update at the same moment by hand does not scale beyond a
handful of machines.

## Options considered

### Option 1: coordinated manual updates

- Benefits: no protocol machinery.
- Drawbacks: any operator who misses the window forks; does not scale.

### Option 2: a new chain for every rule change

- Benefits: clean state.
- Drawbacks: balances and history are lost or must be migrated by hand.

### Option 3: rule versions scheduled on chain, with a halt for unsupported versions

- Benefits: every node switches at the same block; a node that cannot
  switch stops and says why instead of forking.
- Drawbacks: an outdated node stops serving until it updates.

## Decision

The rules in force carry a version, starting at 1. The founder, or
governance once the founder has resigned, schedules the next version for a
wall-clock time at least the minimum notice away: 10 minutes on testnet,
7 days on mainnet. At the first block whose timestamp reaches it, every node
switches at once and runs the migration. A node whose build does not
implement the scheduled version stops applying and producing blocks, keeps
serving what it has, and tells its operator to update.

### Key points

- **Wall-clock notice, not block counts:** empty slots make block time
  irregular.
- **Wire transition window:** peers accept protocol versions from
  `MIN_PROTOCOL_VERSION` to `PROTOCOL_VERSION`, so nodes one version apart
  keep talking while they update.
- **Signed automatic updates** make the notice period enough in practice.

## Consequences

### Positive

- No silent forks: an outdated node stops visibly.
- Rule changes keep the chain, its balances and its history.

### Tradeoffs

- Availability depends on operators updating within the notice.
- The migration code for every version stays in the binary.

## Implementation

### Where it lives

- `core-crypto/src/pallets/upgrade.rs`: versions, scheduling, minimum
  notice.
- `core-crypto/src/chain.rs`: activation before the block's transactions.
- `mesh-network/src/protocol.rs`: `PROTOCOL_VERSION`,
  `MIN_PROTOCOL_VERSION`.
- `nepheral-desktop/src/updates.rs`: signed updates.

## Related decisions

- ADR-0007: the chain whose rules change.
- ADR-0008: who may schedule a change.

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
