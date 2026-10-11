# ADR-0007: A purpose-built pallet chain instead of an external ledger

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

The network has to record who contributed, who won each lottery, credit
balances, market escrow, governance and ratings, in a way every participant
can verify. Every install is a full node, including small machines, and
people asking questions must not need a gas token or a wallet in another
system.

## Options considered

### Option 1: an existing public blockchain or rollup

- Benefits: established security and tooling.
- Drawbacks: every receipt and lottery result would cost fees in another
  token; latency and throughput set by another network; receipts would be
  public on a chain the project does not control.

### Option 2: a general framework such as Substrate

- Benefits: consensus, networking and storage already built.
- Drawbacks: a large runtime, binary and sync burden for a desktop or a
  phone; much of it unused.

### Option 3: a small chain written for this protocol

- Benefits: only the calls the protocol needs; deterministic replay that
  every node runs; small enough to audit.
- Drawbacks: the consensus and state code are the project's own and must
  be reviewed as such; no ecosystem tooling.

## Decision

Nepheral runs its own chain, in the style of Substrate's pallets, inside
`core-crypto`. State is held per pallet; the pallet roots combine into a
`state_root` that every block header commits to and every receiver checks
after applying the block.

### Key points

- **Pallets:** System, Balances, Contributions, Nai, Identity, Authority,
  Economy, Market, Governance, Reputation, Finality and Upgrade.
- **Two-second slots,** with early blocks when many transactions wait.
- **Full verification on receipt:** roots recomputed from the body, state
  root checked after apply, previous state restored on any failure.
- **Every node keeps and serves the full chain;** light clients start from a
  verified snapshot instead (ADR-0016).

## Consequences

### Positive

- No external token or fee to use the network.
- The rules and their state fit in one crate that can be audited alone.

### Tradeoffs

- The validator set is small at the start (ADR-0008), so the chain is
  only as decentralised as that set.
- Tooling (explorers, indexers) has to be built by the project.

## Implementation

### Where it lives

- `core-crypto/src/chain.rs`, `block.rs`, `extrinsic.rs`, `pallets/`.
- `core-crypto/src/chain_file.rs`: the append-only chain file.
- `app-kernel/src/chain_node.rs`: production, sync and persistence.

## Related decisions

- ADR-0008: who produces and finalises blocks.
- ADR-0009: how the rules change.
- ADR-0016: light clients.

## References

- `docs/WHITEPAPER.md`, section 4.

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
