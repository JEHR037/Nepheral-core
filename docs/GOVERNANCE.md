# Governance

Nepheral starts with a founder and is built to keep running without one.
This document describes who can change what on chain, what happens when
the founder resigns, and why the nodes that run the network are not
affected by it.

## Two things kept apart: titles and roles

**Titles** give power over the network: admitting and removing nodes,
moving the economic levers. **Roles** are what a node does: serving
Top-Tier channels and producing blocks. Resigning ends titles. It never
touches roles.

## Founder 0

The genesis authority key. While active it is the only key that can:

- admit founding nodes (`Authority::Add`) and remove them
  (`Authority::Remove`);
- change the economic levers: sale oracle, direct sale wallet, daily mint
  cap, channel session costs and the emission target;
- record shares and consumption on behalf of peers;
- resign (`Authority::Dissolve`).

Nobody can remove founder 0. There is no vote, supermajority or other
call that ends its title against its will. Governance cannot propose
membership changes while founder 0 is active.

## Founding nodes

Top-Tier and business operators admitted by founder 0. Each one declares
the channels it serves and produces blocks in its own slots, signing with
its mesh identity. A founding node can change its own channels, rotate its
own key and resign. It cannot add anyone, remove anyone else, touch the
economy or resign on behalf of founder 0.

## Resignation

`Authority::Dissolve` is one-way and belongs to founder 0 alone. When it
lands:

- every founder title ends at once, founder 0 included; no key keeps the
  power to admit or remove nodes or to move the economy;
- every founding node keeps its role exactly as before: the same channels,
  the same place in the block production schedule, the same inference
  traffic. The node does not need to be restarted or reconfigured;
- the economic levers and the node set change only through enacted
  proposals from then on.

The chain refuses the resignation until governance has replaced the sale
oracle by vote at least once. Resigning earlier would freeze the sale
oracle forever, and if that key were then lost or compromised, direct
sales would break with no way to repair them.

The headless node submits the resignation only when started with
`NEPHERAL_DISSOLVE_AUTHORITY=dissolve-founder-authority`, and only once
the condition above holds.

## Block production without a founder

Block producers are the nodes in the on-chain authority set, read from
chain state at every block, not a list fixed at genesis. Slots rotate
among them (`slot mod n`). A node that is offline leaves its slots empty
and the next producer continues, so the chain runs as long as one
producer is online. Each block must use a slot later than its parent.

Forks are resolved by the longest valid chain above the last finalised
block. A node never reorganises more than 64 blocks deep; to make that
cheap it keeps in-memory state checkpoints every 16 blocks. Orphaned
transactions are returned to the pending pool.

## Finality

Every founding node is also a finality voter. Each time the chain passes a
multiple of 16 blocks, every validator signs "final up to height H with
hash X" and gossips the vote. When two thirds plus one of the validator
set have signed the same block, any producer includes the justification
on chain (`Finality::Finalize`), and from then on:

- no node reorganises below the finalised height, whatever fork it is
  offered; the 64-block limit remains only as a backstop for the blocks
  after it;
- nodes also refuse such forks as soon as they have seen the votes, before
  the justification is included.

A justification is checked against the validator set that was in force
at the height it finalises, not the current one. The chain keeps the
history of set changes until they fall below the finalised height, so a
node joining or leaving between the vote and its inclusion does not
invalidate it.

Production does not wait for finality. With 1 validator finality is
immediate, with 2 or 3 it needs every one of them, and from 4
validators it tolerates one offline or malicious node (3f+1). A lagging
finality never stops the chain; it only delays how far back a block
becomes irreversible.

A validator that signs two different blocks at the same height has
produced a verifiable equivocation proof. Any node that sees both votes
submits `Finality::ReportEquivocation`, and the chain removes the offender
from the validator set automatically. Founder 0 cannot be removed; an
equivocation by founder 0 is recorded on chain for everyone to see.
Validators remember the last height they voted for on disk, so a restart
cannot make an honest node equivocate.

## Voters and proposals

Every identity with recorded contribution (custodied shares and answered
queries) first seen at least 7 days ago can vote. Voting weight is the
contribution maturity recorded on chain, not the balance, so buying
credits buys no votes.

| Action | When |
|---|---|
| `SetSaleOracle` | Always |
| `SetDirectSaleWallet` | Always |
| `SetDailyMintCap` | Always |
| `SetChannelSessionCost` | Always |
| `SetEmissionTarget` | Always |
| `AddPartner` / `RemovePartner` | Only after founder 0 has resigned |

Lifecycle:

1. **Propose.** The electorate weight is frozen at creation. Each voter may
   keep at most 2 proposals open; the chain holds at most 32 open ones.
2. **Vote.** Open for 20,000 blocks. One vote per identity, weighted at the
   moment it is cast; it cannot be changed.
3. **Delay.** 20,000 more blocks during which nothing happens, so every
   operator sees what is coming before it lands.
4. **Enact.** Anyone can submit `Enact`. A proposal passes with two thirds
   of the weight cast and participation of at least 20% of the
   electorate. A proposal that passes but cannot be applied is recorded as
   failed; the block is not rejected.

While founder 0 is active it can also move the economic levers directly;
votes on them are how governance proves it can run the network before the
founder leaves.

All numbers are constants in `core-crypto/src/pallets/governance.rs` and
change only with a new chain generation.

## What governance does not cover

The system prompt. Nodes do not accept a system prompt from the network;
each operator sets its own, so a vote cannot inject instructions into
every node. Answer quality is handled by ratings instead, described in
`docs/REPUTATION.md`.
