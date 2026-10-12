# Architecture Decision Records

An Architecture Decision Record (ADR) captures one significant decision: the
problem it answered, the options that were weighed, what was chosen and what
it costs. The code shows what Nepheral does; the ADRs keep why.

Larger proposals that need discussion before anything is decided start as an
RFC in [`docs/rfcs`](../rfcs/README.md). An accepted RFC ends in one or more
ADRs.

## When to write one

Write an ADR when a change:

- alters the wire protocol, the chain rules or any consensus-critical data;
- changes a security or privacy property, or the threat model;
- adds, replaces or removes a core dependency or a crate;
- sets a rule every contributor must follow (a process, a standard, a
  tool);
- would make a future contributor ask "why was it done this way?".

Routine changes, bug fixes and refactors that keep behaviour do not need
one.

## Process

1. Copy [`0000-template.md`](0000-template.md) to the next free number,
   `NNNN-short-title.md`, on a branch of its own. Set the status to
   **Proposed**.
2. Open a pull request. The discussion happens there: answer every
   comment, and update the record rather than the thread when an argument
   changes the decision.
3. Merge when it is approved. The status becomes **Accepted**, with the
   date of the merge.
4. When the code that implements it lands, link the pull request under
   Implementation.

An accepted ADR is not rewritten. A decision that changes gets a new ADR
that supersedes the old one; the old one only has its status updated to
**Superseded by ADR-NNNN**. A decision that simply stops applying is marked
**Deprecated**, with the reason in its history.

## What every ADR must contain

- The context: the problem, the constraints and why it matters now.
- Every option that was seriously considered, each with its benefits and
  drawbacks.
- The decision, stated in a few lines that someone can act on.
- Every new dependency (library, model or dataset) with its licence. Only
  licences compatible with the licence model are accepted
  ([ADR-0018](0018-dependency-licences.md)); a decision that needs any
  other licence is rejected.
- The consequences: what improves and what it costs, including when the
  decision should be revisited.
- Where it lives in the code, and the next steps if it is not complete.
- Links to related ADRs and references.
- A version history.

Retrospectives on accepted ADRs are welcome: add what was learned to the
history, or open a new ADR if the decision should change.

## Index

| ADR | Title | Status |
|---|---|---|
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-rust-workspace-and-crate-boundaries.md) | One Rust workspace with consensus code at the bottom | Accepted |
| [0003](0003-quic-with-mutual-tls.md) | QUIC with mutual TLS bound to node identities | Accepted |
| [0004](0004-one-lottery-per-message.md) | One lottery per message, no conversation affinity | Accepted |
| [0005](0005-shamir-custody-on-every-node.md) | Shamir-split custody on every node | Accepted |
| [0006](0006-sender-side-redaction.md) | Redact personal data on the sender | Accepted |
| [0007](0007-purpose-built-pallet-chain.md) | A purpose-built pallet chain instead of an external ledger | Accepted |
| [0008](0008-founding-validators-and-finality.md) | Founding validators, slot production and finality | Accepted |
| [0009](0009-rule-changes-halt-instead-of-fork.md) | Rule changes are scheduled on chain; old nodes halt instead of forking | Accepted |
| [0010](0010-node-key-separate-from-wallet.md) | The node key is separate from the wallet | Accepted |
| [0011](0011-no-plaintext-queues.md) | No plaintext queues on responders | Accepted |
| [0012](0012-nat-traversal-direct-first.md) | NAT traversal: direct links first, blind relays last | Accepted |
| [0013](0013-litecoin-rail-without-custody.md) | Litecoin as the payment rail, without custody | Accepted |
| [0014](0014-local-inference-with-llama-cpp.md) | Local inference with llama.cpp, backend chosen at build time | Accepted |
| [0015](0015-conversation-memory-on-device.md) | Conversation memory is decided on the device | Accepted |
| [0016](0016-light-clients-trust-the-chain.md) | Light clients trust the chain, not keys built into the app | Accepted |
| [0017](0017-business-source-license.md) | Business Source License with conversion to Apache-2.0 | Accepted |
| [0018](0018-dependency-licences.md) | Only dependencies whose licences fit the licence model | Accepted |
| [0019](0019-knowledge-maps-on-the-answering-node.md) | Knowledge maps, searched on the node that answers | Accepted |
| [0020](0020-licences-for-knowledge-map-content.md) | Licences for knowledge-map content | Accepted |
| [0021](0021-reused-contexts-and-local-prefix-cache.md) | Reused inference contexts, zeroed between replies; a prefix cache only for the owner | Accepted |

ADRs 0002 to 0016 were recorded retroactively for the first public release:
the decisions were taken while the code was written, and each record
describes them as implemented in 0.5.4.
