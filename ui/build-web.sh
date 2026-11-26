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

# The pinned Rune checkout (RUNE_DIR, else cloned once into ~/.cache/lunacy/rune).
RUNE=$(tools/find-rune.sh)

# Absolute: Rune's script runs some steps from another directory (wasm-opt failed on "dist/.next").
args=(lunacy-ui --out "$PWD/dist")
[ "${1:-}" = "--dev" ] && args+=(--dev) || args+=(--opt)
"$RUNE/scripts/web-build.sh" "${args[@]}"
# Our own page: absolute asset paths (deep links), a /runs/{id} redirect, readable errors.
cp web/index.html dist/index.html
cp web/site.webmanifest dist/site.webmanifest
cp ../assets/favicon.svg dist/favicon.svg
cp ../assets/logo.svg dist/logo.svg
cp ../assets/icons/favicon.ico dist/favicon.ico
cp ../assets/icons/apple-touch-icon.png dist/apple-touch-icon.png
cp ../assets/icons/icon-192.png dist/icon-192.png
cp ../assets/icons/icon-512.png dist/icon-512.png
if command -v sha256sum >/dev/null 2>&1; then
  hash=$(cat dist/app_bg.wasm dist/app.js | sha256sum | awk '{print substr($1, 1, 12)}')
elif command -v shasum >/dev/null 2>&1; then
  hash=$(cat dist/app_bg.wasm dist/app.js | shasum -a 256 | awk '{print substr($1, 1, 12)}')
else
  echo "Neither sha256sum nor shasum found to compute build hash." >&2
  exit 1
fi
tmp=$(mktemp)
trap 'rm -f "${tmp:-}"' EXIT
sed "s|/\*__LUNACY_BUILD__\*/|$hash|g" dist/index.html > "$tmp"
cat "$tmp" > dist/index.html
rm -f "$tmp"
