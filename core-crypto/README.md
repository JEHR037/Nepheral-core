# core-crypto

Cryptographic and chain foundation for Nepheral: Shamir Secret Sharing,
sealed and multi-recipient encryption for mesh queries, the network selector
and domain-separated signing, the binary wire codec, service receipts, a
mini-Substrate-style chain with ten pallets, the responder lottery and VRF,
node identity, and mnemonic handling.

Run the tests with `cargo test -p core-crypto`.

---

## Shamir Secret Sharing (`lib.rs`)

Shamir's scheme over GF(2^8) (reduction polynomial `0x11B`, the AES field),
using compile-time exp/log tables for multiplication and inversion. Each
secret byte is processed independently.

| Item | Description |
|---|---|
| `ShamirConfig { total_shares, threshold }` | Scheme parameters (`n`, `t`) |
| `ShamirConfig::try_new(n, t) -> Result<Self, String>` | Validated constructor: `n >= 1`, `t >= 1`, `t <= n`. Invalid input is an error, never a panic |
| `ShamirConfig::dynamic_for(n_peers)` | Sizes the scheme to the peers available: `n = clamp(n_peers, 1, 255)`, `t = max(2, ceil(n/2))` for `n >= 2`, otherwise `t = 1` |
| `ShamirShare { index, value }` | One share; `Zeroize` + `ZeroizeOnDrop` |
| `ShamirSSS::new(config)` | Engine for one configuration |
| `split_secret(&self, secret) -> Result<Vec<ShamirShare>, String>` | Splits into `n` shares with a CSPRNG; rejects an empty secret |
| `reconstruct_secret(&self, shares) -> Result<Vec<u8>, String>` | Lagrange interpolation over the first `t` shares; rejects too few shares, index 0, duplicate indices and inconsistent lengths |
| `ShamirSSS::fingerprint_share(share) -> [u8; 32]` | SHA-256 over index and value |

```rust
let shamir = ShamirSSS::new(ShamirConfig::try_new(5, 3)?);
let shares = shamir.split_secret(b"my-secret")?;
assert_eq!(shamir.reconstruct_secret(&shares[..3])?, b"my-secret");
assert!(shamir.reconstruct_secret(&shares[..2]).is_err());
```

Fewer than `t` shares reveal nothing about the secret. The arithmetic is
table-based and not hardened against timing side channels.

## Network selection and signing domains (`network.rs`)

| Item | Description |
|---|---|
| `Network::{Testnet, Mainnet}` | The two networks. `name()`, `parse()` (accepts `testnet`/`test`, `mainnet`/`main`, case-insensitive) |
| `Network::chain_id()` | SHA-256 over the network name and `chain_file::CHAIN_GENERATION` |
| `Network::alpn()` | QUIC ALPN, `nepheral/3/<network>`, so nodes on different networks cannot connect |
| `Network::default_bootstraps()` | Built-in bootstrap peers (testnet has one; mainnet none) |
| `Network::data_subdir()` | Per-network data directory name |
| `set_active(n)` / `active()` | Process-wide selection, fixed on first use. Without `set_active`, `active()` reads `NEPHERAL_NETWORK`, then the build-time `NEPHERAL_DEFAULT_NETWORK`, then defaults to testnet |
| `signing_digest(domain, parts) -> [u8; 32]` | SHA-256 over length-prefixed domain, the active `chain_id`, then the parts. Every signature in the system is taken over one of these digests |
| `domains::*` | `EXTRINSIC`, `BLOCK_HEADER`, `VRF`, `HELLO`, `SHARE_PUSH`, `SHARE_CLAIM`, `AGENT_RESPONSE`, `ROOM_ENVELOPE`, `WIN_RECEIPT`, `SERVICE_RECEIPT` |

Binding the chain id into every digest means a signature made on testnet
never verifies on mainnet, and the domain tag keeps one message type from
being replayed as another.

## Wire codec (`wire.rs`)

