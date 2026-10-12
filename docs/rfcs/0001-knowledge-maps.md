# RFC-0001: Knowledge maps

- **Status:** Accepted
- **Author:** Javier Hernandez
- **Created:** 2026-10-12
- **Decided in:** [ADR-0019](../adr/0019-knowledge-maps-on-the-answering-node.md),
  [ADR-0020](../adr/0020-licences-for-knowledge-map-content.md)

## Problem

Most nodes answer on consumer hardware, and many have no GPU they can use.
On a four-core desktop CPU (Intel i5-7600, AVX2) the largest model that
answers in well under a minute is a 4-billion-parameter one: Qwen3 4B
Instruct, quantised to Q4_K_M. Models of that size know less than the
questions people bring.

We measured it on 28 questions in Spanish across nine topics: programming,
medicine, civil engineering, mechanics, automotive, physics, mathematics,
history and journalism. Each answer was scored 0, 1 or 2, for a maximum of
56, by a single grader; differences of two points or less are noise.

| Model | Score |
|---|---|
| Qwen3 4B Instruct | 26 |
| Qwen3.5 9B | 36 |

The larger model does better but is slower than people will wait on a CPU,
and with reasoning turned on it took about twelve minutes per question.
Sending questions to an outside service would break the privacy model.
What remains is to give the node the knowledge as text it reads when it
answers: retrieval-augmented generation.

## Proposal

### What a map is

A knowledge map is a downloadable package of passages from books and
documentation under open licences, with a vector for each passage. It is
one safetensors file: the passages' text, their vectors, the source cards,
the licence texts, and the manifest in the file's metadata.

- **Passages** of 120 words, cut in reading order, so every passage has a
  previous and a next one in the same source. Passages made mostly of
  numbers or symbols (indexes, tables) are dropped.
- **Vectors** from the conversation-memory model, `nai-memory-v1` (Granite
  Embedding 107M Multilingual, [ADR-0015](../adr/0015-conversation-memory-on-device.md)):
  384 dimensions, one per passage, normalised.
- **Source cards:** one vector per source, made from its title and author,
  used to favour the right book.
- **Manifest:** the map's labels and version, the hash of the model that
  made its vectors, and every source with its title, author, licence, origin
  and attribution. The licence texts travel in the package
  ([ADR-0020](../adr/0020-licences-for-knowledge-map-content.md)).

Each map carries three labels:

| Label | Values | Meaning |
|---|---|---|
| Topic | `programming`, `medicine`, `history`… or `general` | What the map covers |
| Class | `b3`, `b9`, `b14`, `b27`, `b34` or `all` | The model size the map was built and tested for |
| Family | `qwen3`, `llama`… or none | The model family it was tuned for, if any |

The class follows the parameter count the node reads from its GGUF file:
`b3` up to 4B, `b9` from 5B to 9B, `b14` from 10B to 14B, `b27` from 15B to
27B, `b34` from 28B to 34B. A map for `all` works with any model. The labels
are in the format from the first map, so later maps can target the
weaknesses of a model size or family without a format change.

The first map is `general`, class `all`: 50 sources across the nine topics
above, 62,204 passages, 140 MB.

### Retrieval on the node that answers

The node that writes the answer is the one that searches its maps: the
person's own node in the local chat, and the responder for a question sent
to the mesh. Both already hold the question in plaintext; retrieval sends
nothing anywhere. The local API is left out: the tools that call it send
many requests in a row and bring their own context, and 20 seconds more per
request would stall them.

1. **Rewrite.** The node's own model, with reasoning off, writes an English
   translation of the question (at most 80 tokens) and a hypothetical answer
   (at most 70 words, in the style of a reference book, with the technical
   terms and names such a book would use).
2. **Embed.** The memory model turns the question, the translation and the
   hypothetical answer into vectors.
3. **Score.** Each passage scores its similarity to the hypothetical answer,
   plus a source prior: 0.2 × (the similarity of the question and the
   translation to the passage's source card, minus the best card
   similarity).
4. **Select.** The three best passages, each widened with its previous and
   next passage in the same source. Windows that touch are merged.
5. **Extract.** The windows are split into sentences. The memory model scores
   each sentence against the question and the hypothetical answer, and the
   eight best are kept word for word, in their original order, each group
   marked with the number of its excerpt.
6. **Answer.** The sentences go before the question, with an instruction to
   use them and cite them by number when they are relevant, and to ignore
   them otherwise.

A node without maps answers as it does today. The translation, the
hypothetical answer and the excerpts are derived from the question and are
handled like it: zeroed when the answer is done.

### Measured effect

The same 28 questions with Qwen3 4B Instruct, on the CPU above:

| Configuration | Score | Answer time |
|---|---|---|
| No map | 26 | ~25 s |
| Vectors of the question only | 35 | — |
| Rewrite, source prior and neighbours, full windows (~1,540 tokens) | 42 | ~62 s |
| The same with extraction (~430 tokens) | **40** | **~28 s** |
| Extraction without the hypothetical answer | 35 | — |

The rewrite costs about 18 seconds per question on that CPU (4 for the
translation, 14 for the hypothetical answer) and extraction 1 to 2 seconds,
so a question takes about 48 seconds with a map against 25 without. The
hypothetical answer is worth five points and stays. Extraction shortens the
context, which also speeds up writing, from 6.2 to 9.4 tokens per second.

