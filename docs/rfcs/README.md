# Requests for Comments

An RFC proposes a change that needs discussion before anyone decides: a new
protocol feature, a change to the economy, a new crate, a new kind of node.
It describes the problem and a proposed solution in enough detail for others
to find the holes. An RFC that is accepted ends in one or more
[ADRs](../adr/README.md), which record what was decided.

## When to write one

- The change spans several crates or alters the wire protocol or the chain
  rules.
- There is more than one reasonable design and the choice is not obvious.
- Operators, users or other contributors are affected and should be heard
  before the work starts.

A change that is already clear can go straight to an ADR, or to a pull
request if no decision is involved.

## Process

1. Copy the template below to `NNNN-short-title.md`, using the next free
   number, on a branch of its own.
2. Open a pull request marked as a draft and ask for comments. Diagrams and
   examples are encouraged.
3. Revise the RFC as the discussion moves. Record the alternatives that were
   rejected and why.
4. Close it in one of two ways: accepted, by merging it together with the
   ADRs it produces; or withdrawn, by merging it with its status set to
   Withdrawn and the reason, so the discussion is not lost.

## Template

```markdown
# RFC-NNNN: Title

- **Status:** Draft | Accepted | Withdrawn
- **Author:** name
- **Created:** YYYY-MM-DD

## Problem

What is wrong or missing today, for whom, and how we know.

## Proposal

The proposed solution: protocol messages, chain calls, data formats, user
experience. Diagrams welcome.

## Alternatives

Other designs considered, with their benefits and drawbacks.

## Compatibility and migration

Effect on nodes running older versions, on the chain state, and on users.

## Dependencies and licences

New libraries, models or datasets, each with its licence (only licences
allowed by ADR-0018).

## Security and privacy

What changes in the threat model.

## Open questions

What still needs an answer before a decision.
```

## Index

| RFC | Title | Status | Decided in |
|---|---|---|---|
| [0001](0001-knowledge-maps.md) | Knowledge maps | Accepted | ADR-0019, ADR-0020 |
