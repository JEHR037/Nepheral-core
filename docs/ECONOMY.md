# Nepheral Economy — N-AI Credit Lifecycle and Emission Model

This document describes how the N-AI credit unit enters circulation,
how it is spent, and how the issuance rate is calibrated against the
real cost of compute. It is the authoritative reference for the
constants in `core-crypto/src/pallets/{contributions,nai}.rs` and
the dispatcher logic in `core-crypto/src/chain.rs`.

The model is designed around two principles:

1. **N-AI is anchored to compute.** A credit should be redeemable for
   roughly its dollar-equivalent of inference work. Long-term emission
   is targeted to track the team's marginal GPU cost plus a fixed
   buffer.
2. **Mesh contribution is the primary supply.** Direct sale via LTC
   exists so users can top up or skip the contribution path, but the
   intent is for most credits in circulation to come from network
   participation.

---

## 1. Credit unit

The on-chain unit is the **N-AI credit**, an integer count tracked by
the Balances pallet. There are no fractional credits — all redemption
and burn math is integer.

Current peg: **1 N-AI ≈ USD 0.026** (= 1 LTC / 5,000 at LTC ≈ 130 USD).
The peg is enforced one-directionally — the team accepts LTC at this
rate via `Nai::DirectSale`. The reverse (credits → LTC) is not
supported on chain.

---

## 1.1 Treasury and total supply

The chain mints **1,000,000 N-AI to the authority wallet at genesis**
(`TREASURY_GENESIS_AMOUNT` in `core-crypto/src/chain.rs`). This sits
in the authority's regular balance — spendable via standard signed
`Balances::Transfer` extrinsics — and is intended for:

- airdrops to early adopters,
- partnership / liquidity arrangements,
- bootstrap incentives,
- any future authority-led on-chain initiative.

The genesis allocation is **the only** initial supply. From there:

- **Issuance** = direct sales (1 N-AI per `1 / NAI_CREDITS_PER_LTC`
  of an LTC) **plus** contribution redemption (mined from custodian
  and lottery work, see §4).
- **Removal** = `Nai::Spend` burns. Burned credits leave circulation
  permanently — `balances.burn_checked` subtracts from the signer's
  balance and no other account receives them. The `total_spent`
  counter in `NaiStorage` is a running diagnostic only; the burn
  itself is reflected in the supply (`Σ balances`) directly.

The supply identity at any height H is:

```
supply(H) = TREASURY_GENESIS_AMOUNT
          + Σ Nai::DirectSale.credits applied up to H
          + Σ Contributions::Redeem.credits minted up to H
          - Σ Nai::Spend.PRIORITY_SESSION_COST burned up to H
```

`NaiStorage.total_minted` tracks the first two terms; the burn
counter tracks the last; the genesis seeds the constant. A node can
reconstruct supply from chain replay alone.

## 2. How credits enter circulation

Two issuance paths, both gated by the chain dispatcher:

### 2.1 Direct sale (`Nai::DirectSale`)

Authority-signed extrinsic. The LTC poller (running on the authority
node) observes confirmed Litecoin payments to the on-chain
`direct_sale_wallet`, converts the LTC amount to credits at the fixed
rate `NAI_CREDITS_PER_LTC`, and emits one `DirectSale` per confirmed
transaction.

Rules:

- One transaction → at most `MAX_DIRECT_SALE_CREDITS` credits.
- Payments above the cap require off-chain coordination with the
  team; the poller refuses to emit them automatically.
- Fractional LTC is accepted; the poller floors to integer credits at
  the fixed ratio.
- Idempotency is enforced by `NaiStorage.direct_sales` keyed on
  `payment_ref` (an opaque, currency-agnostic reference; Litecoin today).

### 2.2 Contribution emission (`Contributions::Redeem`)

Peer-signed. The signer must equal the receiver. The Redeem
extrinsic converts accumulated custodian work and lottery wins into
credits according to the formula in §4.

The two underlying counters:

- **`share_counts[peer]`** — Shamir share custodianship events. Bumped
  by a founder-signed `Contributions::RecordShare` extrinsic each
  time the peer stores a share routed to them via `SHARE_PUSH`.
- **`wins_by_peer[peer]`** — answered queries. Bumped when a
  `Contributions::SubmitReceipt` lands on chain. A receipt is a service
  claim signed by the responder (query digest, model size, output tokens,
  generation time) and countersigned by the sender after it verified and
  accepted the answer. The chain deduplicates receipts by query digest
  per hour and caps them at 120 per sender and 30 per sender/responder
  pair per hour, so a pair of colluding wallets cannot farm wins. Each
  receipt also records one consumption for the sender and feeds the
  lottery dimension from the claimed model size and work.

