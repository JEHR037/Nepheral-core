# Git and pull requests

- **Status:** Accepted
- **Date:** 2026-10-10

## Branches

- One protected default branch, `main`. Every change reaches it through a
  pull request.
- Branch names: `<type>/<short-description>`, lower case, words joined by
  hyphens; add the issue number when there is one: `fix/123-relay-hop-limit`.
- Keep branches short-lived and narrow in scope.

## Commit messages

[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/):

```
<type>(<scope>): <short description, imperative, 50 characters at most>

Why the change was needed and how it differs from the previous behaviour,
wrapped at 72 characters.

- Bullet points for separate changes
- Refs: #123
```

- Types: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`,
  `chore`. A breaking change adds `!` after the type or a
  `BREAKING CHANGE:` footer.
- Scopes are crate or area names: `chain`, `mesh`, `agent`, `relay`,
  `desktop`, `api`, `recall`, `economy`.
- Required for pull request titles, since the title becomes the squashed
  commit; recommended for every commit.
- No emojis or decorative characters.

## Pull requests

### Settings of the repository

- Squash merge only; the branch is deleted on merge.
- No direct pushes to `main`.
- Required checks must pass; conversations must be resolved.

### Authors

- Check the box of the [Contributor License Agreement](../../CLA.md) in the
  pull request template; without it the pull request is not reviewed.
- Open the pull request as a draft until it is ready.
- One concern per pull request. If it grows large, split it.
- The description says what changed and why, how it was tested (commands,
  logs, screenshots), and anything tricky a reviewer should look at.
- Before asking for review: `just check` passes, CI is green, you have read
  your own diff for leftovers (debug output, commented-out code, stray
  files), and the changelog has an entry if users will notice.
- Avoid excessive comments. Generated code and comments get the same
  critical read as anything else.
- You own every line you submit, whatever tool produced it.

### Reviewers

- Start from the issue or the ADR: does the change do what was asked, and
  only that?
- Do not approve what you do not understand. Ask, or bring in someone who
  knows the area (see `.github/CODEOWNERS`).
- Check the tests as carefully as the code: a test that cannot fail proves
  nothing.
- Check that the change implements an accepted ADR when it alters
  behaviour or design, and that every new dependency has a licence allowed
  by ADR-0018.
- For anything touching the wire protocol, the chain rules or a privacy
  property, check that an ADR covers it and that older nodes keep working
  within the transition window.
