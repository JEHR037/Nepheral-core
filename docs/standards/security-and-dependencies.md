# Security and dependencies

- **Status:** Accepted
- **Date:** 2026-10-10

## Secrets

- No secret is ever committed: private keys, seeds, tokens, passwords,
  signing keystores and their properties files. `.gitignore` covers the
  usual file types, and secret scanning with push protection is enabled on
  the repository.
- Runtime secrets come from the environment or the vault, never from the
  source.
- A secret that reaches the repository is treated as compromised: rotate it
  first, then remove it from the history.

## Reporting vulnerabilities

Reports follow [SECURITY.md](../../SECURITY.md) and stay private until a fix
is released.

## Vulnerability workflow

1. **Identify.** Sources: RustSec advisories through `cargo deny`,
   Dependabot alerts, code scanning, reports and reviews.
2. **Classify.** Severity (critical, high, medium, low, informational) and
   type (code, dependency, configuration). Severity is judged in context:
   what an attacker could do to a node, a wallet or the network.
3. **Review.** Decide whether it is a true positive. A false positive is
   dismissed with its justification written down.
4. **Resolve.** Upgrade, patch or mitigate. Retest, and add a regression
   test where it makes sense.
5. **Record.** The pull request that fixes it explains the issue; the
   changelog mentions it under `Security` once it is safe to.

## Dependency checks

`just audit` runs `cargo deny check`, and CI runs it on every pull request.
It checks:

- **Advisories:** no dependency with a known vulnerability or that has been
  yanked.
- **Licences:** only licences on the allow list in `deny.toml`.
- **Sources:** crates come from crates.io only, unless an exception is
  recorded.

### Exceptions

An advisory may be ignored only when no fixed version exists, or the fix
needs a major upgrade that the dependent crates cannot take yet. Every
ignore in `deny.toml` carries:

- the advisory id and the crate;
- why the fix cannot be applied;
- which dependency holds it back;
- the date it should be reviewed again.

Exceptions are reviewed at every dependency update and removed as soon as
the fix can be taken.

## New and updated dependencies

- **Cooldown.** Dependabot proposes a new version only after it has been
  published for two days, so a compromised release has time to be noticed
  and yanked. A security fix that cannot wait is applied by hand, pinned to
  the exact patched version, with the advisory in the pull request.
- **Licences:** only the licences allowed by
  [ADR-0018](../adr/0018-dependency-licences.md) may enter the dependency
  tree, transitive dependencies included. `cargo deny` enforces it in CI.
- **Adding a crate** is a design decision when it is large, touches
  cryptography or networking, or replaces something we have, and goes
  through an ADR that lists it with its licence. Prefer maintained crates
  with few transitive dependencies; say why it is needed.
- **Lockfile.** `Cargo.lock` is committed and CI builds with it.
- **CI actions** are pinned to a full commit hash, not a tag.