These counters never decrease. The `redeemed[peer]` counter tracks
what has been paid out, so the Redeem path computes:

```
redeemable_shares    = share_counts[peer]  - redeemed[peer].shares
redeemable_lotteries = wins_by_peer[peer] - redeemed[peer].lotteries
```

A peer may redeem any amount up to those caps in a single extrinsic.
Only the equivalents actually converted into whole credits are marked
redeemed; the remainder carries over to the next Redeem. Every minted
credit is added to `nai.total_minted`.

Eligibility to be selected in the lottery uses the peer's contribution
maturity. A new peer that consumes without contributing keeps access
during a grace of 50 consumptions, limited to 7 days after it was first
seen on chain.

---

## 3. How credits leave circulation

### 3.1 Priority session burn (`Nai::Spend`)

Peer-signed. Burns `PRIORITY_SESSION_COST` from the signer's balance
and opens an `NaiSession` keyed by a client-generated `session_token`.

Each session covers `INFERENCES_PER_SESSION` inferences against the
team-hosted larger models (Qwen3-72B class), valid for
`SESSION_TTL_MS` milliseconds.

The session token is a one-shot nonce — reuse is rejected by the
pallet so a peer cannot "extend" a session past its intended TTL.

### 3.2 Transfer (`Balances::Transfer`)

Peer-to-peer credit movement. Standard signed extrinsic; no fee in
v1. Does not enter or leave circulation, just changes the owner.

---

## 4. The Redeem formula

Custodian shares and lottery wins are not equivalent units of work.
A custodian stores a Shamir share and forwards it on demand —
bandwidth and disk work, negligible compute. A lottery winner runs
the actual AI inference on their local engine, which is the
expensive operation in compute terms.

This replaces the flat `LOTTERY_WIN_SHARE_VALUE = 200`
constant with a per-win **dimension** computed by the authority
when each lottery is sealed into a block. The dimension reflects
the actual work done — model size, output token count, generation
time. A Qwen3-72B inference earns substantially more dimension than
a Qwen3-4B one without any constant lookup table.

The formula in `core-crypto/src/lottery.rs::compute_dimension` is:

```
token_log    = ilog2(1 + output_tokens / 100)
model_factor = max(1, model_params_billions)
time_log     = ilog2(1 + gen_time_ms / 100)
dimension    = DIMENSION_BASE                     // = 25
             * max(1, token_log + 1)
             * model_factor
             + min(4, time_log / 2)
```

Reference points:

| model_b | tokens | gen_ms | dimension |
|---|---|---|---|
| 4 | 500 | 10_000 | ~225 |
| 32 | 500 | 5_000 | ~1800 |
| 72 | 500 | 1_000 | ~3825 |
| 72 | 2000 | 1_000 | ~7650 |
| 405 | 500 | 5_000 | ~21000 |

The Redeem formula then becomes a straight sum + floor-divide:

```
total_share_equivalents = shares + lottery_dimension
credits                 = total_share_equivalents / SHARES_PER_CREDIT
```

Where `lottery_dimension` is the **accumulated** dimension across
the peer's wins (`Σ result.dimension` for every `LotteryResult`
naming the peer as winner). A peer who has custodied 800 shares
and accumulated 2100 dimension across several wins (say one Qwen3-4B
win at 200 plus one Qwen3-72B win at 1900) can redeem:

```
total_share_equivalents = 800 + 2100 = 2900
credits                 = 2900 / 1000 = 2 credits
                                       (900 share-equiv remainder
                                        stays in the pool)
```

The integer floor matters: a Redeem call producing zero credits is
rejected by the pallet so the `redeemed` counter does not advance
without anything being minted.

### Why dimension is computed off-chain

The formula intentionally lives in `core-crypto::lottery::compute_dimension`
— not as an extrinsic, not inside a pallet's `apply`. The authority
calls it when queueing each `LotteryResult` and the resulting number
is signed into the block. Peers trust the dimension the same way
they trust the rest of the block: via the authority's signature
over the block header. Retuning the formula is a pure software
release, not a hard fork — old blocks' dimensions are immutable
because they're embedded in already-signed headers, but the next
block can use a new formula without coordination.

---

## 5. Calibration — anchoring emission to compute cost

