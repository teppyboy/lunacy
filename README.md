<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="assets/logo-light.svg">
  <img alt="Lunacy" src="assets/logo-light.svg" width="64">
</picture>

# Lunacy

A small local CI/deploy daemon. It polls a git remote, runs a pipeline for each
new commit, and reports the result to GitHub and Discord. It replaces per-repo
shell scripts, timers and webhook workers with one binary and one config file.

Runs on Linux, macOS and Windows. No OpenSSL: TLS is pure Rust (`rustls`).

## What it does

For every `[[project]]`:

1. `git fetch <remote> <branch>` on an interval (default 300 s).
2. If the upstream sha differs from the last processed sha, run the pipeline.
3. Run `ci` steps in order. If one fails, the rest of CI and all of `deploy` are
   recorded `skipped`.
4. Run `deploy` steps only when CI passed.
5. Report each phase to GitHub (commit status or check run) and Discord.
6. Persist the run and per-step logs under `state_dir`, and advance the
   last processed sha **whether the pipeline passed or failed**, so a broken
   commit is not retried every poll. Use `lunacy once --force` to re-run it.

Projects are independent: one thread each, never two pipelines for the same
project at once, and one project's failure never stops the daemon.

Two kinds of step:

- `kind = "shell"` (default) — your own command, e.g. the repo's deploy script.
- `kind = "act"` — a job from a GitHub Actions workflow, run by
  [nektos/act](https://github.com/nektos/act). Lunacy clones the commit into
  `state_dir/checkouts/<run id>/repo`, writes `event.json`, streams act's
  `--json` output into per-step substeps (visible in the API, the GitHub summary
  and Discord), and serialises act across projects with a machine-wide lock.

Every step of the run sees that clone's path as `LUNACY_CI_DIR` (unset when no
act step ran). act runs with `--bind`, so files a CI job writes into the clone
are still there for a later deploy step — that is how a deploy publishes the
`build/web` the CI job produced without running act twice. The clone is removed
when the pipeline ends. act's container can leave root-owned files behind; the
cleanup then only logs a warning, and you remove the directory with `sudo rm -rf`.

`[act] lock_file` points the act lock at the file a manual `make ci` also uses.
`fs2`'s `lock_exclusive` is `flock(2)`, the same lock the `flock` CLI takes, so
Lunacy and lunarum's `scripts/with-act-lock.sh` never run act at the same time.

## Install

```bash
cargo build --release      # target/release/lunacy
cargo test
```

Copy `config.example.toml` to `config.toml` (git-ignored) and edit it.

```bash
lunacy check -c config.toml                 # validate, list projects
lunacy once   -c config.toml lunarum        # run one pipeline now
lunacy once   -c config.toml lunarum --force  # even if the sha was processed
lunacy run    -c config.toml                # the daemon
```

`lunacy run` serves the HTTP API when `[server]` is present (see
[`docs/api.md`](docs/api.md)); the optional Rune UI in `ui/` is served from
`web_root` or built natively as `lunacy-ui`.

For desktop app launchers and bundles:

- Linux: install [`contrib/lunacy.desktop`](contrib/lunacy.desktop) into
  `~/.local/share/applications/` and copy `icons/hicolor/` from a release archive
  into `~/.local/share/icons/hicolor/` (or from source, install
  `assets/icons/lunacy.svg` to `~/.local/share/icons/hicolor/scalable/apps/lunacy.svg`
  and `assets/icons/lunacy-<size>.png` to
  `~/.local/share/icons/hicolor/<size>x<size>/apps/lunacy.png`).
- macOS: assemble `Lunacy.app` with [`contrib/macos/Info.plist`](contrib/macos/Info.plist),
  `lunacy-ui`, and [`assets/icons/lunacy.icns`](assets/icons/lunacy.icns).

