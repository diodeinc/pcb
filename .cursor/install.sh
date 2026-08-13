#!/usr/bin/env bash
# Repository bootstrap for the pcb Cloud Agent environment.
#
# Runs after the repository is checked out. It is idempotent and only prepares
# source-derived state: the pinned Rust toolchain/components, Python dev deps,
# and a warm Cargo build cache. Heavy, repo-independent tooling (KiCad, rustup,
# cargo-nextest, uv) is installed in .cursor/Dockerfile.
set -euo pipefail

cd "$(dirname "$0")/.."

# Materialize the Rust toolchain pinned by rust-toolchain.toml and ensure the
# lint/format components exist for it.
rustup show active-toolchain || rustup show
rustup component add clippy rustfmt

# Python dev/test environment for the layout-lens scripts.
uv sync --locked

# Warm the Cargo caches so agents (and CI-style checks) start fast.
cargo fetch --locked
cargo build -p pcb -p pcbc --locked