The constants above are calibrated against the team's marginal GPU
cost so emission tracks real compute and the system does not dilute
the LTC peg.

### Reference cost (mid-2026)

- Qwen3-72B on RunPod 1x H100, vLLM batched, INT8: ≈ 2,000 tokens/sec
  output at ≈ USD 3/hour.
- One priority session = 10 inferences ≈ 30,000 tokens of work ≈
  150 seconds ≈ USD 0.125 team cost.
- User pays `PRIORITY_SESSION_COST = 10` credits ≈ USD 0.26.
- Direct sale margin: 52 %.

### Mesh equivalence

The mesh runs Qwen3-4B on user hardware. FLOPs ratio is 18× (4B vs
72B per token), adjusted to 25× for output-quality parity. One
priority session is therefore equivalent in compute terms to:

- ≈ 250 mesh messages
- ≈ 1,250 custodian-share events (5 custodians per message)
- ≈ 250 lottery wins (1 winner per message)

With the constants above, those 250 mesh messages emit:

```
emitted_share_equivalents = 1250 + 250 * 200 = 51,250
emitted_credits           = 51,250 / 1000    = 51 credits ≈ USD 1.33
```

That is above the team's USD 0.125 marginal cost for the same work
— intentionally so. The bootstrap phase subsidizes contribution to
attract early participants. The dynamic adjustment in §6 closes the
gap as the network grows.

### Per-user expectation

A casually active user, online 8 hours, participating in 100 message
flows per day (custodian on ~60, responder on ~3):

```
60 shares       +   3 wins × 200 = 660 share-equivalents per day
                                   ÷ 1000
                                   = 0.66 credits per day
```

≈ 24 priority sessions per year from pure contribution. Modest, real.

A heavy active user, 16 hours, 1000 flows/day:

```
600 shares  +  30 wins × 200 = 6,600 share-equivalents per day
                              ÷ 1000
                              = 6.6 credits per day
```

≈ 241 priority sessions per year. Enough to make the contribution
path the natural way for active users to fund their own priority
usage without ever touching LTC.

---

## 6. Dynamic `SHARES_PER_CREDIT` (Economy pallet)

The initial value of 1000 is calibrated for a small bootstrap mesh.
As the network grows the per-second share rate grows linearly, but
the team's GPU capacity does not. Without an adjustment mechanism,
contribution emission would scale faster than buy-side demand and
dilute the LTC peg.

### Adjustment loop

A new `EconomyStorage` field carries the live ratio. Every
`ADJUSTMENT_PERIOD_MS` (10 minutes of wall clock, read from block
timestamps) the block dispatcher computes:

```
window           = blocks whose timestamp is within the last
                   ADJUSTMENT_WINDOW_MS (1 hour)
span_ms          = now - oldest timestamp in the window, capped at 1 hour
equiv_window     = sum of share-equivalents recorded in those blocks
target_credits   = TARGET_CREDITS_PER_HOUR * span_ms / 3_600_000
new_ratio        = clamp(MIN_SHARES_PER_CREDIT,
                         MAX_SHARES_PER_CREDIT,
                         equiv_window / max(1, target_credits))
```

Under 10 minutes of history the ratio is left alone.

The window is wall-clock time, not a block count. The producer only seals a
slot when it has something to seal (plus a heartbeat), so one block stands
for anything from two seconds to a minute. An earlier version counted 1,800
blocks and priced them as one hour; on the testnet those blocks spanned 3.4
hours under load and about 15 when quiet, so it minted 3 to 15 times less
than its target, and least when the network was quiet. Changing this changed
every ratio the chain computes, hence chain generation 11.

This keeps the credit emission rate centered on
`TARGET_CREDITS_PER_HOUR` regardless of how much mesh activity is
happening. Initial parameters:

- `TARGET_CREDITS_PER_HOUR = 100` — ≈ USD 2.60/hour worth of credits
  emitted by contribution. Tunable via authority extrinsic.
- `ADJUSTMENT_WINDOW_MS` = 1 hour of block timestamps
- `ADJUSTMENT_PERIOD_MS` = 10 minutes
- `MIN_SHARES_PER_CREDIT = 100` — even in a dead mesh, one credit
  costs at least one custodian's worth of work to mint.
- `MAX_SHARES_PER_CREDIT = 1_000_000` — sanity ceiling.

### Behavior under different network sizes