Config is strict: unknown keys are a hard error, so a typo fails `lunacy check`
instead of being silently ignored. Secrets may live in the file; the API masks
them (`"********"`) and a `PUT /api/config` that echoes the mask keeps the
stored value. A secret can also come from a file, which is read (and trimmed) at
load and kept out of the config: `github.token_file`,
`github.app.webhook_secret_file`, `discord.webhook_url_file` and
`server.token_file`. Setting both the inline value and its `_file` is a
validation error; `~/` is expanded in every path. Only the file *paths* appear in
the API's Config JSON — the values never do.

## Windows host

act is a Linux-first tool, so a Windows host needs a little care. Lunacy handles
the common cases:

- **Self-hosted jobs run natively.** On Windows, `windows-latest`,
  `windows-2022` and `windows-2025` default to act's `-self-hosted` platform,
  so those jobs run on the host instead of inside a Linux container. Override
  this with `[act] platforms`, a project's `platforms`, or a step's `platforms`
  (later wins). On macOS the same happens for `macos-latest`, `macos-14` and
  `macos-15`. On Linux there is no default for `windows-*`/`macos-*`, because
  those jobs cannot run there.
- **`[act] native_command` for self-hosted jobs.** If `[act] command` is the WSL
  form (`["wsl.exe", "-e", "act"]`), self-hosted jobs still use
  `native_command` (default `["act"]`), because WSL is Linux and cannot be
  self-hosted. Lunacy decides per step by reading the workflow YAML and the
  job's `runs-on`.
- **WSL path translation.** In WSL mode, Lunacy rewrites `C:\...` to
  `/mnt/c/...` for `-W`, `-e`, `--secret-file`, `--var-file`,
  `--artifact-server-path`, `-C` and `--flag=<path>` forms.
- **`only_on`** marks a step as host-specific:

  ```toml
  [[project.ci]]
  name = "Windows build"
  kind = "act"
  workflow = ".github/workflows/flutter-ci.yml"
  job = "windows"
  only_on = ["windows"]
  ```

  On any other host the step is recorded `skipped`, not `failed`: CI still
  passes, and the skip shows up in the API, the GitHub summary and Discord.
  Values are `"linux"`, `"windows"`, `"macos"`.
- **Artifacts work.** Every act step gets
  `--artifact-server-path <state_dir>/artifacts/<run id>`, so
  `actions/upload-artifact@v4` succeeds. Retrieve the files from that directory.
- **Shell default** is `["cmd", "/C"]` on Windows, `["bash", "-c"]` elsewhere.
  Override globally with `[general] shell`, per project, or per step — e.g.
  `shell = ["pwsh", "-NoProfile", "-Command"]`.

Prerequisites for a Windows host running the sample `windows` job: Git,
Node.js, Flutter 3.47.8, and Visual Studio 2022 Build Tools with the
"Desktop development with C++" workload. A `shell` step that runs a repository's
deploy script still needs whatever that script needs.

## Running it as a service

- Linux (systemd): [`contrib/lunacy.service`](contrib/lunacy.service)
- macOS (launchd): [`contrib/dev.lunacy.plist`](contrib/dev.lunacy.plist)
- Windows: `sc.exe create lunacy binPath= "C:\path\lunacy.exe run -c C:\path\config.toml" start= auto`
  (or run it from Task Scheduler at logon).

## GitHub reporting

`[github] mode = "status"` posts classic commit statuses (a PAT is enough).
`mode = "checks"` posts check runs named `lunacy/ci` and `lunacy/deploy`, with a
live-updating step table and the tail of the failing log; the Checks API only
accepts a GitHub App installation token, so it needs `[github.app]`.

Setting up the App once:

1. GitHub → Settings → Developer settings → GitHub Apps → **New GitHub App**.
2. Permissions: **Checks: read & write**, **Commit statuses: read & write**,
   **Contents: read**, **Metadata: read**. Subscribe to the **Check run** and
   **Check suite** events.
