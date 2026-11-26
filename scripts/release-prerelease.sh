#!/usr/bin/env bash
set -Eeuo pipefail

export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/.."

DRY_RUN="${LUNACY_RELEASE_DRY_RUN:-0}"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run)
      DRY_RUN=1
      ;;
    -h|--help)
      cat <<'EOF'
Usage: scripts/release-prerelease.sh [--dry-run]

Builds Linux (x86_64), Windows (x86_64 via cargo-xwin), and Web (wasm32)
artifacts for Lunacy, packages them with SHA256SUMS.txt, and publishes them
to a rolling GitHub pre-release.

Options:
  --dry-run    Build, package, validate, and print release notes without
               uploading to GitHub or syncing LUNACY_SYNC_LOCAL_WEB_DIR.

Environment:
  LUNACY_RELEASE_DRY_RUN=1      Same as --dry-run
  LUNACY_GITHUB_REPO            GitHub owner/repo (default: teppyboy/lunacy)
  LUNACY_RELEASE_TAG            Rolling release tag (default: prerelease)
  LUNACY_SHA                    Commit SHA to tag/describe (default: git HEAD)
  LUNACY_CARGO_TARGET_ROOT      Persistent Cargo target root
                                (default: ~/.local/share/lunacy/cargo-target)
  LUNACY_SYNC_LOCAL_WEB_DIR     Optional local dir to sync ui/dist/ into
EOF
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done

is_dry_run() {
  case "$DRY_RUN" in
    1|true|TRUE|yes|YES) return 0 ;;
    *) return 1 ;;
  esac
}

REPO="${LUNACY_GITHUB_REPO:-teppyboy/lunacy}"
TAG="${LUNACY_RELEASE_TAG:-prerelease}"
SHA="${LUNACY_SHA:-$(git rev-parse HEAD)}"
export LUNACY_SHA="$SHA"
SHORT_SHA="${SHA:0:7}"
TARGET_ROOT="${LUNACY_CARGO_TARGET_ROOT:-$HOME/.local/share/lunacy/cargo-target}"
DAEMON_TARGET="$TARGET_ROOT/lunacy"
UI_TARGET="$TARGET_ROOT/lunacy-ui"

for cmd in cargo cargo-xwin git python3 tar sha256sum; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "error: required command not found on PATH: $cmd" >&2
    exit 1
  fi
done
if ! is_dry_run && ! command -v gh >/dev/null 2>&1; then
  echo "error: required command not found on PATH: gh" >&2
  exit 1
fi

COMMIT_SUBJECT="$(git log -1 --pretty=%s "$SHA")"
TIMESTAMP_UTC="$(date -u +"%Y-%m-%d %H:%M:%S UTC")"

NATIVE_RUSTFLAGS="${RUSTFLAGS:-}"
if [ "$(uname -s)" = "Linux" ] && command -v ld.lld >/dev/null 2>&1 && command -v clang >/dev/null 2>&1; then
  case " $NATIVE_RUSTFLAGS " in
    *"-fuse-ld=lld"*) ;;
    *) NATIVE_RUSTFLAGS="${NATIVE_RUSTFLAGS:+$NATIVE_RUSTFLAGS }-C linker=clang -C link-arg=-fuse-ld=lld" ;;
  esac
fi

echo "==> [release] Building Linux x86_64 binaries"
RUSTFLAGS="$NATIVE_RUSTFLAGS" CARGO_TARGET_DIR="$DAEMON_TARGET" cargo build --release --locked --bin lunacy
RUSTFLAGS="$NATIVE_RUSTFLAGS" CARGO_TARGET_DIR="$UI_TARGET" cargo build --manifest-path ui/Cargo.toml --release --locked --bin lunacy-ui

LINUX_DAEMON_BIN="$DAEMON_TARGET/release/lunacy"
LINUX_UI_BIN="$UI_TARGET/release/lunacy-ui"
for bin in "$LINUX_DAEMON_BIN" "$LINUX_UI_BIN"; do
  if [ ! -s "$bin" ]; then
    echo "error: expected Linux binary missing or empty: $bin" >&2
    exit 1
  fi
