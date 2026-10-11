# Blind tier

A channel where the responder runs inside a hardware enclave and the client
verifies which build it is talking to **before** the prompt is encrypted for
it. Status: **design, not implemented.** Nothing in this document ships today.

This is the concrete form of what the whitepaper calls Blind AI. Read
[WHITEPAPER.md §10](WHITEPAPER.md) first for what attestation can and cannot
establish; this document assumes those limits rather than restating them.

## 1. The gap it closes

Today the elected responder decrypts the prompt to run inference. The
whitepaper states this plainly as a known limitation: privacy comes from
structural routing, the way Tor protects a circuit, not from the responder
being unable to read what it processes. Anyone with administrative access to a
responder's host can read prompts out of its memory.

The Blind tier does not fix that for ordinary nodes. It creates a separate
channel where the responder's host is removed from the trust boundary by an
enclave, and where the client refuses to send anything to a responder that
cannot prove it is running an approved build.

## 2. The one rule: quote before wrap

The existing agent-room crypto already does the hard part.
`EncryptedBundle::encrypt(plaintext, recipients)` generates a session key,
wraps it per recipient with X25519 ECDH against an ephemeral key, and AEAD-
encrypts the payload. A recipient not in `recipients` receives no wrapped key
and therefore cannot open the bundle, whatever else it receives.

So the Blind tier needs no new cipher, no new envelope format, and no change to
the lottery. It needs one rule about list membership:

> A client building a Blind-tier request includes a responder in `recipients`
> only after that responder returns a fresh attestation quote, over a nonce the
> client chose, whose measurement matches one recorded as approved on chain.

Everything else follows. A responder that fails attestation still wins the
lottery, still receives the Shamir shares, and still cannot read the prompt,
because it holds no wrapped session key. The failure mode is inability to
answer, not disclosure.

The check runs **on the client**. The authority publishes which measurements
are approved; it does not sit in the path of the verification, and cannot wave
a responder through.

## 3. Wire additions

Two messages on the existing control stream, and one chain call.

| Message | Direction | Payload |
|---|---|---|
| `AttestRequest` | client → responder | `nonce: [u8; 32]` chosen by the client |
| `AttestQuote` | responder → client | `evidence: Vec<u8>`, `tee_kind: u8`, `measurement: [u8; 32]` |

`evidence` is the raw vendor quote (TDX, SEV-SNP, or a GPU CC attestation
report). The client verifies it with the vendor's own verification path, checks
that the embedded nonce is the one it sent, and then checks `measurement`
against the chain.

The nonce is what makes the quote fresh rather than replayed. It does not make
it unforgeable — see §6.

### Chain call

```rust
// pallets::authority, authority-gated
ApproveMeasurement {
    tee_kind: u8,
    measurement: [u8; 32],
    // Height after which this measurement stops being accepted.
    // Zero means "until explicitly revoked".
    expires_at_height: u64,
}
```

Approved measurements live in `AuthorityStorage` and feed its pallet root, so
every node agrees on the set and a client can verify locally against its own
replica without asking anyone.

This is what `attestation_hash` was reserved for. That field records a manual
audit today; this replaces it with a value that is checked.

## 4. Why the chain is the transparency log

Apple's Private Cloud Compute publishes the images it runs to a transparency
log so outside researchers can check which builds exist. Certificate
Transparency and Sigstore's Rekor do the same for certificates and signatures.
The property that matters is append-only public record: an operator cannot
quietly approve one build for one victim.

Nepheral already runs an append-only replicated log with an authority that
signs entries. Recording approved measurements as chain state gets that
property without introducing a second system to trust, and gives every client
a local copy to check against.

## 5. What a node operator must do

A Blind-tier responder is not a consumer machine. It is a rented confidential
VM — EPYC with SEV-SNP or Xeon with TDX, optionally with an H100-class GPU in
CC mode. Operating it in the spirit of the tier means:

- No interactive shell into the running VM. An operator who can attach a
  debugger has made the enclave decorative.
- Stateless: no prompt or response touches disk.
- The image is published and its measurement approved on chain before it
  serves anything.
- Session keys live only in enclave memory and are zeroized on session end.

The software half of this — leasing a key over a live session, refreshing it,
and dropping it when the session dies — is the same pattern documented
elsewhere in this project's key-lease design, and it runs **inside** the
enclave rather than instead of it. If the hardware layer is broken, the
software layer is what is still standing.

## 6. What this does not achieve

Stated here rather than left implied, because a tier that sells a privacy
guarantee attracts exactly the readers who will check it:

- **Physical access defeats it.** TEE.Fail extracted Intel's attestation
  signing keys with a DDR5 bus interposer costing under $1,000, then minted
  quotes that pass vendor verification at the highest trust level. Those same
  keys bypass NVIDIA GPU CC attestation. A forged quote is not caught by
  checking quotes more often, because it is freshly signed, not replayed. The
  mitigation is vendor key revocation, which is outside this project's control.
- **The enclave is an active research target.** RMPocalypse (CVE-2025-0033)
  and Heracles both broke SEV-SNP at CCS 2025. Treat the tier as raising the
  cost of extraction to "physical access or a published break", not as proof.
- **It says nothing about metadata.** Who connected to whom, when, and how much
  data moved are unchanged.
- **It does not apply to the open mesh.** Standard-tier responders decrypt
  prompts exactly as they do today. Nothing here is retroactive.

## 7. Order of work

1. `ApproveMeasurement` and the storage it writes, with the client-side check
   behind a flag that defaults off.
2. `AttestRequest` / `AttestQuote` on the control stream, verifying SEV-SNP
   evidence first — it is the tier most likely to be rented.
3. A reference VM image, published, with its measurement approved on chain.
4. GPU CC evidence, once an H100-class node exists to produce it.

Step 1 is worth doing before any of the rest: it converts `attestation_hash`
from a field nothing checks into one that does, which is an improvement even
while every measurement in it is still approved by hand.