| Item | Description |
|---|---|
| `encode(&T) -> Vec<u8>` | `postcard` serialization |
| `decode(&[u8]) -> Option<T>` | `postcard` deserialization; inputs over `MAX_DECODE_BYTES` (8 MB) are refused before parsing, and malformed input returns `None` |
| `bytes64` | Serde helper for `[u8; 64]` signatures (raw bytes in binary formats, hex in JSON) |

Blocks, extrinsics and all mesh protocol messages use this codec.

## Agent-query encryption (`agent_crypto.rs`)

X25519 keys are derived from the node's ed25519 identity, so a peer id is
also an encryption key.

| Item | Description |
|---|---|
| `pubkey_ed_to_x()` / `signing_key_ed_to_x_secret()` / `signing_key_ed_to_x_public()` | ed25519 to X25519 conversion |
| `derive_shared_key()` | ECDH plus key derivation bound to both public keys and a context |
| `WrappedKey` / `wrap_session_key()` / `unwrap_session_key()` | A session key wrapped for one recipient |
| `aead_encrypt()` / `aead_decrypt()` | XChaCha20-Poly1305 with a random 24-byte nonce |
| `generate_ephemeral_keypair()` | Per-message X25519 keypair |
| `EncryptedBundle` | Ephemeral pubkey, one `WrappedKey` per recipient, AEAD ciphertext. `encrypt(plaintext, recipients)`, `decrypt(...)`, `digest()`, `to_bytes()` / `from_bytes()` |
| `SealedMessage` / `seal_to_peer()` / `open_from_peer()` | Single-recipient sealed box (used for the responder's reply) |
| `CryptoError` | Error type |

## Service receipts (`receipts.rs`)

Proof that a node answered a query, used to credit contribution on chain.

| Item | Description |
|---|---|
| `ServiceClaim` | Signed by the responder: query digest, sender and responder ids, hour epoch, model hash, model size (B params), output tokens, generation time |
| `ServiceClaim::sign(key, ...)` | Builds and signs a claim, clamping figures to `MAX_RECEIPT_MODEL_B` (72), `MAX_RECEIPT_TOKENS` (8192), `MAX_RECEIPT_GEN_MS` (600 000) |
| `ServiceClaim::verify()` / `claim_digest()` / `dimension()` | Rejects self-service and out-of-range figures; `dimension()` applies `lottery::compute_dimension` |
| `ServiceReceipt` | A claim countersigned by the sender. `countersign(claim, sender_key)`, `verify()` |
| `ReceiptError` | `BadResponderKey`, `BadResponderSignature`, `BadSenderKey`, `BadSenderSignature`, `SelfService`, `OutOfRange` |

The sender submits the receipt with `ContributionsCall::SubmitReceipt`.

## Lottery and VRF (`lottery.rs`)

| Item | Description |
|---|---|
| `rank_candidates_weighted(peers, weights, seed, n)` | Deterministic weighted ranking: each candidate draws one SHA-256 ticket per unit of weight over the seed (the query's `LotteryInputs` digest) and ranks by its best ticket, so the chance of ranking first is proportional to the weight. Weights come from answer quality. Selects who a query is encrypted for |
| `lottery_select_top_n()` / `lottery_select()` | Score-based ranking over hour epoch, message length and engine count (not used by the current query flow) |
| `vrf_seed(message_id, sender_pid, hour_epoch)` | Per-query seed |
| `vrf_evaluate(signing_key, seed) -> (output, proof)` | ed25519 signature over the VRF-domain digest; output is a hash of the proof |
| `vrf_verify(pubkey, seed, proof) -> Option<output>` | Checks a claimant's proof |
| `compute_dimension(model_params_b, output_tokens, gen_time_ms)` | Contribution weight of one answered query (`DIMENSION_BASE` = 25) |
| `current_hour_epoch()` | Unix hours |
| `PI_BITS` | Constant mixed into the legacy lottery score |

## Chain (`chain.rs`, `block.rs`, `extrinsic.rs`, `validators.rs`, `chain_file.rs`)

### Extrinsics

`Extrinsic { version, pallet_idx, args, signer, nonce, signature }`.
`args` is the JSON-encoded call; the envelope travels as `wire` bytes.

| Item | Description |
|---|---|
| `Extrinsic::sign(key, pallet_idx, &call, signer, nonce)` | Signs over `signing_digest(domains::EXTRINSIC, ...)` |
| `verify_signature()` | Rejects a version other than `EXTRINSIC_VERSION` (2) and args over `MAX_EXTRINSIC_ARGS_BYTES` (16 KiB) |
| `to_bytes()` / `from_bytes()` | Wire codec |
| `DecodedCall::decode(pallet_idx, args)` | Routes to the pallet's call enum |
| `PALLET_*` | `SYSTEM` 0, `BALANCES` 1, `CONTRIBUTIONS` 2, `NAI` 3, `AUTHORITY` 5, `ECONOMY` 6, `MARKET` 7 |

### Blocks

| Item | Description |
|---|---|
| `BlockHeader` | version, height, slot, parent hash, state root, extrinsics root, lottery root, author, signature, timestamp |
| `BlockHeader::hash_for_signing()` / `sign()` / `verify_signature()` / `block_hash()` | Authority signing over `domains::BLOCK_HEADER` |
| `BlockHeader::slot_matches_timestamp()` | Slot within `SLOT_DRIFT_TOLERANCE` of `timestamp / SLOT_DURATION_MS` (2 s slots) |
| `Block::extrinsics_root()` / `lottery_root()` | Commitments over the body |
| `LotteryResult` | Winner and dimension for one query, with the model size, tokens and time it was computed from; `dimension_matches_inputs()` |

The genesis block's parent hash is the active network's `chain_id`.

### `ChainState`

Pallet storages plus block history.

| Method | Description |
|---|---|
| `new()` / `new_empty()` / `with_genesis_authority(pk)` | Constructors |
| `apply_extrinsic(ext, height, ts, author)` | Verifies the signature, enforces founder-only, member-only and sale-oracle-only calls, bumps the nonce, dispatches to the pallet and executes enacted proposals |
| `apply_block(block)` | Verifies the author signature against the slot's primary in the validator set derived from chain state (`NoValidators` on an empty set), requires a slot later than the parent's, then applies |
| `reorg(blocks)` | Builds the chain that results from a longer fork no more than `MAX_REORG_DEPTH` (64) blocks deep, from in-memory checkpoints every 16 blocks; returns it with the orphaned extrinsics |
| `apply_block_unverified()` | Applies without the signature and state-root checks |
| `check_block_shape(block)` | Size limits (`MAX_BLOCK_EXTRINSICS` 512, `MAX_BLOCK_LOTTERY_RESULTS` 256, `MAX_BLOCK_BYTES` 2 MiB), extrinsics root, lottery root and slot/timestamp |
| `state_root()` | SHA-256 over every pallet's root |
| `serialize_blocks()` / `deserialize_blocks()` / `replay_blocks(blocks)` | Persistence; replay stops at the first invalid block |
| `balance_of()` / `redeemable()` / `consumed_of()` / `shares_of()` / `wins_of()` / `history_of()` / `last_win_of()` | Queries |
| `head_hash()` / `head_height()` / `head_timestamp_ms()` / `block_count()` / `ext_count()` / `lottery_count()` | Chain-head queries |

Block application checks height, parent hash, shape and roots, and that the
timestamp does not go backwards. It then snapshots state and applies every
extrinsic and lottery result. An invalid extrinsic, a `LotteryResult` whose
dimension does not match its inputs, or (for verified blocks) a state root
that differs from the header **rolls the state back** to the snapshot and
rejects the block (`BlockContainsInvalidExtrinsic`,
`LotteryDimensionMismatch`, `BadStateRoot`).

`team_pubkey()` returns `NEPHERAL_TEAM_PUBKEY`, overridable at build time
through the environment variable of the same name.

### Validators

`ValidatorSet { authorities, peer_ids }`: `new()`, `is_authority()`,
`primary_for_slot()`, `finality_threshold()`, `verify_block_signature()`.
`authority_peer_id()` maps a key to a peer id. The set in force for a block
is `ChainState::validator_set()`, derived from the on-chain authority
storage of the parent state.

### Chain file (`chain_file.rs`)

On-disk format: `CHAIN_FILE_MAGIC` (`NEPHCHN\0`) + `CHAIN_GENERATION`
(big-endian u32, currently 8) + serialized blocks. `with_header()`,
`strip_header()` (returns `None` for a foreign file or another generation)
and `generation_of()`.

## Pallets (`pallets/`)

| Pallet | Index | Calls |
|---|---|---|
| `system` | 0 | None; holds nonces, block height and slot |
| `balances` | 1 | `Transfer` |
| `contributions` | 2 | `RecordShare`, `RecordConsumption` (founder only), `Redeem`, `SubmitReceipt` |
| `nai` | 3 | `DirectSale`, `Spend`, `UseSession`, `OpenChannelSession`, `SetDirectSaleWallet`, `SetChannelSessionCost`, `SetDailyMintCap`, `SetSaleOracle`, `SetSaleHealth` |
| `authority` | 5 | `Add` and `Dissolve` (founder 0 only), `Remove` (founder 0, or a node resigning; founder 0 cannot be removed), `SetChannels` (founder 0 or self), `RotateKey` (self). The set is also the block producer schedule |
| `economy` | 6 | `SetEmissionTarget` (founder only; after dissolution only by vote) |
| `governance` | 8 | `Propose`, `Vote`, `Enact`: contribution-weighted votes, 20,000-block voting period plus 20,000-block enactment delay, membership proposals only after founder 0 resigns; see `docs/GOVERNANCE.md` |
| `finality` | 10 | `Finalize` (a justification: two thirds plus one of the authority set signed the block; sets the finalised height, no reorg below it), `ReportEquivocation` (two votes at one height from one validator; removes it, founder 0 is only recorded). Types in `finality.rs` |
| `reputation` | 9 | `Rate`: like or justified dislike on a landed receipt, weighted by the rater's maturity, decaying per day; see `docs/REPUTATION.md` |
| `market` | 7 | `PostAsk`, `CancelAsk`, `PostBid`, `CancelBid`, `ClaimFill`, `FailClaim`, `Settle` |

`SubmitReceipt` must be signed by the receipt's sender. It verifies both
signatures, accepts only the current or previous hour, rejects a query
digest that was already credited, and enforces per-sender and per-pair
hourly limits. `DirectSale`, `SetSaleHealth`, `Settle` and `FailClaim` may
be signed by the registered sale oracle as well as by an authority.

## Payment memos (`payment_memo.rs`)

The `OP_RETURN` payload a Litecoin payment carries to name who is credited:
32 bytes for a direct sale (`Memo::Wallet`), 40 bytes for a market fill
(`Memo::Fill { buyer, ask_id }`). `encode_fill()`, `extract()`,
`extract_wallet_root()`, `extract_from_script_bytes()`.

## Identity and mnemonic (`identity.rs`, `mnemonic.rs`)

| Item | Description |
|---|---|
| `PeerId` | `[u8; 32]` |
| `NodeIdentity::from_mnemonic(&NodeMnemonic)` | Deterministic ed25519 key from the mnemonic seed and a self-signed Ed25519 TLS certificate carrying that key. `peer_id` is the ed25519 public key |
| `NodeIdentity::ephemeral()` | Throwaway identity; its `peer_id` is the SHA-256 of the certificate |
| `cert_der` / `key_der()` / `fingerprint_short()` / `fingerprint_hex()` | Certificate, PKCS#8 key (zeroized on drop), and id formatting |
| `peer_id_from_cert(cert_der)` | SHA-256 of a certificate |
| `NodeMnemonic` | BIP-39 wrapper: `generate()`, `from_phrase()`, `phrase()`, `to_peer_id()`, `to_seed()` |

Mesh connections identify peers by the Ed25519 key inside the certificate
(see `mesh-network/src/tls.rs`), which equals `peer_id` for a
mnemonic-derived identity.

## Examples

`examples/derive_pubkey.rs`, `examples/new_authority.rs` and
`examples/sale-oracle-keygen.rs` derive or generate the key to bake as
`NEPHERAL_TEAM_PUBKEY`, and generate a sale-oracle key pair (writing the
phrase to a file, printing only the public key).