done

echo "==> [release] Building Web WASM bundle"
CARGO_TARGET_DIR="$UI_TARGET" ./ui/build-web.sh

for web_file in \
  ui/dist/index.html \
  ui/dist/app.js \
  ui/dist/app_bg.wasm \
  ui/dist/favicon.svg \
  ui/dist/favicon.ico \
  ui/dist/apple-touch-icon.png \
  ui/dist/icon-192.png \
  ui/dist/icon-512.png \
  ui/dist/site.webmanifest; do
  if [ ! -s "$web_file" ]; then
    echo "error: expected Web bundle file missing or empty: $web_file" >&2
    exit 1
  fi
done

echo "==> [release] Building Windows x86_64 binaries via cargo-xwin"
CARGO_TARGET_DIR="$DAEMON_TARGET" cargo xwin build --release --locked --target x86_64-pc-windows-msvc --bin lunacy
CARGO_TARGET_DIR="$UI_TARGET" cargo xwin build --manifest-path ui/Cargo.toml --release --locked --target x86_64-pc-windows-msvc --bin lunacy-ui

WIN_DAEMON_BIN="$DAEMON_TARGET/x86_64-pc-windows-msvc/release/lunacy.exe"
WIN_UI_BIN="$UI_TARGET/x86_64-pc-windows-msvc/release/lunacy-ui.exe"
for bin in "$WIN_DAEMON_BIN" "$WIN_UI_BIN"; do
  if [ ! -s "$bin" ]; then
    echo "error: expected Windows binary missing or empty: $bin" >&2
    exit 1
  fi
done

echo "==> [release] Packaging archives"
stage="$(mktemp -d)"
trap 'rm -rf "${stage:-}"' EXIT

pkg_root="$stage/pkg"
out="$stage/artifacts"
mkdir -p "$pkg_root" "$out"

# 1. Linux x86_64 tarball
linux_dir="$pkg_root/lunacy-linux-x86_64"
mkdir -p "$linux_dir/web" "$linux_dir/contrib" "$linux_dir/icons/hicolor/scalable/apps"
install -m 0755 "$LINUX_DAEMON_BIN" "$linux_dir/lunacy"
install -m 0755 "$LINUX_UI_BIN" "$linux_dir/lunacy-ui"
cp -a ui/dist/. "$linux_dir/web/"
install -m 0644 config.example.toml "$linux_dir/config.example.toml"
install -m 0644 README.md "$linux_dir/README.md"
install -m 0644 contrib/lunacy.service "$linux_dir/contrib/lunacy.service"
install -m 0644 contrib/lunacy.desktop "$linux_dir/contrib/lunacy.desktop"
install -m 0644 assets/icons/lunacy.svg "$linux_dir/icons/hicolor/scalable/apps/lunacy.svg"
for sz in 16 32 48 64 128 256 512; do
  mkdir -p "$linux_dir/icons/hicolor/${sz}x${sz}/apps"
  install -m 0644 "assets/icons/lunacy-${sz}.png" "$linux_dir/icons/hicolor/${sz}x${sz}/apps/lunacy.png"
done
tar -czf "$out/lunacy-linux-x86_64.tar.gz" -C "$pkg_root" lunacy-linux-x86_64

# 2. Windows x86_64 zip (via Python stdlib zipfile — no `zip` CLI needed)
win_dir="$pkg_root/lunacy-windows-x86_64"
mkdir -p "$win_dir/web"
install -m 0755 "$WIN_DAEMON_BIN" "$win_dir/lunacy.exe"
install -m 0755 "$WIN_UI_BIN" "$win_dir/lunacy-ui.exe"
cp -a ui/dist/. "$win_dir/web/"
install -m 0644 config.example.toml "$win_dir/config.example.toml"
install -m 0644 README.md "$win_dir/README.md"

