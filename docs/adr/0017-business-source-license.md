# ADR-0017: Business Source License with conversion to Apache-2.0

- **Status:** Accepted
- **Date:** 2026-10-10
- **Deciders:** Javier Hernandez

## Context

Publishing the source lets operators, auditors and users verify what a node
does with their questions and their credits; a privacy claim that cannot be
checked is only a promise. The same publication would let anyone take the
code and run it as a competing hosted service, or start a copy of the
network with its own credits sold for money, before the project can sustain
itself.

## Options considered

### Option 1: a permissive licence (MIT or Apache-2.0)

- Benefits: the widest adoption; recognised as open source.
- Drawbacks: allows hosted resale and competing networks immediately.

### Option 2: a copyleft licence (AGPL-3.0)

- Benefits: open source; hosted modifications must be published.
- Drawbacks: still allows a competing network or a hosted service, as long
  as the source is shared.

### Option 3: the Business Source License 1.1 with an additional use grant

- Benefits: source available to everyone now; running nodes, internal use,
  research and contributions are expressly allowed; each version becomes
  Apache-2.0 after four years.
- Drawbacks: not an open-source licence under the OSI definition; some
  contributors and distributions avoid it.

## Decision

Nepheral is published under the Business Source License 1.1, with the
founder as Licensor.

### Key points

- **Expressly allowed:** operating nodes on the Nepheral network, including
  for profit and at commercial scale; internal use; personal, research,
  educational and evaluation use; contributing, forking for patches,
  analyses, benchmarks and security research.
- **A Competing Use needs a commercial licence:** offering Nepheral to third
  parties as a hosted, managed, embedded or white-labelled product, or
  operating a separate network or credit system derived from it whose units
  are sold.
- **Scope:** every version from 0.1.0, including versions released only
  as binaries before the source was first published.
- **Change Date:** each version converts to the Apache License 2.0 on the
  fourth anniversary of its first public availability.
- **Third-party components keep their own licences;** they are listed in
  `legal/THIRD-PARTY-NOTICES.md`.

## Consequences

### Positive

- The code can be audited and run by anyone who joins the network.
- Every version becomes open source on a fixed date.

### Tradeoffs

- The project must not be described as open source until the change date.
- Contributions under this licence need clear terms if the Licensor is to
  offer commercial licences that include them.

## Implementation

### Where it lives

- `LICENSE`, `legal/`, and `license = "BUSL-1.1"` in `Cargo.toml`.

### Next steps

1. Define the terms for outside contributions (a contributor licence
   agreement or an equivalent) before accepting them.

## References

- Business Source License 1.1, MariaDB Corporation Ab.

## History

- v1.0 (2026-10-10): Initial decision.
