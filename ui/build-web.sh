#!/usr/bin/env bash
# Builds the web client into `ui/dist/`, ready to serve over HTTP.
#
#   ./build-web.sh            # optimised
#   ./build-web.sh --dev      # faster, larger
#
# Requires: rustup target add wasm32-unknown-unknown, and wasm-bindgen-cli whose version matches
# the wasm-bindgen crate in Cargo.lock (the build fails loudly when they disagree).
set -euo pipefail
cd "$(dirname "$0")"

RUNE=${RUNE_DIR:-}
if [ -z "$RUNE" ]; then
  # The pinned Rune checkout: reuse a local clone at the locked rev, else fetch it.
  rev=$(sed -n 's/.*rev = "\([0-9a-f]\{40\}\)".*/\1/p' Cargo.toml | head -1)
  # A cargo git checkout keeps the repo under <name>/<hash>/, so look for the script itself.
  for base in "${RUNE_CACHE:-$HOME/.cargo/git/checkouts}" "${TMPDIR:-/tmp}"; do
    [ -d "$base" ] || continue
    RUNE=$(find "$base" -maxdepth 8 -path '*/scripts/web-build.sh' -printf '%h\n' 2>/dev/null | head -1)
    [ -n "$RUNE" ] && break
  done
  RUNE=${RUNE%/scripts}
  if [ -z "$RUNE" ] || [ ! -x "$RUNE/scripts/web-build.sh" ]; then
    echo "No Rune checkout found. Set RUNE_DIR to a checkout of github.com/anomalyco/rune at $rev." >&2
    exit 1
  fi
fi

args=(lunacy-ui --out dist)
[ "${1:-}" = "--dev" ] && args+=(--dev) || args+=(--opt)
"$RUNE/scripts/web-build.sh" "${args[@]}"
# Our own page: absolute asset paths (deep links), a /runs/{id} redirect, readable errors.
cp web/index.html dist/index.html