On eight of the questions, against Ollama 0.40.3 with the same model file:
Ollama alone scored 2 of 16 and Nepheral with the map 8 of 16. Ollama given
the same excerpts also scored 8. The gain comes from the map, not from the
engine.

### The Maps panel

The AI section of the desktop app gets a Maps panel that:

- lists the maps for the node's class and family, falling back to `all`;
- shows each map's topic, size, sources and licences;
- downloads, verifies and removes maps;
- downloads the memory model first if it is missing, since retrieval needs
  it;
- flags installed maps of another class after the model changes.

Headless nodes get the same actions from the command line. Each map is one
file in the node's data directory, under `maps/`.

### Distribution

- The catalog of maps is built into each release, like the model catalog:
  identifier, labels, size, SHA-256 and download addresses. A map is used
  only if its files match the pinned hash.
- Maps are served from the project's download servers, like models.
- A new map, or a new version of one, ships with a release.

### Building maps

A builder in the repository takes a list of sources (text, title, author,
licence, origin) and writes a map: passages, vectors, cards, manifest and
licence texts. It is public, so anyone can rebuild a published map from the
sources its manifest lists.

### Who answers

The lottery does not prefer nodes that hold maps, and nodes do not announce
which maps they hold. With few nodes, a preference would pile questions on a
handful of machines, and announcing maps would tell every peer what each
node carries. This is to be revisited when the network is large.

## Alternatives

- **A larger model.** Better without a map (36 for the 9B model) but slower
  than people will wait on a CPU, and out of reach for most nodes.
- **Vectors of the question alone.** 35 against 40. The questions are in
  Spanish and most sources in English, and a question rarely uses the words
  of the book that answers it.
- **Keyword search (BM25), alone or fused with the vectors.** Tried in the
  retrieval bench and left out: it did not improve on the source prior, and
  its index makes the map about 40 MB larger (175 MB against 133 MB).
- **A small model that compresses the excerpts** (Qwen3 0.6B). 29 points in
  about the same time. In 14 of 28 cases it kept nothing, and when it
  paraphrased it invented facts.
- **Speculative decoding** to write faster. With a 0.6B draft model it was
  twice as slow on four cores (30 % of drafts accepted); with n-grams it
  gained nothing (12 % accepted).
- **Outside services or web search.** The question would leave the node in a
  form a third party can read.
- **Retrieval on the asker's device.** The asker would search and send
  excerpts with the question. Phones and small nodes cannot run the rewrite,
  every message would grow, and the asker would have to hold every map.

## Compatibility and migration

- No change to the wire protocol or the chain. Nodes with and without maps
  answer the same messages.
- On a CPU, an answer with a map takes about 48 seconds instead of 25,
  within the 120 seconds an asker waits.
- The class comes from the parameter count read from the GGUF file since
  0.5.4.

## Dependencies and licences

- No new library or model. Retrieval uses `nai-memory-v1` (Granite
  Embedding 107M Multilingual, Apache-2.0), already distributed for the
  conversation memory.
- Map content is distributed as separate works under their own licences.
  [ADR-0020](../adr/0020-licences-for-knowledge-map-content.md) sets which
  licences are accepted, as an addition to
  [ADR-0018](../adr/0018-dependency-licences.md).
- The first map's 50 sources: 37 in the public domain (Project Gutenberg
  texts and US federal works), 4 under CC BY 4.0, 1 under CC BY 2.5 Canada,
  2 under MIT OR Apache-2.0, 1 under PSF-2.0, 2 under the GFDL, 2 published
  as public information by the US Federal Highway Administration, and 1 US
  federal publication prepared by a contractor (see the open questions).

## Security and privacy

- Retrieval runs where the question already is in plaintext. Nothing new
  leaves the node, and there is no request per question.
- The rewrite and the excerpts are zeroed with the rest of the prompt when
  the answer is done.
- A map is used only when its files match the hash pinned in the release, so
  a download server cannot slip in a different map.
- Excerpts are untrusted text inside the prompt. The sources are books and
  official documentation, the instruction presents them as quoted excerpts,
  and extraction keeps sentences word for word, which limits injected
  instructions to what a source itself contains.
- Downloading a map tells the download server which topic a node wants. The
  first map covers every topic in one package, so it reveals nothing about
  interests.

## Open questions

1. **Class boundaries.** The ranges above are provisional. They become fixed
   when the first map for a specific class ships.
2. **Public domain by country.** Project Gutenberg texts are in the public
   domain in the United States, but a few are not yet everywhere: Einstein's
   *Relativity* stays protected in Spain until 2036, and longer where
   copyright lasts 100 years after the author's death. The status of the
   contractor-prepared federal publication also needs checking. These are
   to be settled before the first map is published.
3. **Maps per topic.** The first map ships whole. Packages per topic, of 7 to
   10 MB with vectors stored as 8-bit integers, wait until 8-bit vectors are
   measured on the same questions.
4. **Maps between releases.** A signed catalog fetched at run time would let
   maps ship without a release.
5. **Preference in the lottery** for nodes with maps, once the network is
   large.
