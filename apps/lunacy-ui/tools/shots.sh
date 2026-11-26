#!/usr/bin/env bash
# Renders every screen in both themes into `screenshots/`, plus scripted interaction shots.
#
#   ./tools/shots.sh                 # everything
#   ./tools/shots.sh settings-step   # one scene, both themes
#
# Headless GPU: the software Vulkan ICD, because this box has no /dev/dri.
set -euo pipefail
cd "$(dirname "$0")/.."

export VK_ICD_FILENAMES=${VK_ICD_FILENAMES:-/home/tretrauit/Android/Sdk/emulator/lib64/vulkan/lvp_icd.json}
mkdir -p screenshots

SIZE=${SIZE:-1180x760}
ONLY=${1:-}

# Scenes that are worth a full-size capture in both themes.
SCENES=(
  projects
  project
  run
  run-finished
  run-failed
  settings-general
  settings-server
  settings-github
  settings-discord
  settings-act
  settings-projects
  settings-step
  settings-rejected
  settings-diagnostics
  settings-connection
  offline
  unauthorized
  toast
)

shoot() {
  local scene=$1 theme=$2 out=$3
  cargo run -q -- --snap "$out" --scene "$scene" --theme "$theme" --size "$SIZE" >/dev/null 2>&1 \
    || { echo "FAIL $scene $theme"; return 0; }
  echo "  $out"
}

if [ -n "$ONLY" ]; then
  for theme in light dark; do shoot "$ONLY" "$theme" "screenshots/${ONLY}-${theme}.png"; done
else
  for scene in "${SCENES[@]}"; do
    for theme in light dark; do shoot "$scene" "$theme" "screenshots/${scene}-${theme}.png"; done
  done
fi

# A scripted pass: keyboard focus rings, the settings Cmd+S, and the run view scrolled to its log.
script_shots() {
  local dir=screenshots
  # Keyboard only: Tab from the first frame, so focus rings and the accessible nav show.
  cat > /tmp/keys.json <<'JSON'
[
  {"t": 0.2, "type": "key", "name": "Tab"},
  {"t": 0.4, "type": "key", "name": "Tab"},
  {"t": 0.6, "type": "key", "name": "Tab"}
]
JSON
  for theme in light dark; do
    cargo run -q -- --snap "$dir/keys-focus-${theme}.png" --scene projects --theme "$theme" \
      --size "$SIZE" --script /tmp/keys.json >/dev/null 2>&1 && echo "  $dir/keys-focus-${theme}.png"
  done
}

script_shots
shopt -s nullglob
shots=(screenshots/*.png)
echo "done: ${#shots[@]} files"
