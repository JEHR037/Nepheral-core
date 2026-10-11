# Answer ratings

The person who asked is the only one who knows whether an answer helped.
Nepheral lets them say so, on chain, and uses it to decide who gets asked
next. Ratings never mint or burn credits.

## Who can rate what

- Only the sender of a query can rate its answer, and only once.
- The answer must be backed by a service receipt that landed on chain:
  the responder signed the work and the sender countersigned it. Ratings
  without a real, paid-for exchange are impossible.
- The window closes 43,200 blocks after the receipt lands.

## Like and justified dislike

A like needs nothing else. A dislike needs a reason (incorrect, harmful,
off topic, incomplete, refused without reason) and a note of 8 to 280
characters. The note is public on chain, so the interface warns against
pasting private data. A dislike weighs twice as much as a like.

## Weight and decay

Each rating counts `isqrt(maturity)` of the rater, between 1 and 32, where
maturity is the contribution the rater has recorded on chain. A fresh
identity counts 1; a long-standing contributor counts up to 32. Cheap
identities therefore cannot outvote real users.

A rater counts at most once per responder per day. Further ratings of
the same responder that day are recorded as rated but add no weight, so a
ring of identities cannot pile up likes for one another.

Ratings are stored per day (43,200 blocks) and halve with every day of
age; after 8 days they no longer count. A node that improves recovers.

Quality is `(likes + 20) / (likes + 2 * dislikes + 40)`, so a node with no
ratings sits at 50% and a handful of votes cannot swing it far.

## Effect on routing

When a sender builds the candidate list for a query:

- a node whose quality is below 25% with at least 40 weighted votes behind
  it is left out, unless that would leave no candidate at all;
- every remaining candidate gets a lottery weight equal to its quality
  (1 to 100). The weights travel inside the signed `SharePush`, and
  custodians use them to verify which node is entitled to each rank, so
  the per-message lottery stays verifiable.

Better answers are asked more often; bad ones fade out of the rotation.
Emission is untouched: a node still earns only for work it actually did.
