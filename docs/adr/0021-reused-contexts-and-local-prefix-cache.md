# ADR-0021: Reused inference contexts, zeroed between replies; a prefix cache only for the owner

- **Status:** Accepted
- **Date:** 2026-10-12
- **Deciders:** Javier Hernandez

## Context

Every reply today creates a new llama.cpp context and ends by clearing its
KV cache with `llama_memory_clear(data = true)`, which writes zeros over
every KV buffer instead of only marking the cells free. No text of one
question stays in memory for the next. The rule was never written down.

In October 2026 we compared the local backend with Ollama 0.40.3 on a
4-vCPU server, with the same model file (Qwen3 4B Instruct, Q4_K_M),
context size, threads and sampling. Writing speed was equal or better in
Nepheral; Ollama reached the first token sooner because it keeps the shared
beginning of prompts (system prompt and template) between requests. We
measured three ways of handling the context on eight questions:

| Mode | First token, with map excerpts | First token, without | Total, without |
|---|---|---|---|
| New context per reply (today) | 19.8 s | 2.8 s | 29.3 s |
| One context, zeroed after each reply | 19.5 s | 2.3 s | 28.9 s |
| One context keeping the shared prefix | 17.4 s | 1.2 s | 27.9 s |
| Ollama, for reference | 17.0 s | 1.3 s | 29.3 s |

Reusing the context and zeroing it gave identical replies on all eight
questions. Keeping the prefix gave equivalent replies, not identical word
for word. In a long conversation the shared prefix is the whole history,
so the saving per turn grows with the conversation.

## Options considered

### Option 1: a new context for every reply (today)

- Benefits: simplest; nothing survives a reply.
- Drawbacks: every reply pays to allocate and set up the context, about
  0.5 s on that server.

### Option 2: one context kept between replies, zeroed after each

- Benefits: the same privacy as today, with the setup cost paid once.
- Drawbacks: the context's memory stays allocated while the node is idle.

### Option 3: keep the shared prefix for every reply

- Benefits: the fastest first token on every path.
- Drawbacks: the KV cache would hold one person's text while the node
  serves another. The time to the first token would also reveal whether a
  new request starts like an earlier one, which lets an asker probe what
  others asked.

### Option 4: keep the shared prefix only where every request comes from the owner

The local chat and the local API, which listens only on the loopback
interface, serve the node's owner and nobody else.

- Benefits: the fastest first token, and much faster turns in long
  conversations, where nobody else's text can be involved.
- Drawbacks: the owner's conversation stays in memory between turns, so it
  needs its own rules for when it is zeroed.

## Decision

Options 2 and 4. Every inference context is kept and reused. Contexts that
serve anyone other than the owner are zeroed after every reply, as today.
The context that serves only the owner keeps the prefix its next request
shares with the previous one, and is zeroed when that stops being safe.

### Key points

- **Two separate lanes.** Mesh answers, rooms and every other request that
  is not the owner's run in contexts that are zeroed after each reply. The
  local chat and the local API run in their own context. The owner's
  context never serves anyone else, and a mesh context never keeps a
  prefix.
- **What "zeroed" means.** `llama_memory_clear(data = true)`: zeros written
  over every KV buffer. The other buffers of a context (logits and scratch
  space) are overwritten by the next request before they are read; today
  they are freed without being zeroed, so reuse does not weaken this.
- **When the owner's context is zeroed.** When the app locks, when the
  model changes or is unloaded, when the node shuts down, and after ten
  idle minutes, after which its memory is released.
- **Prefix only, never search.** The owner's context keeps the tokens the
  new request shares with the previous one from the start, and discards the
  rest. It does not look up earlier conversations.
- **Excerpts are per turn.** Map excerpts (ADR-0019) are attached to the
  turn they serve and dropped from the history afterwards, so they do not
  grow later prompts. The prefix stays valid up to that turn.
- **Settings stay.** KV cache in 16-bit floats, micro-batches of 256
  tokens, flash attention chosen by llama.cpp (`NEPHERAL_FLASH_ATTN=on|off`
  only for diagnosis). None of the alternatives measured improved on them:
  micro-batches of 512 tokens, OpenMP and a build for x86-64-v3 made no
  measurable difference, and turning flash attention off was 6 % slower.

## Dependencies and licences

| Dependency | Version | Licence |
|---|---|---|
| None | | |

## Consequences

### Positive

- About 0.5 s less per reply on every path, with the same privacy.
- In the owner's chat, the first token arrives as fast as in Ollama, and a
  long conversation no longer re-reads its whole history on every turn.

### Tradeoffs

- A kept context holds its KV buffers between replies: about 0.6 GB for a
  4B model with 4096 tokens of context, per context.
- The owner's conversation stays in memory while the chat is in use, for up
  to ten idle minutes.
- To revisit if llama.cpp gains a way to zero a single sequence, or if a
  measurement shows the saving in long conversations is smaller than
  expected.

## Implementation

### Where it lives

- `ai-engine/src/local_backend.rs`: the kept contexts, the zeroing after
  each reply, and the prefix kept in the owner's context.
- `app-kernel` and `nepheral-desktop`: zeroing the owner's context on lock,
  model change and shutdown.

### Next steps

1. Reused, zeroed contexts on every path.
2. The owner's context with its prefix, and its zeroing rules.
3. Measure the saving per turn in long local conversations.

## Related decisions

- ADR-0010: the app lock that also zeroes the owner's context.
- ADR-0011: one reserved slot per worker; each worker keeps its own
  context.
- ADR-0014: local inference with llama.cpp.
- ADR-0019: map excerpts, attached to one turn only.

## References

- llama.cpp, `llama_memory_clear`, https://github.com/ggml-org/llama.cpp

## History

- v1.0 (2026-10-12): Initial decision, after the October 2026 comparison
  with Ollama.
