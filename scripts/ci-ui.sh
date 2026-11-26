#!/usr/bin/env bash
set -Eeuo pipefail

export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/.."

export CARGO_TARGET_DIR="${LUNACY_CARGO_TARGET_ROOT:-$HOME/.local/share/lunacy/cargo-target}/lunacy-ui"

echo "==> [ci-ui] cargo fmt --manifest-path ui/Cargo.toml --check"
cargo fmt --manifest-path ui/Cargo.toml --check

echo "==> [ci-ui] cargo clippy --manifest-path ui/Cargo.toml --all-targets -- -D warnings"
cargo clippy --manifest-path ui/Cargo.toml --all-targets -- -D warnings

echo "==> [ci-ui] cargo test --manifest-path ui/Cargo.toml --locked"
cargo test --manifest-path ui/Cargo.toml --locked

echo "==> [ci-ui] cargo check --manifest-path ui/Cargo.toml --target wasm32-unknown-unknown --locked"
cargo check --manifest-path ui/Cargo.toml --target wasm32-unknown-unknown --locked
