# ADR-0019: Knowledge maps, searched on the node that answers

- **Status:** Accepted
- **Date:** 2026-10-12
- **Deciders:** Javier Hernandez

## Context

The models most nodes can run on a CPU, around 4 billion parameters, know
too little: Qwen3 4B Instruct scored 26 of 56 on a 28-question test across
nine topics. Larger models are slower than people will wait on a CPU, and
outside services would break the privacy model.
[RFC-0001](../rfcs/0001-knowledge-maps.md) proposes knowledge maps:
packages of passages from openly licensed books and documentation, with
vectors, that a node searches before it answers. With them the same model
scored 40.

The decision is where retrieval runs, how a map is described and how maps
reach nodes.

## Options considered

### Option 1: no retrieval; rely on larger models

- Benefits: nothing to build or distribute.
- Drawbacks: most nodes cannot run larger models fast enough, so quality
  stays where it is.

### Option 2: retrieval on the asker's device

The asker searches its maps and sends the excerpts with the question.

- Benefits: the asker chooses the knowledge.
- Drawbacks: phones and small nodes cannot run the rewrite that makes
  retrieval work; every message grows; the asker must hold every map.

### Option 3: retrieval on the node that answers

The node that writes the answer searches the maps it holds: the person's
own node for the local chat, the responder for the mesh.

- Benefits: the node that holds the question in plaintext is the only one
  that touches it; nothing changes in the protocol; askers need nothing.
- Drawbacks: the answer depends on which maps the responder happens to
  hold; on a CPU each answer takes about 20 seconds longer.

### Option 4: outside services or web search

- Benefits: unlimited, current knowledge.
- Drawbacks: the question leaves the node in a form a third party can read.

## Decision

Option 3. A node searches the maps it holds before it writes an answer,
with the method measured in RFC-0001; a node without maps answers as before.
Maps are installed by their owners from a Maps panel and pinned by hash in
each release. The lottery does not prefer nodes with maps.

### Key points

- **Format.** A map is one safetensors file: 120-word passages in reading
  order, one vector per passage from `nai-memory-v1`, one vector per source
  made from its title and author, the licence texts of its sources, and a
  manifest in the metadata. The manifest records the hash of the model that
  made the vectors; a map made with another model is not used.
- **Where it runs.** The local chat and answers for the mesh. The local API
  is left out: the tools that call it send many requests in a row and bring
  their own context, and the rewrite would add about 20 seconds to each.
- **Labels.** Every map carries a topic, a class (`b3`, `b9`, `b14`, `b27`,
  `b34` or `all`) and an optional model family. The node takes its class
  from the parameter count in its GGUF file. The class boundaries are
  provisional until the first class-specific map ships.
- **Retrieval.** The node's model, with reasoning off, writes an English
  translation of the question and a hypothetical answer of at most 70 words.
  Passages are scored against the hypothetical answer, plus 0.2 times the
  similarity of the question and translation to the passage's source card,
  relative to the best card. The three best passages are widened with their
  neighbours, and the eight sentences that best match the question and the
  hypothetical answer are kept word for word.
- **Prompt.** The sentences go before the question, numbered, with an
  instruction to use and cite them when relevant and to ignore them
  otherwise. They are attached to that turn only and are not kept in a
  conversation's history.
- **Privacy.** The translation, the hypothetical answer and the excerpts are
  zeroed when the answer is done, like the prompt.
- **Distribution.** The map catalog is built into each release with the
  SHA-256 of every file; maps are downloaded from the project's servers and
  checked against those hashes before use. New maps ship with releases.
- **Panel.** The AI section of the desktop app lists the maps for the node's
  class and family (falling back to `all`), shows their sources and
  licences, and downloads, verifies and removes them. Headless nodes do the
  same from the command line.
- **First map.** `general`, class `all`: the 50 sources of the test,
  62,204 passages and 140 MB, shipped whole.
- **No announcement.** Nodes do not tell peers which maps they hold, and the
  lottery ignores maps.

## Dependencies and licences

| Dependency | Version | Licence |
|---|---|---|
| None new. `nai-memory-v1` (Granite Embedding 107M Multilingual) is already distributed for the conversation memory. | v1 | Apache-2.0 |

The licences of map content are set by
[ADR-0020](0020-licences-for-knowledge-map-content.md).

## Consequences

### Positive

- On the test, a 4B model goes from 26 to 40 of 56, with no change to the
  protocol or the chain.
- People choose the knowledge their node carries.
- Extraction keeps the prompt short (about 430 tokens of excerpts), which
  keeps writing fast.

### Tradeoffs

- About 20 seconds more per answer on a four-core CPU: 18 for the rewrite,
  1 to 2 for extraction. Within the 120 seconds an asker waits.
- Disk: 140 MB for the first map plus 128 MB for the memory model.
- Memory: a map is mapped from its file, not read into memory; while it is
  searched, its 96 MB of vectors and 44 MB of text sit in the page cache.
- The quality of an answer from the mesh depends on the responder's maps,
  which the asker does not choose.
- To revisit when 8-bit vectors are measured (maps per topic of 7 to 10 MB),
  and when the network is large enough for the lottery to prefer map
  holders.

## Implementation

### Where it lives

- `recall/src/map.rs`: the map format, loading, search and sentence
  extraction; `recall/examples/build_map.rs`: the public map builder.
- `app-kernel/src/maps.rs`: the built-in map catalog, downloads and
  verification, and retrieval; `recall_model.rs`: the memory model, which
  headless nodes now carry too (the `knowledge-maps` feature).
- `app-kernel/src/agent.rs`: retrieval before an answer is written, in the
  mesh path (`serve_collected`) and the local chat.
- `nepheral-desktop/ui`: the Maps card on the AI page;
  `nepheral-node maps`: the same on headless nodes.

### Next steps

1. The map format and search in `recall`, with tests on a small map.
2. The map builder, and the first map rebuilt with it.
3. Retrieval in the three answering paths.
4. The catalog, downloads and the Maps panel.
5. A check that the product reproduces about 40 of 56 on the test
   questions.

## Related decisions

- ADR-0015: the memory model reused for map vectors.
- ADR-0018 and ADR-0020: licences of what the project distributes.
- ADR-0021: how inference contexts are reused; why excerpts are not kept in
  the local history.

## References

- [RFC-0001](../rfcs/0001-knowledge-maps.md): problem, measurements and
  alternatives.
- Gao et al., *Precise Zero-Shot Dense Retrieval without Relevance Labels*
  (HyDE), 2022, https://arxiv.org/abs/2212.10496

## History

- v1.0 (2026-10-12): Initial decision.
