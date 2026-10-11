# ADR-0018: Only dependencies whose licences fit the licence model

- **Status:** Accepted
- **Date:** 2026-10-10
- **Deciders:** Javier Hernandez

## Context

Nepheral is distributed under the Business Source License 1.1, each version
converts to the Apache License 2.0 on its change date, and the Licensor
offers commercial licences for uses the grant excludes (ADR-0017). Binaries
of the desktop client and the headless node are distributed to the public.

Every dependency travels with those binaries. A dependency under a licence
that imposes its own terms on the whole program (GPL, AGPL, LGPL when linked
statically, as Rust does), that forbids commercial use, or that restricts the
field of use would make the distribution non-compliant, block the conversion
to Apache-2.0, or make commercial licensing impossible. Removing such a
dependency after the fact can mean rewriting the code built on it.

## Options considered

### Option 1: review licences case by case in pull requests

- Benefits: no tooling.
- Drawbacks: transitive dependencies go unnoticed; one missed review is
  enough.

### Option 2: an allow list, recorded in an ADR and enforced in CI

- Benefits: every dependency, direct or transitive, is checked on every
  pull request; the rule is written down before anyone proposes a library.
- Drawbacks: a useful library under an excluded licence cannot be used, and
  the list needs a new ADR to change.

### Option 3: no third-party dependencies beyond the standard library

- Benefits: no licence risk.
- Drawbacks: unrealistic for cryptography, networking and inference.

## Decision

Only licences on this list may enter the dependency tree, for libraries,
vendored code, model weights and datasets that the project builds in or
distributes:

| Licence | Notes |
|---|---|
| MIT, MIT-0, Apache-2.0, Apache-2.0 WITH LLVM-exception | Permissive |
| BSD-1-Clause, BSD-2-Clause, BSD-3-Clause, 0BSD, ISC, Zlib, BSL-1.0 | Permissive |
| Unlicense, CC0-1.0 | Public-domain dedications |
| Unicode-3.0, CDLA-Permissive-2.0 | Permissive, for Unicode tables and certificate data |
| MPL-2.0 | File-level copyleft: allowed for crates used unmodified; any change to an MPL file is published under MPL-2.0 |

Not allowed: GPL, LGPL, AGPL and other strong copyleft licences; SSPL, the
Business Source License or any other source-available licence on a
dependency; non-commercial (NC) and no-derivatives (ND) licences; anything
without a clear licence. A dependency offered under a choice of licences
(`MIT OR LGPL-2.1`) is allowed when at least one choice is on the list, and
is used under that one.

Models and data that the project distributes, such as the conversation
memory model, need a licence that allows redistribution and commercial use;
their notices go in `legal/`. Content offered for retrieval under a
share-alike licence (CC BY-SA) is distributed as data, with its attribution,
and is never merged into the code.

### Key points

- **Every ADR and RFC lists the new dependencies it adopts, with their
  licences.** A proposal that needs a licence outside the list is rejected,
  or comes with an ADR that changes this list first.
- **CI enforces it:** `cargo deny check` runs the allow list in `deny.toml`
  on every pull request, transitive dependencies included.
- **Notices:** a dependency whose licence requires a notice is added to
  `legal/THIRD-PARTY-NOTICES.md` in the same pull request.

## Consequences

### Positive

- Distributing binaries, converting to Apache-2.0 and offering commercial
  licences stay possible.
- A licence problem is caught when the dependency is proposed, not after
  code depends on it.

### Tradeoffs

- Some useful libraries are out of reach.
- MPL-2.0 dependencies carry an obligation to publish changes to their
  files.

## Implementation

### Where it lives

- `deny.toml`: the allow list and the other dependency checks.
- `.github/workflows/ci.yml`: the `Dependencies` job.
- `docs/adr/0000-template.md` and `docs/rfcs/README.md`: the "Dependencies
  and licences" section.

## Related decisions

- ADR-0017: the licence model this list protects.

## References

- SPDX License List, https://spdx.org/licenses/

## History

- v1.0 (2026-10-10): Initial decision.
