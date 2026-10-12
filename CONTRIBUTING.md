# Contributing to Nepheral

Thank you for helping. This page is the short version; the full standards are
in [`docs/standards`](docs/standards/README.md).

## Contributor License Agreement

Every contribution is made under the [Contributor License Agreement](CLA.md).
You keep the copyright in your work and grant the Licensor the right to
publish it under the project's licences, including commercial ones. You
accept the agreement by checking its box in the pull request template; a
pull request without that box checked is not reviewed or merged.

## Security issues

Never report them in public. Follow [SECURITY.md](SECURITY.md).

## The path of a change: document first

Code is the last step, not the first. Every change that alters behaviour,
design, the protocol, the chain rules, a privacy property or the set of
dependencies follows this path:

1. **RFC** ([`docs/rfcs`](docs/rfcs/README.md)), when the change needs
   discussion before anyone decides: the problem, a proposal, the
   alternatives. Skip it when the decision is already clear.
2. **ADR** ([`docs/adr`](docs/adr/README.md)): the decision, the options
   with their benefits and drawbacks, the consequences, and every new
   dependency with its licence. It is reviewed and merged as **Accepted**
   before the implementation is merged.
3. **Code**: a pull request that implements the accepted ADR and links it.
4. **Changelog**: an entry under `Unreleased` in [CHANGELOG.md](CHANGELOG.md).

Only two kinds of change go straight to a pull request: fixes that restore
behaviour already documented, and changes to the documentation itself. If
you are unsure which kind yours is, open an issue first.

## Licences of dependencies

Nepheral is published under the Business Source License 1.1 and converts to
the Apache License 2.0 ([LICENSE](LICENSE)). No ADR, RFC or pull request may
add a library, model or dataset whose licence is incompatible with that
model: no GPL, LGPL or AGPL, no non-commercial or no-derivatives licences,
no other source-available licences. The allowed licences and the reasons are
in [ADR-0018](docs/adr/0018-dependency-licences.md); CI enforces the list
with `cargo deny`.

Knowledge maps are the one exception. They are separate works, downloaded
on request and never built in, so their content may also come from sources
under CC BY-SA, the GFDL or the GPL, each kept under its own licence with
its attribution. Non-commercial and no-derivatives licences stay out there
too. The rules are in [ADR-0020](docs/adr/0020-licences-for-knowledge-map-content.md).

## Workflow

1. Branch from `main` with a short, typed name: `feat/light-sync-proofs`,
   `fix/relay-hop-limit`, `docs/adr-0019`.
2. Make small commits. Run the checks locally before you push:

   ```sh
   just check      # format check, clippy with warnings as errors, tests
   just audit      # advisories, licences and sources of every dependency
   ```

   Without [`just`](https://github.com/casey/just), run the commands in the
   [`justfile`](justfile) by hand.
3. Open a pull request against `main`, as a draft while it is in progress.
   Its title follows [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/)
   (`fix(relay): cap hops at two`), because it becomes the squashed commit.
4. Fill in the template: what changed, why, how you tested it, and the ADR
   it implements.
5. A maintainer reviews. Answer every comment; push fixes to the same
   branch. The pull request is merged when CI is green and it is approved.

## Ground rules

- You are responsible for every line you submit, however it was produced.
  Do not submit code you cannot explain.
- English in code, comments, commit messages and documentation. User-facing
  text goes through the translation files.
- No emojis in code, commits or documentation.
- No commented-out code; the history keeps old versions.
- New behaviour comes with tests. Changes to the wire format update the
  golden tests in `mesh-network/tests/golden_wire.rs` on purpose, never by
  accident.
