# Running CI with nektos/act

lunacy runs GitHub Actions workflows locally with [nektos/act](https://github.com/nektos/act) (`kind = "act"` steps).
Notes below were checked against act 0.2.97 on Linux. The Windows and macOS notes come from the act docs and issues and have not been tested here.

## How lunacy drives act

- Command: `act <event> --json -W <workflow> [-j <job>] -e <event.json> -P <platform>=<image> --secret-file <tmp> --var-file <tmp> [args...]`, run in a **fresh clone** of the target SHA (`git clone --no-checkout` + `git fetch <sha>` + `checkout --detach`). act copies the working tree, and a linked `git worktree` has no usable `.git` dir. frostedge's `scripts/ci-act.sh` already uses this pattern.
- **One clone per run**, at `state_dir/checkouts/<run id>/repo`: the first act step creates it, later act steps reuse it, and it is removed when the pipeline ends. Every step of the run gets its path as `LUNACY_CI_DIR` (unset when no act step ran), so a deploy shell step can publish what a CI job built in it — act leaves files in place with `--bind`. Cleanup is best-effort: `--bind` can leave root-owned files, and then lunacy only warns (remove it with `sudo rm -rf`).
- `--json` writes one JSON object per line on stdout/stderr. Example from act 0.2.97:
  - step start: `{"job":"T/Build it","jobID":"build","stage":"Main","step":"Hello","stepID":["0"],"msg":"⭐ Run Main Hello"}`
  - output: `{"step":"Hello","raw_output":true,"msg":"hi\n"}`
  - step end: `{"step":"Hello","stepResult":"success"|"failure"|"skipped","executionTime":5501845}` (executionTime is in ns)
  - job end: `{"jobID":"build","jobResult":"success"|"failure","msg":"🏁  Job failed"}`
  - Setup and teardown steps use `stepid` (lowercase) `["--setup-job"]` / `["--complete-job"]`.
  - The process exits non-zero when any job fails.
  lunacy turns these lines into per-step progress (`substeps` in the API and in GitHub check summaries). The human-readable log keeps the `msg` text.
- One act run at a time per machine (global lock). act names containers after the workflow and job, so two parallel runs remove each other's containers (see frostedge `ci-act.sh`, lunarum `with-act-lock.sh`).
  The lock file is `[act] lock_file`, default `state_dir/act.lock`. lunacy locks it with `fs2`'s `lock_exclusive`, which is `flock(2)` — the same lock the `flock` CLI takes — so pointing it at lunarum's `~/.local/state/act-gate.lock` keeps a manual `make ci` and lunacy apart.
- `event.json` is a push event: `after`/`head_commit.id` = SHA, `ref` = `refs/heads/<branch>`, `repository.full_name`, `head_commit.message` (the full commit message), `commits[0]`, and minimal `pusher`/`sender` objects. `before` is the last sha lunacy processed for the project when it is an ancestor of the target, else the target's first parent, else all zeros (a root commit). Workflows that read commit messages over `before..after` (frostedge's `rust.yml`) would otherwise treat a zero `before` as "walk from the repo root" and re-check the whole history.

## Linux

Install act (`pacman -S act`, `brew install act`, or the release tarball) and Docker Engine. `ubuntu-*` jobs run in containers. Use `catthehacker/ubuntu:act-latest` (medium image) or pin a digest the way frostedge does.

## macOS

Install act with `brew install act` and run Docker Desktop, OrbStack or colima. On Apple Silicon, pass `--container-architecture linux/amd64` when an image or action is amd64-only. `macos-*` jobs cannot run in containers. Map them to the host with `-P macos-latest=-self-hosted` (needs the job's tools installed on the Mac).

## Windows

- **Install:** `winget install nektos.act`, `choco install act-cli` or `scoop install act`. Then install Docker Desktop with the WSL2 backend in **Linux containers** mode. act talks to `npipe:////./pipe/docker_engine` by itself.
- **`ubuntu-*` jobs** run in Linux containers through Docker Desktop, the same as on Linux.
- **`windows-*` jobs run directly on the host (chosen setup).** act cannot run Windows containers (nektos/act#2726), so on a Windows host lunacy maps `windows-latest`/`windows-2022`/`windows-2025` to `-self-hosted` by default. Steps run in a work dir under the act cache with the host's real toolchain; nothing is isolated. Install what the job needs: for lunarum's `windows` job in `flutter-ci.yml`, that is Git, Flutter 3.47.8 (or let `subosito/flutter-action` download it), and Visual Studio 2022 Build Tools with "Desktop development with C++". Node.js is also required, because act runs JS actions such as `actions/checkout` with the host's `node`. `actions/upload-artifact@v4` needs `--artifact-server-path`, which lunacy sets to `state_dir/artifacts/<run id>`.
- Steps can set `only_on = ["windows"]` (or `"linux"`/`"macos"`). On any other host OS the step is reported as `skipped` and is not counted as a failure. One config file can then serve every machine.
- **Known bug:** act running natively on Windows can pass mangled Windows paths to some JS/composite actions (nektos/act#2645). Those workflows run fine from WSL2. When you hit this bug, set the act command to run inside WSL:
  `[act] command = ["wsl.exe", "-e", "act"]`. lunacy then translates repo/temp paths to `/mnt/<drive>/...` (`wslpath` rules) and creates the clone on the WSL side. WSL mode only applies to container (`ubuntu-*`) jobs. A step whose job maps to `-self-hosted` on Windows always uses the native act (`[act] native_command`, default `["act"]`), because WSL is Linux.
- Each step's shell defaults to `cmd /C`. Set `shell = ["pwsh", "-NoProfile", "-Command"]` or Git Bash for bash scripts.

## Config

```toml
[act]                       # global defaults, all optional
command = ["act"]           # e.g. ["wsl.exe", "-e", "act"] on Windows
platforms = { "ubuntu-latest" = "catthehacker/ubuntu:act-latest" }
args = []                   # extra args for every act run, e.g. ["--pull=false"]
lock_file = "~/.local/state/act-gate.lock"   # default: <state_dir>/act.lock

[[project.ci]]
name = "Web"
kind = "act"
workflow = ".github/workflows/local-deploy-ci.yml"
job = "web"
event = "push"
timeout = 3600
```
