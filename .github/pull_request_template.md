## What and why

What this pull request changes and the reason for it. Link the issue, RFC or
ADR it implements.

## How it was tested

Commands run, logs, screenshots, or the multi-node setup used.

## Things to look at

Anything tricky, risky or deliberately left out.

## Checklist

- [ ] I have read the [Contributor License Agreement](../CLA.md) and agree
      to it for this and my later contributions.
- [ ] The title follows Conventional Commits (`type(scope): description`).
- [ ] `just check` passes locally (format, clippy, tests).
- [ ] New behaviour has tests; a fixed bug has the test that would have caught it.
- [ ] The change implements an accepted ADR (or only restores documented
      behaviour, or only changes documentation).
- [ ] Wire-format or chain-rule changes keep older nodes working inside the
      transition window.
- [ ] New dependencies have licences allowed by ADR-0018, and `just audit`
      passes.
- [ ] Documentation and `CHANGELOG.md` (under `Unreleased`) are updated.
- [ ] No secrets, personal paths or commented-out code in the diff.
