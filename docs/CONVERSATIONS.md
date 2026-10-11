# Conversations: memory, storage and privacy

Design approved by the founder on 2026-10-04.
Ships in a 0.5.x release.

## Why

A question carries the last 4 messages (`CONTEXT_WINDOW_MESSAGES`) plus a
running summary that whichever node answers rewrites (`want_memory`). The
servers run 0.5B–3B models, which wreck that summary: a 20-question test on
2026-10-04 ended with a summary that mixed the Moon landing with breathing
oxygen. It also means every responder writes, and so reads, a digest of the
whole conversation.

## Principles

- **The device decides** what context leaves it. Nodes never ask for more and
  never write memory.
- **Summaries are computed on the device**, never by the network.
- What leaves is what the rules allow and, past a threshold, **what the
  person approved**.

## Embeddings

- A small sentence-embedding model, **seriously multilingual** (Spanish and
  English first, the other major languages too; not an English model that
  tolerates Spanish), run in-process in Rust, the same code on desktop and
  phone.
- Before choosing: measure two or three candidates for size, speed on a
  phone CPU, and retrieval quality in Spanish and English.
- Delivered with a hash check, and updated through the same auto-update
  channels as the apps.
- Each message is embedded once, when it is stored; the vector is kept with
  it.

## What each question carries

- **The last 2 messages**, always: the immediate thread.
- **Older messages chosen by similarity** to the question (cosine), above a
  threshold, diversified with MMR so one topic is not sent twice, within a
  fixed size cap.
- **A request for a summary** ("summarise what we talked about", in any
  language) is recognised by similarity to example phrases. The app then
  clusters the conversation by topic and takes the most representative
  message of each: an **extractive digest**, computed on the device with no
  language model. The node that answers writes the prose from it.
- On a desktop with a local model, the digest can be turned into prose
  locally instead.
- Everything passes through the existing redactor (`ai-engine/src/redaction.rs`);
  placeholders are put back only on the device.
- `want_memory = false`: responders stop writing memory.

No protocol change: `AgentQuery` v2 already carries up to 8 turns and a free
`memory` string, and nodes on 0.5.2 accept both. The device just fills them
differently.

## Sending rules

1. **Private messages never leave the device.**
2. **Confirmation** is required when the older context (everything but the
   last 2 messages) exceeds **30% of the conversation's text**, counted in
   words, not messages; and whenever one of the person's own messages among
   the fragments touches a sensitive topic (health, money, identity),
   however small. The confirmation lists the fragments, and the person can
   remove any of them.
3. Otherwise the context goes without asking.

A summary request nearly always passes 30%, so summaries are always
confirmed.

