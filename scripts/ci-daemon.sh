#!/usr/bin/env bash
set -Eeuo pipefail

export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/.."

export CARGO_TARGET_DIR="${LUNACY_CARGO_TARGET_ROOT:-$HOME/.local/share/lunacy/cargo-target}/lunacy"

echo "==> [ci-daemon] cargo fmt --check"
cargo fmt --check

echo "==> [ci-daemon] cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets -- -D warnings

echo "==> [ci-daemon] cargo test --locked"
cargo test --locked