| Mesh activity (shares + wins×200 / hour) | Resulting ratio | Credits emitted/hour |
|---|---|---|
| 1,000 (very small mesh) | clamps to MIN = 100 | 10/hour |
| 10,000 (bootstrap target) | 100 | 100/hour ✓ |
| 150,000 (mid-size mesh) | 1,500 | 100/hour ✓ |
| 1,500,000 (large mesh) | 15,000 | 100/hour ✓ |
| 100M+ (massive mesh) | clamps to MAX = 1M | 100/hour |

Steady state: total emission is constant in dollar terms, individual
share value drops as the network grows. Equivalent to Bitcoin
difficulty adjustment in spirit.

---

## 7. Who controls the parameters

These parameters change on chain, without a code redeploy:

| Parameter | Call | Controlled by |
|---|---|---|
| `target_credits_per_hour` | `Economy::SetEmissionTarget` | Founder; after dissolution only an enacted proposal |
| `daily_mint_cap` | `Nai::SetDailyMintCap` | Founder, then governance |
| Channel session cost | `Nai::SetChannelSessionCost` | Founder, then governance |
| Sale oracle | `Nai::SetSaleOracle` | Founder, then governance |
| Direct sale wallet | `Nai::SetDirectSaleWallet` | Founder, then governance |

`NAI_CREDITS_PER_LTC` and `PRIORITY_SESSION_COST` remain compile-time
constants and change only with a release.

Governance proposals vote for 20,000 blocks and wait 20,000 more before
enactment, with a two-thirds quorum of the weight cast and 20%
participation. Voting weight is contribution maturity, never balance, so
credits bought on the market carry no votes. The founder may dissolve
only after governance has replaced the sale oracle by vote. See
`docs/GOVERNANCE.md`.

---

## 8. Calibration log

Initial values:

| Constant | Initial value | Rationale |
|---|---|---|
| `NAI_CREDITS_PER_LTC` | 5,000 | 1 N-AI ≈ USD 0.026 at LTC ≈ 130 USD. |
| `MAX_DIRECT_SALE_CREDITS` | 5,000 | Single LTC payment cap. Larger purchases off-chain. |
| `PRIORITY_SESSION_COST` | 10 | Sale price USD 0.26, marginal GPU cost USD 0.125, margin ≈ 52 %. |
| `LOTTERY_WIN_SHARE_VALUE` | 200 | Reflects 18-25× FLOPs ratio (4B vs 72B) plus 8-10× quality compensation. |
| `SHARES_PER_CREDIT` (initial) | 1,500 | Bootstrap-phase subsidy: emits ≈ USD 0.88 per session-equivalent of mesh work vs team cost USD 0.125. Dynamic adjustment will compress this over time. |
| `TARGET_CREDITS_PER_HOUR` | 100 | Steady-state emission target ≈ USD 2.60/hour. |
| Adjustment window | 1 hour | Smooths transient bursts without lag-locking the chain. |
| Adjustment period | 10 minutes | Fast enough to feel responsive, slow enough to avoid chasing noise. |

Every value here is a parameter, not a fundamental constant. Expect
all of them to be retuned at least once after the first month of
real network data.

## Secondary market

Credits earned by contributing can be sold to other participants without the
network acting as a counterparty. The `Market` pallet holds a notice board with
two sides, and the asymmetry between them is deliberate.

**Asks are covered.** Posting a sell listing deducts the credits from the
seller's balance and holds them in pallet escrow in the same block the listing
becomes visible. A listing can therefore never advertise credits the seller has
already spent elsewhere, and a buyer can verify this without trusting anyone.
Withdrawing the listing returns the escrow in full.

**Bids are not.** A buy listing is an advertisement and nothing more. The chain
cannot hold Litecoin, and a buy-side escrow would be a promise it has no way to
keep, so it does not offer one.

**Settlement is one-directional.** The buyer pays the seller directly at the
Litecoin address on the listing. The sale oracle observes that payment
off-chain and calls `Settle`, which releases the escrow to the buyer. The
network never holds, forwards or custodies Litecoin at any point, and the
seller's proceeds never pass through it.

The trust boundary this leaves is explicit: the oracle asserts that a payment
happened, and the chain takes that assertion at face value for that one ask.
It is the same key, and the same boundary, as a direct sale. What the oracle cannot do is mint credits out of a
listing, take an escrow for itself, or settle the same payment twice: escrow
only ever moves to the named buyer, and a payment reference is recorded and
refused on replay.

Prices are quoted in litoshi for the whole lot, with per-credit shown alongside
so listings can be compared. The network sets no price and takes no fee.
