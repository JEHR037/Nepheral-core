# ADR-0012: NAT traversal: direct links first, blind relays last

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

Most nodes run at home or on phones, behind routers that drop packets nobody
inside asked for. Two such nodes cannot dial each other. Early on, a node
learned about another only from a direct connection, so nodes behind NAT
never saw each other, and custody fell entirely on the few public servers.

## Options considered

### Option 1: relay everything through public servers

- Benefits: always works.
- Drawbacks: the servers carry every byte and become a central point of
  failure and observation.

### Option 2: direct links wherever possible, relays only as a fallback

- Benefits: most traffic goes peer to peer; relays carry only what cannot.
- Drawbacks: several mechanisms to build and test.

## Decision

Nodes try, in order: a LAN link behind the same router, a router port
mapping, a hole punch, and only then a relay.

### Key points

- **Presence:** every node passes on the newest signed `HELLO` of every node
  it has heard of. A `HELLO` verifies under its author's key wherever it
  travels, so nodes behind NAT learn about each other without connecting.
- **Port mapping:** a node asks its router to forward the mesh port with
  UPnP-IGD, NAT-PMP or PCP. With one, it is reachable like a server.
- **Hole punching:** a public node that sees both peers tells each where the
  other is, and both dial at once.
- **Blind relays:** a frame for a node out of reach is sealed to that node,
  so the relay learns who it is for and how big it is, not what it is. Any
  node that strangers can reach relays, not only the project's servers;
  each frame goes through the two closest relays, with a bounded number of
  hops.

## Consequences

### Positive

- Custody and lottery reach every node, wherever it runs.
- Relays see neither the content nor the kind of message.

### Tradeoffs

- Symmetric NAT and carrier-grade NAT still need relays.
- A relay still learns sizes, timing and endpoints.

## Implementation

### Where it lives

- `app-kernel/src/presence.rs`, `portmap.rs`, `punch.rs`, `relay.rs`.

## Related decisions

- ADR-0003: the UDP transport that makes punching possible.
- ADR-0005: custody that depends on reaching every node.

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
