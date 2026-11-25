# lunacy-ui

A Rune client for the [lunacy](../) local CI/deploy daemon: the project list, run history, run
detail with act job trees and live logs, and the full config editor.

It talks to the daemon over HTTP only (`docs/api.md`), so it runs against a daemon on this
machine, one on another machine, or `tools/mock_server.py`. One view tree builds natively and for
the web.

- **Native:** `cargo run` — a desktop window with the system light/dark theme.
- **Web:** `./build-web.sh` → `dist/` — a static folder you serve over HTTP(S). WebGPU needs a
  secure context, so `http://localhost` works but a LAN IP needs HTTPS.

## Prerequisites

The window is Rune's, pinned to a git rev, so a Rust toolchain recent enough for it is required
(this tree is built with `cargo 1.100.0-nightly`).

| Platform | Needs |
| --- | --- |
| Linux | `libvulkan` (or a working Vulkan ICD), `libxkbcommon`, `libwayland` or X11, and a font package such as `ttf-inter`/`noto-fonts` |
| macOS | Xcode command line tools (`xcode-select --install`); Metal is used as-is |
| Windows | MSVC build tools (`winget install Microsoft.VisualStudio.2022.BuildTools`) |

For the web build additionally:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130 --locked   # must match Cargo.lock
# `wasm-opt` from binaryen, e.g. `pacman -S binaryen`, `brew install binaryen`,
# or `apt install binaryen` — the optimised build calls it.
```

## Running against the daemon

```sh
cargo run                                   # http://127.0.0.1:7878
cargo run -- --server http://host:7878 --token "$TOKEN"
LUNACY_SERVER=http://host:7878 LUNACY_TOKEN=$TOKEN cargo run
```

The same values can be typed into **Settings → Connection**, which also shows the daemon version
and the connection state. The token may be empty when the daemon has no token configured.

The web build defaults to the page's own origin, so serving `dist/` from the daemon (or a proxy in
front of it) needs no configuration.

## Screens

| Screen | What it shows |
| --- | --- |
| Projects | Every project, its branch, latest commit, run status and a **Run now** button |
| Project | The current run's steps and the run history |
| Run | Steps in execution order, act jobs as a GitHub-Actions tree, and the step log |
| Settings | General / Server / GitHub / Discord / act config, project CRUD, per-project CI and deploy steps, Diagnostics, Connection |

`github_url` from the daemon becomes **View on GitHub**. When it is absent, the link is built from
the project's `github_repo` and the run's `sha`. In the web build the address bar tracks the
current run (`#/runs/{id}`), so a daemon check's `details_url` opens that run directly.

### Host-restricted steps

A step may carry `only_on: ["linux" | "windows" | "macos"]`; absent or empty means every host. The
step editor has an **Host OS** row of chips for this, and it says whether the step will run on the
daemon you are connected to. In a run, a step the daemon skipped this way reads
`skipped (not on this OS: …)` rather than looking like a failure. **Diagnostics** leads with the
daemon's host OS, since that is what decides the question.

## Configuration editing

Settings reads `GET /api/config` and writes `PUT /api/config` with the daemon's own JSON. Unknown
keys are preserved. Secrets come back masked as `********`; leaving a masked value alone keeps the
stored secret. The form tracks dirt: **Save** (or `Cmd`/`Ctrl`+`S`)
sends the whole config, **Discard** reverts. A `400` puts the daemon's message on the offending
field and jumps to its page. The daemon validates, so an invalid config never leaves the window as
a half-written file.

Polling runs off the UI thread (a worker on native, `fetch` tasks on web) and stops while the
window is unfocused. `Cmd`/`Ctrl`+`R` reconnects.

## Developing without a daemon

`tools/mock_server.py` serves the fixtures in `tools/fixtures.json` with the daemon's real status
codes, validation and CORS:

```sh
python3 tools/mock_server.py --port 7878 --token dev
cargo run -- --token dev
```

It animates the running project so the polling and log views have something to move.

### Screenshots

The scenes in `src/demo.rs` seed `State` straight from `tools/fixtures.json`, so a screenshot never
needs a daemon, a port or a token:

```sh
./tools/shots.sh                 # every scene, light and dark, into screenshots/
./tools/shots.sh settings-step   # just one
cargo run -- --list-scenes       # the scene names
cargo run -- --snap /tmp/a.png --scene run --theme dark --size 1180x760
```

Headless rendering needs a GPU or a software ICD. Without `/dev/dri` on Linux, Mesa's lavapipe
works:

```sh
VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json ./tools/shots.sh
```

## Tests

```sh
cargo test        # path, state and fixture-timestamp tests
cargo clippy
```

## Layout

```text
src/
  api.rs      endpoint paths, the request enum, response parsing
  net.rs      native ureq / web fetch, one Reply shape
  model.rs    the daemon's JSON types (tolerant of unknown fields)
  config.rs   the editable config, incl. masked secrets
  paths.rs    dotted-path access into the config JSON
  state.rs    State, the poll loop, the editor's draft/dirty state
  theme.rs    status inks over the kit's neutral tokens
  widgets.rs  the small shared pieces (pills, cards, field rows, logs)
  views/      projects, project, run, settings, steps, diagnostics
  app.rs      window chrome, the nav rail, screens, keys, scenes
  demo.rs     fixture-backed scenes for the screenshots
tools/
  mock_server.py  a stand-in daemon
  fixtures.json   the demo data, also served by the mock
  shots.sh        render every scene in both themes
```

## Deviations from the daemon contract

None are required. The UI reads `os` from `GET /api/system` and `only_on` from config and runs;
both are already in `docs/api.md`/`docs/act.md`. Where a field is missing the UI degrades rather
than failing: no `github_url` falls back to the commit URL, an unknown `os` omits the host note, and
an older daemon without `only_on` simply shows no chips selected.