python3 - "$pkg_root" "lunacy-windows-x86_64" "$out/lunacy-windows-x86_64.zip" <<'PY'
import os
import sys
import zipfile

base_dir, top_dir, zip_path = sys.argv[1], sys.argv[2], sys.argv[3]
src_root = os.path.join(base_dir, top_dir)

with zipfile.ZipFile(zip_path, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as zf:
    for root, dirs, files in os.walk(src_root):
        dirs.sort()
        files.sort()
        for filename in files:
            full_path = os.path.join(root, filename)
            arcname = os.path.relpath(full_path, base_dir).replace(os.sep, "/")
            zf.write(full_path, arcname)

with zipfile.ZipFile(zip_path, "r") as zf:
    bad = zf.testzip()
    if bad is not None:
        raise SystemExit(f"error: corrupt zip member: {bad}")
    names = set(zf.namelist())
    required = {
        f"{top_dir}/lunacy.exe",
        f"{top_dir}/lunacy-ui.exe",
        f"{top_dir}/web/index.html",
        f"{top_dir}/web/app.js",
        f"{top_dir}/web/app_bg.wasm",
        f"{top_dir}/web/favicon.svg",
        f"{top_dir}/web/favicon.ico",
        f"{top_dir}/web/apple-touch-icon.png",
        f"{top_dir}/web/icon-192.png",
        f"{top_dir}/web/icon-512.png",
        f"{top_dir}/web/site.webmanifest",
        f"{top_dir}/config.example.toml",
        f"{top_dir}/README.md",
    }
    missing = sorted(required - names)
    if missing:
        raise SystemExit(f"error: missing expected zip entries: {missing}")
PY

# 3. Web WASM bundle tarball
web_dir="$pkg_root/lunacy-web"
mkdir -p "$web_dir"
cp -a ui/dist/. "$web_dir/"
tar -czf "$out/lunacy-web.tar.gz" -C "$pkg_root" lunacy-web

# Validate tarballs
tar -tzf "$out/lunacy-linux-x86_64.tar.gz" >/dev/null
tar -tzf "$out/lunacy-web.tar.gz" >/dev/null

# 4. SHA-256 checksums
(
  cd "$out"
  sha256sum \
    lunacy-linux-x86_64.tar.gz \
    lunacy-windows-x86_64.zip \
    lunacy-web.tar.gz > SHA256SUMS.txt
  sha256sum -c SHA256SUMS.txt
)

# 5. Release notes
notes="$stage/release-notes.md"
{
  cat <<EOF
Rolling pre-release build from \`${SHORT_SHA}\`.

- **Commit:** \`${SHA}\` — ${COMMIT_SUBJECT}
- **Built at:** ${TIMESTAMP_UTC}

### Artifacts

| Platform | Target | Archive | Contents |
| --- | --- | --- | --- |
| Linux | \`x86_64-unknown-linux-gnu\` | \`lunacy-linux-x86_64.tar.gz\` | \`lunacy\`, \`lunacy-ui\`, \`web/\`, \`config.example.toml\`, \`README.md\`, \`contrib/lunacy.service\`, \`contrib/lunacy.desktop\`, \`icons/\` |
| Windows | \`x86_64-pc-windows-msvc\` | \`lunacy-windows-x86_64.zip\` | \`lunacy.exe\`, \`lunacy-ui.exe\`, \`web/\`, \`config.example.toml\`, \`README.md\` |
| Web | \`wasm32\` static bundle | \`lunacy-web.tar.gz\` | \`ui/dist/\` static web UI |

> Note: macOS builds are not included yet.

### SHA-256 Checksums

\`\`\`
EOF
  cat "$out/SHA256SUMS.txt"
  cat <<'EOF'
```
EOF
} > "$notes"

echo "==> [release] Built artifacts:"
ls -lh "$out"

if is_dry_run; then
  echo ""
  echo "==> [release] DRY RUN — skipping GitHub upload and local web sync"
  echo "Target repository : $REPO"
  echo "Release tag       : $TAG"
  echo "Release title     : Pre-release ($SHORT_SHA)"
  echo "Commit SHA        : $SHA"
  if [ -n "${LUNACY_SYNC_LOCAL_WEB_DIR:-}" ]; then
    echo "Local web sync dir: ${LUNACY_SYNC_LOCAL_WEB_DIR} (skipped in dry-run)"
  fi
  echo ""
  echo "--- lunacy-linux-x86_64.tar.gz contents ---"
  tar -tzf "$out/lunacy-linux-x86_64.tar.gz"
  echo ""
  echo "--- lunacy-windows-x86_64.zip contents ---"
  python3 -m zipfile -l "$out/lunacy-windows-x86_64.zip"
  echo ""
  echo "--- lunacy-web.tar.gz contents ---"
  tar -tzf "$out/lunacy-web.tar.gz"
  echo ""
  echo "--- Release notes ---"
  cat "$notes"
  exit 0
fi

echo "==> [release] Publishing pre-release $TAG on $REPO ($SHORT_SHA)"
if gh api "repos/$REPO/git/ref/tags/$TAG" >/dev/null 2>&1; then
  gh api -X PATCH "repos/$REPO/git/refs/tags/$TAG" -f sha="$SHA" -F force=true >/dev/null
fi
if gh release view "$TAG" -R "$REPO" >/dev/null 2>&1; then
  gh release edit "$TAG" -R "$REPO" \
    --target "$SHA" \
    --prerelease \
    --title "Pre-release ($SHORT_SHA)" \
    --notes-file "$notes"
  gh release upload "$TAG" -R "$REPO" --clobber \
    "$out/lunacy-linux-x86_64.tar.gz" \
    "$out/lunacy-windows-x86_64.zip" \
    "$out/lunacy-web.tar.gz" \
    "$out/SHA256SUMS.txt"
else
  gh release create "$TAG" -R "$REPO" \
    --target "$SHA" \
    --prerelease \
    --title "Pre-release ($SHORT_SHA)" \
    --notes-file "$notes" \
    "$out/lunacy-linux-x86_64.tar.gz" \
    "$out/lunacy-windows-x86_64.zip" \
    "$out/lunacy-web.tar.gz" \
    "$out/SHA256SUMS.txt"
fi

# Optional local web UI sync
if [ -n "${LUNACY_SYNC_LOCAL_WEB_DIR:-}" ]; then
  sync_dir="${LUNACY_SYNC_LOCAL_WEB_DIR/#\~/$HOME}"
  sync_dir="${sync_dir%/}"
  if [ -n "$sync_dir" ]; then
    sync_parent="$(dirname "$sync_dir")"
    if [ -d "$sync_parent" ]; then
      src_real="$(realpath ./ui/dist)"
      dst_real="$(realpath -m "$sync_dir")"
      if [ "$src_real" != "$dst_real" ]; then
        echo "==> [release] Syncing web UI bundle to $sync_dir"
        if command -v rsync >/dev/null 2>&1; then
          mkdir -p "$sync_dir"
          rsync -a --delete --delay-updates ./ui/dist/ "$sync_dir/"
        else
          tmp_sync="$(mktemp -d "$sync_parent/.lunacy-web-sync.XXXXXX")"
          cp -a ./ui/dist/. "$tmp_sync/"
          if [ -e "$sync_dir" ]; then
            old_sync="$sync_parent/.lunacy-web-old.$$"
            rm -rf "$old_sync"
            mv "$sync_dir" "$old_sync"
            if ! mv "$tmp_sync" "$sync_dir"; then
              mv "$old_sync" "$sync_dir" || true
              rm -rf "$tmp_sync"
              exit 1
            fi
            rm -rf "$old_sync"
          else
            mv "$tmp_sync" "$sync_dir"
          fi
        fi
      fi
    fi
  fi
fi

echo "==> [release] Done."
