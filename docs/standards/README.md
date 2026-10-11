# Engineering standards

How work is done in this repository. Each standard is short on purpose; the
reasons behind the larger ones are recorded as [ADRs](../adr/README.md).

| Standard | What it covers |
|---|---|
| [Development workflow](development-workflow.md) | Task runner, local checks, branch lifecycle, push policy, what CI does |
| [Git and pull requests](git-and-pull-requests.md) | Branch names, commit messages, pull request content, reviews |
| [Code and testing](code-and-testing.md) | Rust conventions, errors, comments, where tests live |
| [Security and dependencies](security-and-dependencies.md) | Secrets, vulnerabilities, advisories, new dependencies |
| [Documentation](documentation.md) | README, design documents, changelog, language |
| [Branch protection](branch-protection.md) | Repository rules for `main` and how they are applied |

## Principles

- **Keep it simple.** Add complexity when a real need appears, not before.
- **One thing at a time.** Finish a change before starting the next; small
  pull requests are reviewed better and faster.
- **Prefer established crates** over writing our own, unless there is a
  reason recorded in an ADR. Every utility we write is ours to maintain.
- **Fail loudly on the unexpected.** Handle the errors you expect; let an
  impossible state stop the process instead of hiding it.
- **Readable over clever.** The next reader may be auditing the privacy
  claims; make that easy.
