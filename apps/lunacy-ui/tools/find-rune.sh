#!/usr/bin/env bash
# Prints a Rune checkout with scripts/web-build.sh at the rev apps/lunacy-ui/Cargo.toml pins.
# RUNE_DIR wins; else ~/.cache/lunacy/rune (cloned there on first use, outside /tmp so it
# survives reboots). Sourced by build-web.sh and e2e.sh.
set -euo pipefail

if [ -n "${RUNE_DIR:-}" ] && [ -x "$RUNE_DIR/scripts/web-build.sh" ]; then
  echo "$RUNE_DIR"
  exit 0
fi
ui="$(cd "$(dirname "$0")/.." && pwd)"
rev=$(sed -n 's/.*anomalyco\/rune", rev = "\([0-9a-f]\{40\}\)".*/\1/p' "$ui/Cargo.toml" | head -1)
[ -n "$rev" ] || { echo "find-rune: no pinned Rune rev in $ui/Cargo.toml" >&2; exit 1; }
dir="${RUNE_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/lunacy/rune}"
if [ ! -d "$dir/.git" ]; then
  mkdir -p "$(dirname "$dir")"
  git clone -q --filter=blob:none https://github.com/anomalyco/rune "$dir" >&2
fi
if [ "$(git -C "$dir" rev-parse HEAD)" != "$rev" ]; then
  git -C "$dir" fetch -q origin "$rev" >&2 || git -C "$dir" fetch -q origin >&2
  git -C "$dir" checkout -q --detach "$rev" >&2
fi
echo "$dir"
