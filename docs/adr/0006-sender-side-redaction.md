# ADR-0006: Redact personal data on the sender

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

The responder reads the message it answers (ADR-0004). Encryption protects
the message on its way, not from the node that runs the inference. People
write emails, phone numbers, account numbers and names into their questions
without thinking about who reads them.

## Options considered

### Option 1: trust the responder

- Benefits: the model sees exactly what the person wrote.
- Drawbacks: every personal detail reaches a stranger.

### Option 2: redact on the sender and restore on the sender

Replace personal data with stable placeholders before anything leaves the
machine, and put the real values back in the answer locally.

- Benefits: the responder never sees the values; the model can still reason
  about them because the same value always maps to the same placeholder.
- Drawbacks: detection is pattern-based; data written in free form is only
  caught if the person lists it.

### Option 3: run inference only inside hardware enclaves

- Benefits: the responder's operator cannot read the prompt.
- Drawbacks: no consumer hardware supports it today; it would exclude almost
  every node.

## Decision

Each conversation has a `Redactor` on the sender. It replaces emails, phone
numbers, IBANs, card numbers, IP addresses, UUIDs, secrets, user paths,
URLs with credentials and user-configured terms (`redact_terms`) with
placeholders such as `[[EMAIL_1]]`, and restores them in the answer before it
is shown. Responders are told to keep the placeholders as they are only when
a message carries some.

### Key points

- **Stable within a conversation:** the same value always becomes the same
  placeholder.
- **Nothing to configure for the common cases;** `redact_terms` covers the
  rest.

## Consequences

### Positive

- The values never leave the machine.
- Works on every node, with any model.

### Tradeoffs

- Free-form personal data (a street address written in prose) passes unless
  configured.
- A model can mishandle placeholders; the instruction is added only when
  they are present so it does not invent its own.
- Hardware-isolated inference remains the long-term complement.

## Implementation

### Where it lives

- `ai-engine/src/redaction.rs`: patterns, placeholders and restoration.
- `app-kernel/src/agent.rs`: the instruction about placeholders.

## Related decisions

- ADR-0004: why the responder reads the message.

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
