#!/usr/bin/env bash
set -Eeuo pipefail

export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/.."

export CARGO_TARGET_DIR="${LUNACY_CARGO_TARGET_ROOT:-$HOME/.local/share/lunacy/cargo-target}/lunacy-ui"

NATIVE_RUSTFLAGS="${RUSTFLAGS:-}"
if [ "$(uname -s)" = "Linux" ] && command -v ld.lld >/dev/null 2>&1 && command -v clang >/dev/null 2>&1; then
  case " $NATIVE_RUSTFLAGS " in
    *"-fuse-ld=lld"*) ;;
    *) NATIVE_RUSTFLAGS="${NATIVE_RUSTFLAGS:+$NATIVE_RUSTFLAGS }-C linker=clang -C link-arg=-fuse-ld=lld" ;;
  esac
fi

echo "==> [ci-ui] cargo fmt --manifest-path ui/Cargo.toml --check"
cargo fmt --manifest-path ui/Cargo.toml --check

echo "==> [ci-ui] cargo clippy --manifest-path ui/Cargo.toml --all-targets -- -D warnings"
RUSTFLAGS="$NATIVE_RUSTFLAGS" cargo clippy --manifest-path ui/Cargo.toml --all-targets -- -D warnings

echo "==> [ci-ui] cargo test --manifest-path ui/Cargo.toml --locked"
RUSTFLAGS="$NATIVE_RUSTFLAGS" cargo test --manifest-path ui/Cargo.toml --locked

echo "==> [ci-ui] cargo check --manifest-path ui/Cargo.toml --target wasm32-unknown-unknown --locked"
cargo check --manifest-path ui/Cargo.toml --target wasm32-unknown-unknown --locked
