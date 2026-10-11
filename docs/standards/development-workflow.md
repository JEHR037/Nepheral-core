# Development workflow

- **Status:** Accepted
- **Date:** 2026-10-10

## Task runner

Common tasks go through [`just`](https://github.com/casey/just), so everyone
and CI run the same commands. `just` with no arguments lists them.

| Command | What it runs |
|---|---|
| `just fmt` | `cargo fmt --all` |
| `just fmt-check` | `cargo fmt --all -- --check` |
| `just lint` | `cargo clippy` with warnings as errors, for the workspace and for the headless node |
| `just test` | `cargo test` for the workspace and for the headless node |
| `just check` | `fmt-check`, `lint` and `test` together |
| `just audit` | `cargo deny check`: advisories, licences, sources |
| `just build` | The desktop client with CPU inference |
| `just build-node` | The headless node with CPU inference |

`lint` and `test` build without local inference, so they need no C++
toolchain. Builds with inference need cmake and clang (see `BUILD.md`).

## Before every commit

1. `just fmt`
2. `just check`
3. Update the documentation and the changelog if behaviour changed.

Commit only when the checks pass.

## Branch lifecycle

1. Update `main` and branch from it: `git switch -c fix/relay-hop-limit`.
2. Commit small and often.
3. Rebase on `main` early and often to keep the branch short-lived.
4. Push the branch and open a pull request (see
   [Git and pull requests](git-and-pull-requests.md)).
5. Address review comments on the same branch.
6. The pull request is squash-merged and the branch is deleted.

## Push policy

- Push freely to your own branches.
- Never push directly to `main`; the repository rules refuse it.
- Force-push only your own branches, with `--force-with-lease`.
- Automated tools, including AI assistants, never push, force-push, publish
  or deploy without the explicit approval of the person running them.

## What runs where

| Locally | In CI | Never from a laptop |
|---|---|---|
| Build, format, lint, tests, local multi-node runs | Format, lint, tests, dependency checks on every pull request and every push to `main` | Signing releases, publishing binaries, operating the live network's servers |

A pull request can only be merged when every required CI check passes.
