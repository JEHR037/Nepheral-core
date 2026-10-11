# Documentation

- **Status:** Accepted
- **Date:** 2026-10-10

## Where things are written

| What | Where |
|---|---|
| What the project is, how to build and run it | `README.md`, `BUILD.md` |
| How to contribute | `CONTRIBUTING.md`, `docs/standards/` |
| How the system works | `docs/` (whitepaper, economy, governance and the other design documents) |
| Why it works that way | `docs/adr/` |
| Proposals under discussion | `docs/rfcs/` |
| What changed in each version | `CHANGELOG.md` |
| How to report a vulnerability | `SECURITY.md` |
| Each crate's purpose and entry points | the crate's `README.md` and its module comments |

## Rules

- English everywhere in the repository. User-facing text is translated
  through the translation files, not written into the code.
- No emojis.
- Update the documentation in the same pull request as the change it
  describes: new features, changed behaviour, new dependencies, new
  configuration or environment variables, new directories.
- Every user-visible change gets a line under `Unreleased` in
  `CHANGELOG.md`, in the section that fits (Added, Changed, Deprecated,
  Removed, Fixed, Security).
- Design documents describe the system as it is. When the design changes,
  update the document and record the decision in an ADR.
- Diagrams are welcome; keep their source in the repository next to the
  document.

## README of a crate

Each crate's README says:

- what the crate is for and what it must never do;
- its main types and entry points;
- how to run its tests, and any test that needs a model or hardware.
