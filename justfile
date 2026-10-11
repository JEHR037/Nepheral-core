# Common tasks. Install just with `cargo install just`, then run `just` to
# list the recipes.

set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# List the recipes.
default:
    @just --list

# Format the code.
fmt:
    cargo fmt --all

# Check formatting without changing files.
fmt-check:
    cargo fmt --all -- --check

# Clippy with warnings as errors: the workspace without local inference, then the headless node.
lint:
    cargo clippy --workspace --all-targets --no-default-features -- -D warnings
    cargo clippy -p app-kernel --features node_only --all-targets -- -D warnings

# Tests: the workspace without local inference, then the headless node.
test:
    cargo test --workspace --no-default-features
    cargo test -p app-kernel --features node_only

# Everything a pull request must pass.
check: fmt-check lint test

# Advisories, licences and sources of every dependency (needs cargo-deny).
audit:
    cargo deny check

# The desktop client with CPU inference (needs cmake and clang).
build:
    cargo build --release -p nepheral-desktop

# The headless node with CPU inference (needs cmake and clang).
build-node:
    cargo build --release -p app-kernel --features node_only_local --bin nepheral-node
