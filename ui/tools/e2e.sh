#!/usr/bin/env bash
# End-to-end verification against a real sandboxed `lunacy` daemon and headless Rune / Chrome.
# Never touches port 7878, ~/.config/lunacy, ~/.local/share/lunacy, systemd units, or ui/dist.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
export RUNE_DIR="${RUNE_DIR:-/tmp/pi-github-repos/runtime-vsiOuE/c8125861ff163c50577a9cb425c53259a4411a18295dcf09b24a2d2066406eb9}"
export VK_ICD_FILENAMES="${VK_ICD_FILENAMES:-/home/tretrauit/Android/Sdk/emulator/lib64/vulkan/lvp_icd.json}"

cd "$ROOT"

echo "[e2e] Building lunacy daemon + lunacy-ui..."
cargo build --bin lunacy
cargo build --manifest-path ui/Cargo.toml --bin lunacy-ui

exec python3 -u "$ROOT/ui/tools/e2e/run_e2e.py"
