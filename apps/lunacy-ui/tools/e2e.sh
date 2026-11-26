#!/usr/bin/env bash
# End-to-end verification against a real sandboxed `lunacy` daemon and headless Rune / Chrome.
# Never touches port 7878, ~/.config/lunacy, ~/.local/share/lunacy, systemd units, or apps/lunacy-ui/dist.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
RUNE_DIR=$("$ROOT/apps/lunacy-ui/tools/find-rune.sh")
export RUNE_DIR
if [ -z "${VK_ICD_FILENAMES:-}" ] && [ -f "/home/tretrauit/Android/Sdk/emulator/lib64/vulkan/lvp_icd.json" ]; then
  export VK_ICD_FILENAMES="/home/tretrauit/Android/Sdk/emulator/lib64/vulkan/lvp_icd.json"
fi

cd "$ROOT"

echo "[e2e] Building lunacy daemon + lunacy-ui..."
cargo build --bin lunacy
cargo build -p lunacy-ui --bin lunacy-ui

exec python3 -u "$ROOT/apps/lunacy-ui/tools/e2e/run_e2e.py"
