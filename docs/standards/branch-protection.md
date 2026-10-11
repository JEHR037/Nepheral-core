# Branch protection

- **Status:** Accepted
- **Date:** 2026-10-10

`main` is the only long-lived branch, and nothing reaches it without a pull
request that passed every check. The rules are kept as code in
`.github/rulesets/` and applied to the repository with the GitHub API, so
they are reviewed like any other change.

## Rules on `main`

Two rulesets, so that the owner can merge their own work without ever
skipping CI:

**`main: protection`** (`.github/rulesets/main-protection.json`), with no
bypass for anyone:

| Rule | Effect |
|---|---|
| Restrict deletions | `main` cannot be deleted |
| Block force pushes | history on `main` cannot be rewritten |
| Require linear history | no merge commits; squash only |
| Require a pull request | no direct pushes; conversations must be resolved; approvals are dismissed when new commits arrive |
| Require status checks | `Format`, `Lint and test (Linux)`, `Lint (Windows)` and `Dependencies` must pass, on a branch that is up to date with `main` |

**`main: review`** (`.github/rulesets/main-review.json`):

| Rule | Effect |
|---|---|
| One approval | from a code owner, given after the last push |
| Bypass | repository administrators, and only through a pull request |

While the project has a single maintainer, the bypass lets that maintainer
merge their own pull requests once every check is green. When a second
maintainer joins, the bypass is removed and every change needs a review by
someone other than its author.

## Repository settings

- Squash merging only; head branches deleted after merge.
- Secret scanning with push protection: a push that contains a credential
  is refused.
- Dependabot alerts and security updates (`.github/dependabot.yml`, with a
  two-day cooldown on new versions).
- Private vulnerability reporting, so reports reach the maintainers without
  a public issue.
- Code scanning with CodeQL's default setup.

## How to apply them

Once the repository exists on GitHub, an administrator runs, from the
repository root (replace `OWNER/REPO`):

```sh
# Merge settings
gh repo edit OWNER/REPO --enable-squash-merge --enable-merge-commit=false \
  --enable-rebase-merge=false --delete-branch-on-merge

# Rulesets
gh api -X POST repos/OWNER/REPO/rulesets --input .github/rulesets/main-protection.json
gh api -X POST repos/OWNER/REPO/rulesets --input .github/rulesets/main-review.json

# Security features
gh api -X PUT repos/OWNER/REPO/vulnerability-alerts
gh api -X PUT repos/OWNER/REPO/automated-security-fixes
gh api -X PUT repos/OWNER/REPO/private-vulnerability-reporting
gh api -X PATCH repos/OWNER/REPO --input - <<'EOF'
{"security_and_analysis": {
  "secret_scanning": {"status": "enabled"},
  "secret_scanning_push_protection": {"status": "enabled"}}}
EOF
```

The status-check names in `main-protection.json` must match the job names
in `.github/workflows/ci.yml`. Run the workflow once before applying the
ruleset, or the first pull request waits for checks that never report.

To change a rule, edit the JSON in a pull request, then update the ruleset
with `gh api -X PUT repos/OWNER/REPO/rulesets/<id> --input <file>`.
