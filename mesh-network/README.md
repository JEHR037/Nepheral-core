# mesh-network

QUIC transport, mutual-TLS peer authentication, the wire protocol (v3) and
end-to-end encrypted rooms for Nepheral. Message dispatch lives in
`app-kernel/src/node_runtime.rs`; this crate defines the transport and the
message formats.

Modules: `tls.rs`, `quic_transport.rs`, `quic_client.rs`, `protocol.rs`,
`rooms.rs`. Integration tests: `tests/dual_bind.rs`.

---

## Peer authentication (`tls.rs`)

Every connection is **mutual TLS 1.3** with Ed25519 only. A node's
certificate is the self-signed certificate from
`core_crypto::NodeIdentity`, and its peer id **is the Ed25519 public key
inside that certificate**. There is no CA, no trust-on-first-use pinning
and no "accept any certificate" mode: identity is proven by the TLS
handshake signature itself.

| Item | Description |
|---|---|
| `TlsIdentity { cert_der, key_der }` | This node's certificate and PKCS#8 key |
| `server_config(&identity)` | rustls server config that **requires** a client certificate |
| `client_config(&identity, expected: Option<PeerId>)` | rustls client config that presents this node's certificate; with `expected` set, the handshake fails unless the server's key matches |
| `peer_id_from_cert_der(der) -> Option<PeerId>` | Extracts the single Ed25519 key from a certificate (rejects certificates carrying zero or several) |
| `peer_id_of(&quinn::Connection) -> Option<PeerId>` | The authenticated peer id of a live connection |

The verifier rejects certificate chains (certificates must be
self-standing), TLS 1.2, and any signature scheme other than Ed25519. The
ALPN is `core_crypto::network::active().alpn()` (`nepheral/3/<network>`),
so testnet and mainnet nodes cannot connect to each other.

## Server (`quic_transport.rs`)

```rust
pub struct EndpointConfig {
    pub bind_addr: SocketAddr,             // e.g. 0.0.0.0:443
    pub bind_addr_v6: Option<SocketAddr>,  // e.g. [::]:443
    pub identity: TlsIdentity,
    pub max_concurrent_streams: u64,
}
```

| `FragmentEndpoint` method | Description |
|---|---|
| `new(config)` | Binds the QUIC listener(s) with the mutual-TLS server config (10 s keep-alive, 30 s idle timeout) |
| `endpoint_v4()` / `endpoint_v6()` | The underlying quinn endpoints, reused for outbound dials |
| `accept()` | Next incoming connection from either family; a closed IPv6 listener is latched off so IPv4 keeps serving |
| `shutdown()` | Closes both endpoints |

`bind_addr_v6` opens a second, `IPV6_V6ONLY` socket rather than one
dual-stack socket, so IPv4 peers never appear as IPv4-mapped addresses and
split into two `SocketAddr` keys. A failed IPv6 bind is logged and the node
runs IPv4-only.

The port is chosen by `app-kernel` (`boot.rs`): 443 by default, falling back
to 4433 if 443 cannot be bound.

## Client (`quic_client.rs`)

| `MeshClient` item | Description |
|---|---|
| `from_endpoint(endpoint, identity)` | Dials from an existing (listening) endpoint, so outbound traffic leaves from the node's own port |
| `ephemeral(target, identity)` | Dials from a fresh `0.0.0.0:0` / `[::]:0` endpoint |
| `connect_to_peer(addr, expected) -> (Connection, PeerId)` | Connects with mutual TLS (timeout `CONNECT_TIMEOUT`, 10 s) and returns the authenticated peer id |
| `endpoint()` | Accessor |

## Wire protocol (`protocol.rs`)

`PROTOCOL_VERSION` = 3. Each message is one bidirectional QUIC stream,
framed by `frame(msg_type, payload)` as `[1B type][4B big-endian len][payload]`.
Struct payloads are `postcard`-encoded with `core_crypto::wire`; `PeerList`
and `GossipPeer` use a fixed big-endian layout. All signatures are taken
over `core_crypto::network::signing_digest` with a per-message domain, so
they are bound to the active network's chain id.

