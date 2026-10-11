# ADR-0010: The node key is separate from the wallet

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

A node signs every few seconds: greetings, lottery claims, answers,
receipts, its own redeems. Its key must open without anyone typing a
password, or the node cannot serve while its owner is away. A key that opens
unattended is exposed to any malware running as the same user. If that key
also held the funds, an infostealer could empty the wallet.

## Options considered

### Option 1: one key, sealed by the operating system

- Benefits: simplest; the node can spend what it earns.
- Drawbacks: whoever can use the node key can move the funds.

### Option 2: one key behind a password

- Benefits: funds protected at rest.
- Drawbacks: the node stops serving whenever nobody is there to unlock it.

### Option 3: two keys linked on chain

A node key that opens unattended and holds nothing, and a wallet key behind
a password that holds the funds.

- Benefits: the node serves unattended; the funds need the password.
- Drawbacks: two keys to explain to users; an on-chain link to maintain.

## Decision

Two keys do two jobs, linked by the Identity pallet:

- **The node key** is the network identity (peer id and TLS certificate).
  It opens without a password. On Windows it is sealed with DPAPI and, where
  the computer has a TPM, also with a key that lives in the TPM; a copy kept
  in the vault restores it if the TPM is cleared. Elsewhere it is
  owner-only. It holds no funds.
- **The wallet key** comes from the 24-word recovery phrase and owns the
  balance. It lives in a password vault (Argon2id, then XChaCha20-Poly1305)
  and is in memory only while unlocked, by hand, until locked again or
  after a stretch of inactivity.

### Key points

- **Earnings flow to the wallet:** a delegated node never holds credits;
  whatever lands on it moves to its wallet.
- **Locked still serves:** chats, payments, trades and votes wait for the
  unlock, and each payment, trade or vote is confirmed again.
- **Same phrase, same wallet** on every machine; each machine's node joins it
  with a key of its own.

## Consequences

### Positive

- Malware that steals the node key cannot spend the funds.
- Nodes keep serving and earning while their owners are away.

### Tradeoffs

- A user can lose access to the funds by forgetting the password and the
  phrase.
- Malware running while the wallet is unlocked, or a keylogger, can still
  take it.

## Implementation

### Where it lives

- `core-crypto/src/pallets/identity.rs`: `Delegate`, `Revoke`.
- `app-kernel/src/wallet.rs`, `vault.rs`, `control.rs`.
- `nepheral-desktop/src/node_store.rs`, `tpm.rs`, `lock.rs`.

## Related decisions

- ADR-0003: the node key is also the transport identity.

## References

- `docs/WHITEPAPER.md`, section 8.
- `SECURITY.md`, known limitations.

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
