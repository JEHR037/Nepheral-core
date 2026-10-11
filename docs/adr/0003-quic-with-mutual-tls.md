# ADR-0003: QUIC with mutual TLS bound to node identities

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

Every decision a node takes about a peer (whether to give it a share, whether
it won a lottery rank, whether it may claim a reward) depends on knowing who
that peer is. An identity declared inside a message can be copied by anyone.
Most nodes sit behind home routers, so the transport must also work over UDP
for hole punching, and many streams run to the same peer at once.

## Options considered

### Option 1: QUIC with TLS 1.3, mutual authentication, certificate bound to the node key

- Benefits: the TLS handshake proves the peer's identity on every
  connection; streams are multiplexed without head-of-line blocking; UDP
  works with hole punching; mature Rust implementation (`quinn`).
- Drawbacks: a custom protocol on top; UDP is blocked on some networks.

### Option 2: TCP with TLS

- Benefits: passes more firewalls.
- Drawbacks: hole punching over TCP is unreliable; no stream multiplexing
  without another layer.

### Option 3: libp2p

- Benefits: discovery, NAT traversal and transports already built.
- Drawbacks: a large dependency with its own identity and protocol
  conventions; harder to keep every message under the project's own size
  caps and signature domains.

## Decision

The mesh runs on QUIC (TLS 1.3) with mutual authentication. Each node's
certificate carries its Ed25519 node key, and the peer id is that key. Every
handler acts on the identity proven by TLS, never on a field the peer
declared.

### Key points

- **ALPN per network:** testnet and mainnet nodes cannot complete a
  handshake with each other.
- **Postcard frames with size caps:** a one-byte type followed by a postcard
  body, decoded with a hard cap and a limit per message type.
- **Domain-separated signatures:** every signed message uses its own domain
  and the chain id, so a signature cannot be replayed in another message
  type or on another network.
- **Port 443/UDP with a fallback to 4433.**

## Consequences

### Positive

- Impersonating a peer requires its private key.
- Replays across message types and networks fail by construction.
- The same UDP socket serves direct links, hole punching and relays.

### Tradeoffs

- A passive observer still sees who connects to whom, when, and how much;
  there is no traffic-analysis defence.
- The wire format is project-specific: every change must keep older peers
  working within the transition window (ADR-0009).

## Implementation

### Where it lives

- `mesh-network/src/tls.rs`: certificates from the node key.
- `mesh-network/src/quic_transport.rs`, `quic_client.rs`: endpoints.
- `mesh-network/src/protocol.rs`: message types, size caps, signature
  domains.
- `mesh-network/tests/golden_wire.rs`: frozen bytes of the core messages.

## Related decisions

- ADR-0012: how the transport reaches nodes behind NAT.

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
