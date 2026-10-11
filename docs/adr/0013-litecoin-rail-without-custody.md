# ADR-0013: Litecoin as the payment rail, without custody

- **Status:** Accepted
- **Date:** 2026-10-10 (recorded retroactively)
- **Deciders:** Javier Hernandez

## Context

Credits are earned by contributing, but people who cannot contribute need a
way to buy them, and people who earned them may want to sell them. Holding
other people's money would turn the network into a custodian, with the
security and legal burden that carries. The payment rail has to be cheap,
fast enough, widely available, and able to carry a short note saying which
wallet a payment is for.

## Options considered

### Option 1: card payments through a processor

- Benefits: familiar to buyers.
- Drawbacks: accounts, chargebacks and a central processor; excludes many
  countries.

### Option 2: a stablecoin on a smart-contract chain

- Benefits: stable price.
- Drawbacks: gas fees, a dependency on another chain's contracts and
  issuer.

### Option 3: Litecoin, with the payment naming the wallet

- Benefits: low fees, blocks every two and a half minutes, broad exchange
  support, `OP_RETURN` outputs for the memo.
- Drawbacks: price volatility; the memo needs a transparent output, so
  payments from MWEB outputs cannot carry it.

## Decision

Credits are sold against Litecoin, and the network never holds, forwards or
custodies Litecoin.

### Key points

- **Direct sale:** the buyer pays the sale address with an `OP_RETURN` memo
  naming the receiving wallet. A payment with no wallet is never credited.
- **A separate sale oracle:** the payment watcher runs outside the nodes and
  signs with a key authorised on chain only for direct sales, which can be
  revoked in one block. Desktop builds carry none of that code.
- **Market:** credits offered for sale are held in chain escrow; bids are
  advertisements only. The buyer pays the seller directly and the escrow is
  released once the payment is observed against a claim either party
  recorded.
- **Fail closed:** when the oracle cannot confirm payments safely, it credits
  nothing and publishes that on chain, so clients withhold the payment
  address and say why.

## Consequences

### Positive

- No custody of anyone's money, at any point.
- The rules of every sale are on chain and checkable.

### Tradeoffs

- The oracle's assertion that a payment happened is trusted for that sale;
  that trust boundary is explicit.
- Litecoin's price moves; the credit price is set in credits per LTC.

## Implementation

### Where it lives

- `core-crypto/src/pallets/nai.rs`, `pallets/market.rs`.
- `core-crypto/src/payment_memo.rs`: the memo format both ends share.

## References

- `docs/ECONOMY.md`.

## History

- v1.0 (2026-10-10): Recorded retroactively for the first public release.