**What counts as sensitive** (since 0.5.4, after a tester's week of
cards that came up for nothing): only the person's own messages; NAI's
answers repeat topics and used to set off nearly every question. With the
model on the device, its verdict alone decides, by meaning and in any
language; a short list of first-person phrases ("me diagnosticaron", "my
password") stands in only until the model is downloaded. Everyday words no
longer count: "sin embargo", "deuda técnica", "diagnosticar un error".

**Not asking for a while.** The card offers "don't ask again for" 1 hour,
8 hours, 1 day or 1 week. Until then questions take their context without
the card; Settings shows until when, with "Ask again"
(`confirm_snooze_until_ms` in the config, command `chat_snooze`).

## Private messages

Any message can be marked private in every version (desktop, phone, and the
local API). A private message is stored with its flag, never selected as
context, and never sent.

## Storage

- Conversations, with the local model, through the mesh, and on the phone,
  can be listed, renamed, deleted and resumed.
- **Persisted on the device, sealed with the wallet key**, behind a setting
  that can be switched off (off: memory only, as today). A locked wallet
  means unreadable conversations; nobody else can open them.
- **Rooms are never persisted: RAM only, always.** A test keeps it that way.

## Naming

The assistant is mentioned as **`@NAI`** in rooms and named so throughout the
UI, in every language. `@agente` and `@nepheral` are still recognised, since
members on older versions type them.

## How it is built (0.5.3)

Crate `recall` (rules, embeddings, conversation side) and
`app-kernel/src/conversations.rs` (model download, context per question,
sealed store). Commands: `chat_send` (with `private`), `chat_confirm`,
`chat_cancel`, `chat_rename`, `chat_private`, `memory_view`,
`memory_download`, `memory_keep`; `chat_thread` carries `pending`, the
context waiting for confirmation.

**The model.** `recall/examples/bench` compared four multilingual
candidates on conversations in Spanish, English, Portuguese, French and
German, with questions asked across languages (Spanish, Portuguese, French,
German, Russian and Chinese questions about English messages) and a
34-message conversation of close topics (three recipes, two Rust errors,
three cities):

| model | size | ms/message (CPU) | MRR | one threshold, related vs. not |
|---|---|---|---|---|
| multilingual-e5-small | 471 MB | 51 | 0.99–1.00 | 0.93–0.95 |
| paraphrase-multilingual-MiniLM-L12 | 471 MB | 47 | 0.96 | 0.955 |
| **Granite Embedding 107M Multilingual** | **214 MB** | **25** | 0.97 | **0.963** |
| potion-multilingual-128M (static) | 512 MB | 0.2 | 0.96 | 0.957 |

Granite (IBM, Apache 2.0) won on speed, size and on how cleanly one
threshold separates related from unrelated messages, and detects sensitive
topics best (0.91 against 0.73–0.86). Above 500 MB nothing promised enough
for choosing among a few dozen messages.

- Run by `recall/src/xlmr.rs`, an XLM-RoBERTa encoder that reads the
  vocabulary (250 000 rows, most of the model) row by row from the mapped
  file: about 45 MB of memory instead of 430. It gives candle's own
  XLM-RoBERTa's vectors (cosine ≥ 0.999999).
- Shipped as one file, `nai-memory-v1.safetensors` (128 MB): the vocabulary
  in 8 bits per number with one scale per row, the tokenizer and
  configuration inside. Same results as the original (cosine ≥ 0.99999).
  Served from `dl.nepheral.net/models/` and pinned by SHA-256 and size in
  the app (`RECALL_SHA256`); a file that does not match is deleted.
- The desktop downloads it on its own; the phone asks first (it may be on
  mobile data). The UI calls it **NAI's private memory**.
- Until it is there, or if it cannot load, a lexical measure (shared words,
  accents and case folded) chooses instead, under the same rules.

**Thresholds** (Granite, measured): related from 0.65; from 0.57 when the
question refers back ("me dijiste", "lo del…", "again", "encore", a Spanish
imperfect such as "¿qué llevaba?"); a summary request from 0.77; a sensitive
passage from 0.67. Measured on 2026-10-05: everyday sentences 0.53-0.63
("sin embargo, el código no compila", a dog's rabies shot, a day in Lisbon),
private ones 0.73-0.89 (a diagnosis, debts, an ID number, a pregnancy).
Someone's full name sits in between (0.655-0.670) and a first name alone
just below (0.61-0.63): too close to tell apart reliably, so names do not
count either way. Long messages are embedded in passages
of 120 words, and the passage closest to the question is what is sent of
them. A summary takes up to 16 excerpts of 40 words, one per topic.

End to end on the bench (39 questions, 11 unrelated ones): the related
context was carried for 39 of 39, and none of the unrelated questions
carried anything.

**Local conversations** use the same choice with more room (the last 6
messages, up to 900 words of older ones) and never ask: nothing leaves the
device. They answer with `infer_stateless`, so a conversation resumed after
a restart keeps its context.

**The store.** `~/.nepheral/conversations/<account>.sealed`: `NEPHCNV1`,
a nonce, and XChaCha20-Poly1305 over the conversations (with their vectors),
under `SHA-256("nepheral/conversations/v1\0" ‖ wallet secret)`. Read once
when the wallet is first unlocked, written a couple of seconds after any
change and when the wallet locks. `keep_conversations = false` deletes it.

## Delivery

Through the auto-update channels: the Windows feed, Linux AppImages,
Google Play, and an updater for the APK served from the website.
