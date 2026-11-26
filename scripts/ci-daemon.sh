#!/usr/bin/env bash
set -Eeuo pipefail

export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/.."

export CARGO_TARGET_DIR="${LUNACY_CARGO_TARGET_ROOT:-$HOME/.local/share/lunacy/cargo-target}/lunacy"

if [ "$(uname -s)" = "Linux" ] && command -v ld.lld >/dev/null 2>&1 && command -v clang >/dev/null 2>&1; then
  case " ${RUSTFLAGS:-} " in
    *"-fuse-ld=lld"*) ;;
    *) export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C linker=clang -C link-arg=-fuse-ld=lld" ;;
  esac
fi

echo "==> [ci-daemon] cargo fmt --check (daemon + core)"
cargo fmt -p lunacy -p lunacy-core --check

echo "==> [ci-daemon] cargo clippy -p lunacy -p lunacy-core --all-targets -- -D warnings"
cargo clippy -p lunacy -p lunacy-core --all-targets -- -D warnings

echo "==> [ci-daemon] cargo test -p lunacy -p lunacy-core --locked"
cargo test -p lunacy -p lunacy-core --locked