3. Webhook URL: `http://<host>:<port>/api/github/webhook` (the `[server] listen`
   address). Set a webhook secret and put the same string in
   `[github.app] webhook_secret`.
4. Generate a private key, save it, and point `private_key_path` at it.
5. Install the App on the repositories, then put `app_id` in the config.
   `installation_id` is looked up automatically when omitted.

The webhook is what makes GitHub's **Re-run** button work: Lunacy verifies
`X-Hub-Signature-256`, then queues the run for every project whose
`github_repo` matches. `public_url` is used for the check run's `details_url`,
which can link back into the UI (`#/runs/{id}`).

## Discord

Set `[discord] webhook_url` for a global default, or
`discord_webhook_url` per project. Messages are embeds: start (optional), then
the outcome with a per-step table and the tail of the failing log. Reporting
failures are logged and never fail the pipeline.

## Mapping to the two repos it replaces

`config.example.toml` is wired for both:

- **lunarum** — CI is the `web` job of `.github/workflows/local-deploy-ci.yml`
  under act; a second act step builds the `windows` job of `flutter-ci.yml`
  with `only_on = ["windows"]`. That job runs with `--bind`, so it leaves
  `build/web` in `LUNACY_CI_DIR` for the deploy step: publishing can rsync
  `$LUNACY_CI_DIR/build/web/` into the live checkout and recreate the container
  instead of running act again. Deploy runs `scripts/local-web-deploy.sh`,
  which still keeps its own fetch/act/health-check logic and health endpoint
  `127.0.0.1:47831`. Set `[act] lock_file = "~/.local/state/act-gate.lock"` so
  Lunacy and the script's `with-act-lock.sh` share one lock.
- **frostedge** — CI is the `validate` job of `.github/workflows/rust.yml`
  under act, with the runner image pinned by digest exactly like
  `scripts/ci-act.sh`. Its commit-message check walks `before..after`, so
  `event.json` carries the last processed sha as `before` when that is an
  ancestor of the target, else the target's first parent, and only zeros for a
  root commit — a zero `before` would make the check walk the whole history. Deploy runs `scripts/deploy-local.sh`, which still does
  its own dirty-worktree check, fast-forward and health check on
  `127.0.0.1:47830`.

Both use `checkout = "none"`: the deploy scripts operate on the live checkout
themselves, and act steps clone the commit into a temporary directory, so
Lunacy must not move `HEAD` under them. Use `checkout = "worktree"` for a
throwaway checkout at the commit, or `"in-place"` to check the commit out in
`repo_path` (it refuses to touch a dirty worktree).

Notes on what Lunacy does *not* take over from the old scripts:
`scripts/ci-act.sh` additionally picks free loopback ports per run, sweeps
leftover `act-Rust-CI-*` containers, injects `RUST_TEST_NOCAPTURE`, and tees a
kept log; Lunacy uses the fixed `vars` in the example plus its own `timeout`.
Both repos' deploy scripts send their own Discord notification, so set
`discord_webhook_url = ""` if you want only Lunacy's message.

## Layout

```
src/config.rs      config types, validation, secret masking, OS/shell defaults
src/configfile.rs  comment-preserving TOML writes (toml_edit + atomic rename)
src/exec.rs        child processes, line streaming, timeout kill per platform
src/git.rs         fetch/rev-parse/worktree/clone helpers
src/act.rs         act platform maps, self-hosted detection, WSL, --json parsing
src/pipeline.rs    checkout, step loop, act steps, reporting hooks
src/report.rs      GitHub statuses + check runs (App JWT, token cache)
src/github_app.rs  RS256 JWT and installation-token cache
src/discord.rs     Discord embeds
src/daemon.rs      per-project workers, polling, config reload
src/api.rs         tiny_http JSON API + static UI
src/model.rs       Run/Step model, state files, log tailing
docs/api.md        HTTP API contract
docs/act.md        the act --json lines Lunacy parses
```