### Message types (`message_types`)

| Constant | Hex | Payload | Purpose |
|---|---|---|---|
| `HELLO` | `0x05` | `Hello` | Signed handshake: version, peer id, listen port, capability bits, model info, own addresses, observed address |
| `PEER_LIST` | `0x10` | `PeerList` | List of dialable peers (reply to `PEER_LIST_REQUEST`, or pushed) |
| `GOSSIP_PEER` | `0x11` | `GossipPeer` | One peer address with a quality score (0-100) |
| `PEER_LIST_REQUEST` | `0x12` | none | Asks for a `PEER_LIST` on the same stream |
| `ROOM_ENVELOPE` | `0x24` | `RoomEnvelope` | Encrypted, signed room traffic, flooded with a TTL |
| `NODE_STATS` | `0x41` | node stats | A peer's advertised statistics |
| `MODEL_ANNOUNCE` | `0x43` | 32B hash + 8B size | A model file the peer can serve |
| `CHAIN_TX` | `0x44` | `Extrinsic` | Signed extrinsic, relayed toward the block producer |
| `BLOCK_BROADCAST` | `0x4A` | `Block` | A newly sealed block |
| `BLOCK_SYNC_REQUEST` | `0x4B` | 8B from + 8B to | Requests a height range; blocks are written back on the same stream |
| `FINALITY_VOTE` | `0x4C` | `core_crypto::finality::FinalityVote` | A validator's signed vote for a block height; gossiped, at most 512 bytes |
| `MODEL_REQUEST` | `0x50` | 32B hash | Requests a model file |
| `MODEL_TRANSFER` | `0x53` | 40B header + file | Model file transfer |
| `SHARE_PUSH` | `0x60` | `SharePush` | A signed Shamir share of an encrypted query, pushed to a custodian |
| `SHARE_CLAIM` | `0x61` | `AgentClaim` | A ranked candidate's VRF-backed claim; the custodian answers on the same stream with a length-prefixed `ShareRelease` |
| `AGENT_RESPONSE_DIRECT` | `0x63` | `AgentResponseDirect` | The responder's sealed reply plus its signed `ServiceClaim` |
| `RELAY_OPEN` | `0x70` | `RelayOpen` | Wraps a `SHARE_PUSH` or `AGENT_RESPONSE_DIRECT` for forwarding through an intermediary (at most `MAX_RELAY_HOPS` = 2) |

Any other type byte is rejected.

### Main structures

| Type | Notes |
|---|---|
| `Hello` | `signing_digest()`, `verify()` (signature must match the claimed peer id), `dialable_addrs(source_ip)` |
| `ModelInfo` / `PeerCapabilities` / `caps_bits` | Model name (max 96 bytes), tier, hash, size; `HAS_AI` and `CAN_RELAY` bits |
| `LotteryInputs` | Hour epoch, plaintext length, engine count, random salt; `digest()` seeds `rank_candidates_weighted` |
| `SharePush` (v3) | Message id, sender, `LotteryInputs`, candidate list (max `MAX_CANDIDATES` = 64) with one lottery weight per candidate (1 to 100, from answer quality), share index/total/threshold, share bytes, bundle digest, optional channel / session token / minimum responder maturity, VRF seed. `validate()` checks the version, Shamir parameters (`ShamirConfig::try_new`), that the VRF seed derives from the message, and the signature. `wrapped_for()` returns the top `MAX_CLAIM_RANK + 1` candidates of the weighted ranking |
| `AgentClaim` (v2) | Claimant, rank, VRF proof, signature |
| `ShareRelease` | A released share |
| `AgentResponseDirect` (v2) | Query id and digest, responder, `SealedMessage` reply, `ServiceClaim`; `verify()` checks the signature and that the claim describes this response |
| `AgentReply` | Decrypted reply: text and optional memory |
| `RelayOpen` | Origin, destination, hop TTL, inner type and bytes; `inner_type_is_relayable()` |
| `PeerList` / `PeerEntry` | `[4B count]` then per entry `[1B family (4/6)][addr][2B port][8B last_seen_ms]`, at most `MAX_PEER_LIST_ENTRIES` (256) |
| `GossipPeer` | `[1B 1][1B addr_len 4/16][addr][2B port][4B quality]` |
| `NodeId`, `generate_node_id()`, `random_message_id()` | Random `u64` ids |

