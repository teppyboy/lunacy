# Lunacy UI (`lunacy-ui`)

A [Rune](https://github.com/anomalyco/rune) client for the [Lunacy](../README.md) local CI/deploy daemon: real-time project overview, run history, inspectable `act` substep accordions with live logs, pre-flight deploy checks, and a full comment-preserving `config.toml` editor.

One Rust view tree builds natively for Linux, macOS, and Windows (GUI subsystem with embedded platform icons) and compiles to a static WebGPU WASM app for the browser. It streams live state and step logs over WebSockets (`GET /api/ws`) with automatic HTTP polling fallback ([`docs/api.md`](../docs/api.md)), so it works against a local daemon, a remote host, or `tools/mock_server.py`.

- **Native:** `cargo run` — desktop window with embedded app icons and System / Light / Dark / OLED themes.
- **Web:** `./build-web.sh` → `dist/` — static bundle served by `lunacy` via `[server] web_root` or any HTTP(S) server. WebGPU requires a secure context (`http://localhost` works; LAN IPs need HTTPS).

## Prerequisites

Rune is pinned to a git revision in [`Cargo.toml`](Cargo.toml), so use a recent Rust toolchain.

| Platform | Requirements |
| --- | --- |
| Linux | `libvulkan` (or a working Vulkan ICD), `libxkbcommon`, `libwayland` or X11, and a system font such as `ttf-inter` or `noto-fonts` |
| macOS | Xcode Command Line Tools (`xcode-select --install`); Metal is used directly |
| Windows | MSVC Build Tools (`winget install Microsoft.VisualStudio.2022.BuildTools`) |

For the WebGPU WASM build additionally:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130 --locked   # must match ui/Cargo.lock
# `wasm-opt` from binaryen (`pacman -S binaryen`, `brew install binaryen`, or `apt install binaryen`)
```

## Running & Local Settings Persistence

```sh
cargo run                                   # connects to saved server or http://127.0.0.1:7878
cargo run -- --server http://host:7878 --token "$TOKEN"
LUNACY_SERVER=http://host:7878 LUNACY_TOKEN="$TOKEN" cargo run
```

You can also edit the Server URL and Bearer token inside **Settings → Connection**, which displays the daemon version and live WebSocket/HTTP connection state. Leave the token empty if `[server]` has no token configured.

Local UI preferences persist across restarts on both desktop and web:

- **Persisted values:** Server URL, Bearer token, Theme (`System` / `Light` / `Dark` / `OLED`), UI zoom, Log line wrap, `Steps | Raw` log view mode, and desktop auto-update check preference.
- **Storage location:**
  - **Desktop:** `prefs.json` in the platform config directory — `~/.config/lunacy/prefs.json` (created with `0o600` permissions on Linux/Unix), `~/Library/Application Support/lunacy/prefs.json` on macOS, or `%APPDATA%\lunacy\prefs.json` on Windows. Honors `RUNE_DIRS_ROOT` and `RUNE_PREFS_DIR`.
  - **Web:** browser `localStorage`.
- **Startup connection precedence:** `LUNACY_SERVER` / `LUNACY_TOKEN` env vars &gt; `--server` / `--token` CLI flags &gt; saved local settings &gt; defaults (`http://127.0.0.1:7878` on desktop, current page origin on web).

## Screens

| Screen | What it provides |
| --- | --- |
| **Projects** | Every configured project, branch, latest commit, open PR indicators, paused badge, live run status (with multi-active-run counts), **Run now**, and **Cancel** |
| **Project** | Live active run cards (with per-run **Cancel** when running concurrently), **Run now**, **Force deploy**, **Cancel**, open PR list, and run history with per-run **Retry** |
| **Run** | CI and deploy steps in execution order, expandable `act` substep accordions with a **Steps \| Raw** log view toggle, line wrap (`Alt+Z`), log copy & jump-to-bottom controls, live log streaming, **Cancel**, **Retry**, **Force deploy**, and **View on GitHub** |
| **Settings — Config** | Live editor for **General** (including `max_concurrent_runs`), **Server**, **GitHub**, **Discord** (including an SSRF-safe test webhook button), **act** (with `max_concurrent` and cache clear), and **Projects** (CRUD, per-repo pause toggle, concurrency & GitHub/Discord/runner overrides, CI & deploy step editors with `only_on` chips and live workflow/job pickers) |
| **Settings — Host & App** | **Deploy** (offline pre-flight **Deploy Checks** via `GET /api/deploy/checks`), **Diagnostics** (daemon host OS, concurrency limits, `act`/`docker`/`git` versions, webhook delivery stats), **Connection** (server URL, token, transport status), and **Appearance** (`System` / `Light` / `Dark` / pure-black `OLED` themes, discrete UI zoom `90%`–`150%`, and desktop auto-updater) |

`github_url` from the daemon powers **View on GitHub** (falling back to the repository commit URL when absent). On the web build, the URL hash tracks the open run (`#/runs/{id}`), so GitHub Check Run `details_url` links open the matching run directly.

### Host-Restricted Steps (`only_on`)

Steps may restrict execution with `only_on = ["linux" | "windows" | "macos"]`. The step editor displays **Host OS** chips and notes whether the step will run on the connected daemon. In run detail, steps skipped due to `only_on` render in muted text (`skipped (not on this OS: …)`) rather than as failures.

### Desktop Auto-Updater

On native Linux (`x86_64`) and Windows (`x86_64`), **Settings → Appearance** checks the rolling GitHub `prerelease` tag (optionally on startup), verifies the matching platform archive against `SHA256SUMS.txt`, atomically swaps the `lunacy-ui` binary, and prompts to relaunch.

## Keyboard Shortcuts

| Shortcut | Action |
| --- | --- |
| `Cmd/Ctrl + S` | Save daemon config on config pages, or save & reconnect on **Settings → Connection** (`Enter` in Server URL / Token also submits) |
| `Cmd/Ctrl + R` | Refresh and reconnect using the active connection |
| `Cmd/Ctrl + ,` | Open Settings |
| `Cmd/Ctrl + =` / `-` | Step UI zoom in / out (`90%`, `100%`, `110%`, `125%`, `150%`) |
| `Cmd/Ctrl + 0` | Reset UI zoom to responsive default |
| `Alt + Z` | Toggle log line wrap on the Run screen |
| `Esc` | Close open drawer/dialog, or navigate back (`Run → Project → Projects`, `Settings → Projects`) |

## Live Streaming & Config Editing

- **Real-time transport:** Native uses `tungstenite` (`rustls`) on a background thread and wakes Rune via `Waker`; web uses `web_sys::WebSocket` with first-frame JSON auth (`{"type": "auth", "token": ...}`). If WebSocket is unavailable or disconnects, `lunacy-ui` automatically falls back to HTTP polling and reconnects with backoff.
- **Safe config edits:** Settings reads `GET /api/config` and writes `PUT /api/config`. Unknown keys and TOML comments are preserved on the daemon; secrets arrive masked as `********` (leaving the mask untouched keeps the stored value). **Save** (`Cmd/Ctrl+S`) submits the draft, **Discard** reverts, and `400` validation errors jump directly to the offending field.

## Developing Without a Daemon

[`tools/mock_server.py`](tools/mock_server.py) serves [`tools/fixtures.json`](tools/fixtures.json) with real status codes, validation, and CORS:

```sh
python3 tools/mock_server.py --port 7878 --token dev
cargo run -- --token dev
```

### Screenshots

Demo scenes in [`src/demo.rs`](src/demo.rs) load fixtures directly with no daemon required:

```sh
./tools/shots.sh                 # render every scene (light, dark & OLED) into screenshots/
./tools/shots.sh settings-step   # render a single scene
cargo run -- --list-scenes       # list available scene names
cargo run -- --snap /tmp/a.png --scene run --theme dark --size 1180x760
```

Headless rendering requires a GPU or software Vulkan ICD (e.g., Mesa lavapipe on Linux):

```sh
VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json ./tools/shots.sh
```

## Tests & E2E

```sh
# Unit tests, clippy & wasm check
cargo test --locked
cargo clippy --all-targets -- -D warnings
cargo check --target wasm32-unknown-unknown --locked

# Full end-to-end verification (real sandboxed lunacy daemon + headless native Rune & Web Chrome)
./tools/e2e.sh
```

`./tools/e2e.sh` runs in an isolated temporary directory on a free port and never touches port `7878`, `~/.config/lunacy`, `~/.local/share/lunacy`, or `ui/dist/`.

## Layout

```text
src/
  main.rs       native desktop entry point & embedded platform app icons
  lib.rs        library exports & wasm32 `start` entry point
  app.rs        window chrome, nav rail, routing, shortcuts & demo scenes
  state.rs      State, WS/HTTP sync loop, draft/dirty config editor & prefs persistence
  ws.rs         real-time WebSocket client (native tungstenite + web_sys::WebSocket)
  api.rs        endpoint paths, request enum & response parsing
  net.rs        native ureq / web fetch transport
  model.rs      daemon JSON models (tolerant of unknown fields)
  config.rs     editable config model & masked secret handling
  paths.rs      dotted-path access into config JSON
  theme.rs      System / Light / Dark / OLED theme tokens & status inks
  icons.rs      cohesive 16×16 stroke icon catalog for Rune
  updater.rs    desktop auto-updater (GitHub prerelease + SHA256SUMS verification)
  widgets.rs    shared UI components (pills, cards, field rows, log viewer)
  demo.rs       fixture-backed scenes for offline screenshots
  views/
    projects.rs     project list overview
    project.rs      single project view, PR list & run history
    run.rs          run detail, expandable Steps | Raw act accordion & live logs
    settings.rs     config pages, Connection & Appearance
    steps.rs        CI/deploy step editor with only_on & workflow pickers
    deploy.rs       offline pre-flight Deploy Checks view
    diagnostics.rs  daemon host OS, toolchain & webhook diagnostics
tools/
  mock_server.py    stand-in mock daemon
  fixtures.json     demo fixtures shared by mock server & screenshot scenes
  shots.sh          render screenshot suite across themes
  e2e.sh            wrapper that builds binaries and invokes tools/e2e/run_e2e.py
  e2e/
    run_e2e.py      sandboxed real-daemon E2E test harness (native + web)
    web_check.mjs   headless Chrome verification script for the WebGPU build
web/
  index.html        web host page with deep-link routing & build hash busting
  site.webmanifest  PWA manifest
```
