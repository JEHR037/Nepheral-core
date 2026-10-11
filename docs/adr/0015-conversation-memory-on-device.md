# ADR-0015: Conversation memory is decided on the device

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

Each question goes to a stranger (ADR-0004), so every earlier message sent
with it is exposed again. Sending the whole conversation every time leaks
everything; sending nothing loses the thread. Something has to choose which
earlier messages are relevant, and that choice must not be made by another
node, which would then need to read the conversation.

## Options considered

### Option 1: always send the last few messages

- Benefits: simple and predictable.
- Drawbacks: sends irrelevant, possibly private messages; misses relevant
  ones further back.

### Option 2: let the responder summarise and keep the history

- Benefits: the most context for the model.
- Drawbacks: a stranger keeps the conversation.

### Option 3: a small embedding model on the device chooses

- Benefits: the choice is made where the conversation already is; works in
  any language.
- Drawbacks: a model file to download and run on every device; a little
  CPU per message.

## Decision

The device keeps the conversation and decides what each question carries.
A small multilingual embedding model, Granite Embedding 107M Multilingual,
packed with its word embeddings in 8-bit integers and run in process with
`candle`, turns every message into a vector. Earlier messages related in
meaning to the new question travel with it; the rest stay on the device.

### Key points

- **Ask before sending something sensitive:** an earlier message that looks
  sensitive (health, money, legal status, credentials) is shown to the
  person for confirmation before it is sent. The confirmation can be paused
  for a chosen time.
- **A name is not enough** to trigger the confirmation: only the person's
  own messages are considered, and everyday words are not flagged.
- **Lexical fallback:** until the model is on the device, a lexical measure
  takes its place.

## Consequences

### Positive

- Less of each conversation reaches responders.
- No other node needs to read the conversation to choose its context.

### Tradeoffs

- Another model to download and keep up to date.
- The sensitivity check is a heuristic: it can ask when it need not, or
  miss something written in an unexpected way.

## Implementation

### Where it lives

- `recall/src/embed.rs`, `xlmr.rs`, `select.rs`, `memory.rs`,
  `exemplars.rs`.
- `app-kernel/src/conversations.rs`: storage and the confirmation rules.
- `recall/examples/pack.rs`: how the model file is made.

## Related decisions

- ADR-0004: why every message goes to a different responder.
- ADR-0006: what is removed from each message before it leaves.

## References

- `docs/CONVERSATIONS.md`.
- `legal/nai-memory-v1.NOTICE.txt`: the model's licence (Apache-2.0).

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