### Payload caps

The receiver checks the declared length before allocating.

| Constant | Cap | Applies to |
|---|---|---|
| `MAX_HELLO_BYTES` | 2 KiB (and at most `MAX_HELLO_ADDRS` = 8 addresses) | `HELLO` |
| `MAX_PEER_LIST_BYTES` | 64 KiB | `PEER_LIST` |
| `MAX_GOSSIP_PEER_BYTES` | 64 B | `GOSSIP_PEER` |
| `MAX_ROOM_ENVELOPE_BYTES` | 96 KiB | `ROOM_ENVELOPE` |
| `MAX_CHAIN_TX_BYTES` | 32 KiB | `CHAIN_TX` |
| `MAX_BLOCK_BYTES` | 4 MiB | `BLOCK_BROADCAST` |
| `MAX_SHARE_PUSH_BYTES` | 1 MiB | `SHARE_PUSH` |
| `MAX_SHARE_CLAIM_BYTES` | 4 KiB | `SHARE_CLAIM` |
| `MAX_SHARE_RELEASE_BYTES` | 1 MiB | `ShareRelease` reply |
| `MAX_AGENT_RESPONSE_BYTES` | 512 KiB | `AGENT_RESPONSE_DIRECT` |
| `MAX_RELAY_OPEN_BYTES` | 1 MiB (+1 KiB envelope) | `RELAY_OPEN` |
| inline in `node_runtime.rs` | 256 B / 64 B / 32 B | `NODE_STATS` / `MODEL_ANNOUNCE` / `MODEL_REQUEST` |

`core_crypto::wire::decode` additionally refuses anything over 8 MB.

## Rooms (`rooms.rs`)

End-to-end encrypted group chat. Relaying nodes see only ciphertext.

| Item | Description |
|---|---|
| `RoomCode` | `XXXX-XXXX-XXXX-XXXX` from an unambiguous 31-character alphabet. `generate()`, `parse()` (case-insensitive, dashes optional), `as_str()`, `keys()` |
| `RoomKeys` | Derived from the code and the active network's chain id: public `room_id` and a secret XChaCha20-Poly1305 key (zeroized on drop) |
| `RoomPayload` | `Join { contributes }`, `Presence { contributes }`, `Leave`, `Message { text, is_ai }` (text at most `MAX_ROOM_TEXT_BYTES` = 32 KiB) |
| `RoomEnvelope` (v1) | `room_id`, sender, 16-byte message id, timestamp, TTL, nonce, ciphertext, signature |
| `RoomEnvelope::seal(keys, signer, payload, now)` | Encrypts the payload (AAD binds room, sender, message id and time), signs the envelope with the sender's identity key, TTL `ROOM_FLOOD_TTL` (6) |
| `RoomEnvelope::verify(now)` | Checks version, TTL, clock skew (`ROOM_MAX_CLOCK_SKEW_MS`, 10 min) and signature; needs no room key, so relays can verify before forwarding |
| `RoomEnvelope::open(keys)` | Decrypts and decodes the payload for members holding the code |
| `animal_for(peer_id, room)` | Stable per-room codename (e.g. `Otter-42`); `AI_CODENAME` is `N-AI` |
| `mentions_agent(text)` | Detects `@nai`, or the older `@nepheral` / `@agente`, as a whole word |

## Security layers

```
Query / room text
    |  redacted on the sender (ai-engine Redactor)
    v
XChaCha20-Poly1305 (EncryptedBundle to ranked candidates, or room key)
    |
    v
Shamir split across custodians (agent queries; t = max(2, ceil(n/2)))
    |
    v
Signed protocol message (Ed25519, network-bound domain)
    |
    v
QUIC with mutual TLS 1.3, Ed25519 identities
```
