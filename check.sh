#!/usr/bin/env bash
# one command for the whole quality gate; run before every commit
set -euo pipefail
cd "$(dirname "$0")"

cargo fmt --all
# --workspace is not optional here: the root manifest is both a package and a workspace, so a
# bare clippy checks the kairo package alone and silently skips the four library crates.
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
