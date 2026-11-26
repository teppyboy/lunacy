#!/usr/bin/env bash
# End-to-end verification against a real sandboxed `lunacy` daemon and headless Rune / Chrome.
# Never touches port 7878, ~/.config/lunacy, ~/.local/share/lunacy, systemd units, or ui/dist.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
if [ -z "${RUNE_DIR:-}" ] || [ ! -x "${RUNE_DIR}/scripts/web-build.sh" ]; then
  found=""
  for base in "${RUNE_CACHE:-$HOME/.cargo/git/checkouts}" "${TMPDIR:-/tmp}"; do
    [ -d "$base" ] || continue
    found=$(find "$base" -maxdepth 8 -path '*/scripts/web-build.sh' -printf '%h\n' 2>/dev/null | head -1)
    [ -n "$found" ] && break
  done
  RUNE_DIR="${found%/scripts}"
fi
if [ -z "${RUNE_DIR:-}" ] || [ ! -x "${RUNE_DIR}/scripts/web-build.sh" ]; then
  echo "[e2e] No Rune checkout found. Set RUNE_DIR to a Rune checkout." >&2
  exit 1
fi
export RUNE_DIR
if [ -z "${VK_ICD_FILENAMES:-}" ] && [ -f "/home/tretrauit/Android/Sdk/emulator/lib64/vulkan/lvp_icd.json" ]; then
  export VK_ICD_FILENAMES="/home/tretrauit/Android/Sdk/emulator/lib64/vulkan/lvp_icd.json"
fi

cd "$ROOT"

echo "[e2e] Building lunacy daemon + lunacy-ui..."
cargo build --bin lunacy
cargo build --manifest-path ui/Cargo.toml --bin lunacy-ui

exec python3 -u "$ROOT/ui/tools/e2e/run_e2e.py"
