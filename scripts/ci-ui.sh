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

echo "==> [ci-ui] cargo fmt -p lunacy-ui --check"
cargo fmt -p lunacy-ui --check

echo "==> [ci-ui] cargo clippy -p lunacy-ui --all-targets -- -D warnings"
RUSTFLAGS="$NATIVE_RUSTFLAGS" cargo clippy -p lunacy-ui --all-targets -- -D warnings

echo "==> [ci-ui] cargo test -p lunacy-ui --locked"
RUSTFLAGS="$NATIVE_RUSTFLAGS" cargo test -p lunacy-ui --locked

echo "==> [ci-ui] cargo check -p lunacy-ui --target wasm32-unknown-unknown --locked"
cargo check -p lunacy-ui --target wasm32-unknown-unknown --locked
