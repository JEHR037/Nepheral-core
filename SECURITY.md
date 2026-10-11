# Security Policy

Nepheral is pre-audit software handling real value (on-chain credits and a
Litecoin payment rail). Treat any report seriously and report
responsibly: do not exploit a vulnerability beyond the minimum needed to
demonstrate it, and do not access, modify, or exfiltrate data that isn't
yours.

## Reporting a vulnerability

Report it privately through GitHub: **Security** tab, then **Report a
vulnerability**. If you cannot use GitHub, email **security@nepheral.net**.
Include:

- A description of the issue and its impact (what an attacker could do,
  and against whom — a specific peer, the authority, the mesh as a whole).
- Steps to reproduce, ideally against a local multi-node setup rather than
  the live mesh.
- Whether you believe it is being actively exploited.

You will get an acknowledgement within 5 business days. Please give us a
reasonable window to ship a fix before any public disclosure — 90 days is
our default target, but we will communicate if a fix needs more or less
time, and we will credit you in the fix's changelog entry unless you ask
us not to.

**Do not open a public GitHub issue for a security report.**

## Scope

In scope: the crates in this workspace (`core-crypto`, `mesh-network`,
`app-kernel`, `ai-engine`, `nepheral-api`, `nepheral-desktop`, `recall`,
`utils`), the published release binaries, and the Android app and the
payment service, which are not in this repository but are reported the
same way. Out of scope: third-party dependencies (report those upstream),
and social-engineering or physical attacks against the team.

## Known, already-declared limitations

These are documented trade-offs, not open reports — see `README.md` and
`docs/WHITEPAPER.md` for the full threat model:

- A network observer can correlate who sends and who responds; there is no
  traffic-analysis defense.
- The lottery winner reads the redacted query it wins. Redaction is
  pattern-based; free-form personal data is only removed when listed in
  `redact_terms`.
- Recipient-side wallet compromise decrypts past captured bundles addressed
  to that wallet.
- A node runs with a key of its own, which must open unattended and so is
  not behind a password: sealed with DPAPI on Windows (`~/.nepheral/node.bin`,
  with the local system prompt), owner-only (0600) elsewhere and on headless
  nodes (`identity.mnemonic`). Malware running as the same user can use it.
  It holds no funds: what the node earns moves to its wallet (pallet 4).
- The wallet - the 24-word phrase and the local API token - is in
  `~/.nepheral/vault.bin` (Argon2id, 64 MiB, 3 passes, then
  XChaCha20-Poly1305) and in memory only while unlocked. Malware running
  while it is unlocked, or a keylogger, can take it.
- The local API answers while locked to whoever has its token; config.json
  holds only the token's hash.
- Wiping the plain copies an older version left (on migration) overwrites
  them first, which an SSD's wear levelling may not honour.
- The desktop writes no log file unless `NEPHERAL_LOG_FILE` is set.
- Finality tolerates faults only from 4 founding nodes up (3f+1). With
  fewer, an offline validator delays finality; block production carries
  on and reorganisations stay capped at 64 blocks.
- Answer ratings are public on chain, including the dislike note.
- Rating rings are limited, not eliminated: receipts are capped per pair
  and per sender, and each rater counts once per responder per day.

If your report is about one of these and you have a concrete new attack
building on them (not just re-stating the limitation), we still want to
hear about it.

## Key-bake procedure (for operators building from source)

The genesis authority (founder 0) is compiled in. By default it is the
constant in `core-crypto/src/chain.rs`; a build can override it with the
`NEPHERAL_TEAM_PUBKEY` environment variable, which changes the genesis
and therefore creates a separate chain. To build a node that can
validate blocks signed by the current authority, compile with:

```sh
NEPHERAL_TEAM_PUBKEY=<64-hex-chars> cargo build --release -p app-kernel --features node_only
```

Use the current authority's public key (not a key you generate yourself,
unless you are standing up your own independent chain for testing). If you
are the Licensor rotating the authority key, generate a fresh keypair with
`cargo run --example new_authority -p core-crypto`, keep the resulting
seed off any machine other than the signing node, and update this
document's reference key material out of band (never commit the private
key or seed to this repository).

If a node's local `chain.bin` was seeded before a key rotation, replay
will halt before reaching the end of the file. Stop the node, delete
`~/.nepheral/<network>/chain.bin`, and restart to re-sync from the
current genesis — see the
warning this node's logs point you back to.
