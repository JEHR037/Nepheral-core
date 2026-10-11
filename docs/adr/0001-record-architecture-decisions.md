# ADR-0001: Record architecture decisions

- **Status:** Accepted
- **Date:** 2026-10-10
- **Deciders:** Javier Hernandez

## Context

Until the first public release, the reasons behind Nepheral's design lived in
three places: the design documents in `docs/`, long comments in the code,
and the memory of the people who wrote it. The design documents describe
the system as it is, not why each part was chosen over the alternatives.
The comments explain a function, not a decision that spans several crates.

Publishing the code changes who reads it. Node operators, auditors and new
contributors need to know which properties are deliberate, which are
trade-offs, and which are open to change, without reconstructing that from
the history.

## Options considered

### Option 1: keep the reasons in the design documents

- Benefits: no new process; the documents already exist.
- Drawbacks: the documents are rewritten as the system changes, so the
  reasons for the previous design disappear. Nothing records the options
  that were rejected.

### Option 2: Architecture Decision Records in the repository

Short Markdown files, one per decision, reviewed through pull requests.

- Benefits: versioned with the code; reviewed like the code; an accepted
  record is never rewritten, so the history of reasoning survives.
- Drawbacks: one more file to write for significant changes; records can go
  stale if superseding them is forgotten.

### Option 3: an external wiki or decision tool

- Benefits: easier to edit and search.
- Drawbacks: not versioned with the code; not reviewed; lost or out of
  reach when the tool changes.

## Decision

Nepheral records significant decisions as ADRs in `docs/adr`, following the
process and the template described in [the ADR index](README.md). Larger
proposals that need discussion first start as RFCs in `docs/rfcs`.

### Key points

- **One decision per record:** small enough to review and to supersede.
- **Immutable once accepted:** a changed decision gets a new ADR; the old one
  only changes its status.
- **Options with benefits and drawbacks:** a record without the rejected
  options is a description, not a decision.

## Consequences

### Positive

- The reasons for the design are public, reviewable and kept.
- Contributors can check a proposal against the decisions it would change.

### Tradeoffs

- Writing an ADR slows down significant changes a little; that is
  intended.
- The retroactive records (0002 to 0016) were written after the fact and
  may miss options that were discussed at the time.

## Implementation

### Where it lives

- `docs/adr/README.md`: process, requirements and index.
- `docs/adr/0000-template.md`: the template.
- `docs/rfcs/README.md`: the RFC process.

### Next steps

1. Every pull request that changes a decision recorded here links the ADR
   that supersedes it.

## Related decisions

- All later ADRs follow this process.

## References

- Michael Nygard, "Documenting Architecture Decisions" (2011).

## History

- v1.0 (2026-10-10): Initial decision.
