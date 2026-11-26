# lunacy HTTP API (v1)

Contract between the daemon (`lunacy run`) and the Rune UI (`ui/`, native + web).
JSON everywhere except logs. Times are unix seconds (u64). Unknown fields must be ignored by clients.

## Server config (`config.toml`)

```toml
[server]
listen = "127.0.0.1:7878"   # omit [server] to disable the API
token = ""                  # optional; if set, every /api request needs `Authorization: Bearer <token>`
token_file = "~/.config/lunacy/api-token"   # or read the token from a file
web_root = "ui/dist"        # optional; static files (Rune web build) served at `/`
```

The daemon adds `Access-Control-Allow-Origin: *` plus `Access-Control-Allow-Headers: Authorization, Content-Type`
and answers `OPTIONS` preflight with 204, so a UI served elsewhere still works.

Errors: non-2xx with `{"error": "message"}`. 401 bad/missing token, 404 unknown project/run, 409 trigger while running.

## Types

```jsonc
// Project
{
  "name": "lunarum",
  "branch": "master",
  "github_repo": "teppyboy/lunarum",   // null if unknown
  "upstream_sha": "abc...",            // null if never fetched
  "last_sha": "abc...",                // last processed sha on default branch, null if none
  "running": false,
  "current_run": Run | null,           // the running pipeline, if any
  "last_run": Run | null,              // most recent finished run
  "open_prs": [PrSummary],             // open GitHub PRs when watch_prs = true
  "watch_prs": false,
  "watch_tags": false,
  "deploy_refs": ["master", "tags/v*"], // empty = default branch only
  "trigger_mode": "both"                // "both" | "webhook" | "poll"
}

// PrSummary
{
  "number": 42,
  "title": "feat: add dark mode",
  "head_ref": "feat/dark-mode",
  "base_ref": "master",
  "head_sha": "abc...",
  "last_tested_sha": "abc..." | null,
  "status": "running" | "success" | "failure" | "error" | "cancelled" | null
}

// Run
{
  "id": "lunarum-1764067200-abc1234",  // unique, URL-safe
  "project": "lunarum",
  "sha": "abc...",
  "subject": "feat: something",        // commit subject, may be ""
  "status": "running" | "success" | "failure" | "error" | "cancelled",
  "started_at": 1764067200,
  "finished_at": 1764067290,           // null while running
  "failed_step": "Test" | null,
  "github_url": "https://github.com/o/r/runs/123" | null, // GitHub check run page, else null
  "trigger": "poll" | "manual" | "webhook" | "pull_request" | "force_deploy" | "tag",
  "deploy_only": false,                // true when triggered as a deploy-only / skip-CI deploy
  "git_ref": "master",                 // optional: branch, tags/<tag>, or PR head ref
  "pr_number": 42,                     // optional: present for PR builds (CI-only)
  "pr_title": "feat: ...",             // optional
  "pr_head_ref": "feat/x",             // optional
  "pr_base_ref": "master",             // optional
  "deploy_skipped_reason": "PR #42 — CI only (deploy runs on master)", // optional
  "steps": [Step]
}

// Step
{
  "phase": "ci" | "deploy",
  "name": "Test",
  "status": "pending" | "running" | "success" | "failure" | "timeout" | "skipped" | "cancelled",
  "duration_ms": 1234,                 // null until finished
  "exit_code": 0,                      // null if not finished / killed
  "kind": "shell" | "act",
  "substeps": [Substep]               // act steps only: live job/step progress parsed from `act --json`; [] for shell
}

// Substep (one GitHub Actions step inside an act job, like the Actions UI)
{
  "job": "Analyze, test and build Web", // act `job` name (without workflow prefix)
  "job_id": "web",
  "name": "Test",                     // act `step`; includes "Set up job" / "Complete job"
  "status": "running" | "success" | "failure" | "skipped" | "timeout" | "cancelled",
  "duration_ms": 1234 | null
}
```

## Endpoints

| Method | Path | Result |
|---|---|---|
| GET | `/api/health` | `{"ok": true, "version": "0.1.0"}` (no auth needed) |
| GET | `/api/projects` | `[Project]` in config order |
| GET | `/api/projects/{name}` | `Project` |
| GET | `/api/projects/{name}/runs?limit=20` | `[Run]`, newest first (default 20, max 200) |
| GET | `/api/runs/{id}` | `Run` |
| GET | `/api/runs/{id}/steps/{index}/log?tail=200` | `text/plain` step output (index into `steps`); `tail` = last N lines, omitted = whole log |
| GET | `/api/ws` | Real-time WebSocket stream (see [WebSocket protocol](#websocket-protocol-get-apiws) below). |
| POST | `/api/github/webhook` | GitHub webhook (no bearer auth; verified with `X-Hub-Signature-256` HMAC using `[github] webhook_secret`/`webhook_secret_file` or `[github.app] webhook_secret`/`webhook_secret_file`; reads `X-GitHub-Event` & `X-GitHub-Delivery`). Repo matching is case-insensitive. Supports `ping` (`200 {"ok": true, "pong": true, "zen": "..."}`), `check_run`/`check_suite` (`action: "rerequested"` → forced rerun), `pull_request` (`opened`/`synchronize`/`reopened` → CI-only PR build when `watch_prs = true`), and `push` (triggers branch/tag build when `ref` matches `refs/heads/<branch>`, `deploy_refs`, or `watch_tags`). Projects with `trigger_mode = "poll"` ignore `push`/`pull_request` webhooks with `202 {"queued": false, "reason": "poll-only mode"}`. 404 if no webhook secret is configured. |
| GET | `/api/config` | `Config` (below). Secrets are replaced by `"********"` (empty string stays `""`). |
| PUT | `/api/config` | body `Config`. `"********"` in a secret field = keep current value. Validated with the same rules as `lunacy check`; on error 400 `{"error": "...", "field": "project[1].ci[0].run" \| null}`. On success: atomic write of config.toml (previous copy kept as `config.toml.bak`), hot reload (idle workers restart with new config; running pipelines finish on the old one), broadcasts WebSocket `config_updated` + `projects_updated`, 200 `Config` (masked). |
| GET | `/api/system` | `{"os": "linux", "version": "0.1.0", "config_path": "...", "state_dir": "...", "act": {"path": "..." \| null, "version": "0.2.97" \| null}, "docker": {"ok": true, "error": null}, "git": {"version": "..." \| null}, "webhook": {"configured": true, "endpoint": "...", "deliveries": 0, "last_event": null, "last_repo": null, "last_delivery_id": null, "last_received_at": null, "last_outcome": null}}` for the settings/diagnostics page |
| GET | `/api/projects/{name}/workflows` | `[{"file": ".github/workflows/ci.yml", "name": "CI", "jobs": [{"id": "web", "name": "Analyze...", "runs_on": ["ubuntu-latest"]}]}]` parsed from the repo's current checkout, for act step pickers. Also accepts `?repo_path=...` instead of a configured project (for the "new project" form). |
| POST | `/api/projects/{name}/trigger` | body `{"sha": null, "force": false, "deploy_only": false, "force_deploy": false, "ref": null, "pr_number": null}` (all fields optional) → 202 `{"queued": true}`; `sha` null = current upstream |
| POST | `/api/projects/{name}/deploy` | Force deploy a project. Optional body `{"sha": null, "skip_ci": true}` → 202 `{"queued": true, "sha": "...", "deploy_only": true}`. Skips CI when no active deploy step references `LUNACY_CI_DIR`, else runs only active `act` CI step(s) to populate `LUNACY_CI_DIR`. 400 if project has no deploy steps, 409 if running, 404 if unknown. |
| POST | `/api/runs/{id}/deploy` | Force deploy an existing run's exact SHA and `git_ref`. Optional body `{"skip_ci": true, "allow_pr": false}` → 202 `{"queued": true, "sha": "...", "deploy_only": true, "run_id": "...", "project": "..."}`. Rejects PR runs with 400 unless `allow_pr: true`; 400 if no deploy steps; 409 if project running; 404 if unknown. |
| POST | `/api/projects/{name}/cancel` | Cancel the active pipeline for project `{name}` → 202 `{"cancelling": true, "cancelled": false, "orphaned": false, "run_id": "...", "project": "..."}`. If no live worker is running, also reconciles any orphaned persisted `running` runs for `{name}` to `cancelled` → 202 `{"cancelling": true, "cancelled": true, "orphaned": true, "run_id": "...", "project": "..."}` and broadcasts WebSocket updates. 409 if no active or orphaned running runs exist, 404 if project not found. |
| POST | `/api/runs/{id}/cancel` | Cancel an active run by id → 202 `{"cancelling": true, "cancelled": false, "orphaned": false, "run_id": "...", "project": "..."}`. If not active in memory but persisted on disk as `running` (dead/orphaned), reconciles it to `cancelled` (finalizing running steps/substeps and appending a log note) → 202 `{"cancelling": true, "cancelled": true, "orphaned": true, "run_id": "...", "project": "..."}` and broadcasts WebSocket updates. 409 if already final, 404 if not found. |
| POST | `/api/cache/clear` | Clear all disk caches under `act.cache_dir` (default `<state_dir>/cache`) and best-effort remove `lunacy-cache-*` Docker volumes → 200 `{"cleared": true, "removed": [...]}` |
| POST | `/api/projects/{name}/cache/clear` | Clear a single project's disk cache (`<cache_dir>/projects/<slug>`) and best-effort remove `lunacy-cache-<slug>-*` Docker volumes → 200 `{"cleared": true, "project": "...", "removed": [...]}`; 404 if unknown project |
| GET | `/api/deploy/checks` | Local deploy-readiness diagnostics across all configured projects (git state, worktree cleanliness, reporting sources, active steps, script/act/workflow checks, timeouts, cache config `cache_enabled`/`cache_dir`, per-project `trigger_mode`/`watch_prs`/`deploy_refs`/`cache_enabled`, and last deploy summary). Fast and offline (no `git fetch` or network calls). |

`Config` is the JSON form of config.toml (same field names and nesting, serde), e.g.
`{"general": {"poll_interval": 300, "trigger_mode"?: "both"|"webhook"|"poll", "step_timeout"?: 3600, ...}, "server": {...}, "github": {"mode": "checks", "token": "********", "webhook_secret"?: "********", "app": {...}}, "discord": {...}, "act": {"cache": true, "cache_dir"?: "...", ...}, "project": [{"name": ..., "trigger_mode"?: "both"|"webhook"|"poll", "step_timeout"?: 3600, "timeout"?: 7200, "watch_prs"?: false, "deploy_refs"?: ["master", "tags/v*"], "watch_tags"?: false, "cache"?: true, "ci": [Step config], "deploy": [...]}]}`.
Trigger modes: `trigger_mode` defaults to `"both"` (periodic polling plus webhooks). `"webhook"` disables periodic git/PR/tag polling for that project and allows `poll_interval = 0`; webhooks still trigger immediate `git fetch` + pipeline execution. `"poll"` disables `push`/`pull_request` webhook triggers while keeping polling active.
Timeouts: `general.step_timeout` (default `3600` seconds) sets the daemon-wide step timeout fallback; `project.step_timeout` overrides it per project; `step.timeout` overrides both for that step; `project.timeout` enforces a total pipeline deadline across all steps. Any explicit timeout must be `>= 1`.
Step config: `{"name", "kind": "shell"|"act", "run"?, "timeout"?, "cwd"?, "shell"?, "env"?, "only_on"?: ["linux"|"windows"|"macos"]}`; act steps also take
`{"workflow"?: ".github/workflows/ci.yml", "job"?: "web", "event": "push", "platforms"?: {"ubuntu-latest": "catthehacker/ubuntu:act-latest"}, "args"?: ["--pull=false"], "secrets"?: {"K": "V"}, "vars"?: {...}}`.
`only_on` is absent when the step runs everywhere; a step whose `only_on` excludes the
host OS is recorded with `status = "skipped"` (not a failure) and shows up as skipped
in run history, the GitHub summary and Discord.
Secret fields (masked): `github.token`, `github.webhook_secret`, `github.app.private_key`, `github.app.webhook_secret`, `discord.webhook_url`, `server.token`, every project `discord_webhook_url`, every act `secrets` value.

A secret may instead come from a file: `github.token_file`, `github.webhook_secret_file`, `github.app.webhook_secret_file`,
`discord.webhook_url_file`, `server.token_file`. The file is read (trimmed) at load; setting both
the inline value and its `_file` is a validation error naming the `_file` field, and a missing or
empty file is one too. The `*_file` paths are plain (non-secret) strings in this JSON — the file
contents never are. `github.app.private_key_path` works the same way.

`[act] lock_file` (default `<state_dir>/act.lock`, `~/` expanded) is the `flock` every act run
takes; point it at the file a manual `make ci` locks to keep the two apart. Every step of a run
whose first act step created a clone gets its path as `LUNACY_CI_DIR` in the environment (not in
this JSON).
Comments in config.toml are preserved where possible (toml_edit); new sections are appended.

Run history is persisted under `state_dir/runs/<project>/<id>.json` so it survives restarts.
On daemon startup (`Daemon::new`), any persisted run still marked `running` is reconciled to `cancelled` (finalizing running steps/substeps and appending a cancellation note to existing step logs).

## WebSocket protocol (`GET /api/ws`)

Real-time updates for projects, runs, live step logs, config reloads, and incoming webhooks (max 64 concurrent connections; periodic server ping every 25 s).

### Authentication
- When `server.token` is empty, connections are authenticated immediately on upgrade.
- Native clients may pass `Authorization: Bearer <token>` on the upgrade request (an invalid header returns HTTP `401` before upgrading).
- Browser clients (which cannot set custom headers on `WebSocket`) upgrade without `Authorization` and must send `{"type": "auth", "token": "<token>"}` as the first text frame within 5 seconds. Invalid, unexpected, or timed-out auth frames receive `{"type": "auth_error", "error": "unauthorized"}` and close code `4001`.
- If `server.token` is added or rotated via `PUT /api/config`, existing WebSocket connections whose token no longer matches receive `auth_error` and close code `4001`.
- Once authenticated, the server immediately sends `auth_ok` followed by `snapshot`.

### Client → Server frames
- `{"type": "auth", "token": "..."}`
- `{"type": "subscribe_run", "run_id": "..."}` — subscribes to live `run_updated` events for `run_id` and immediately sends `run_updated` if found, or `run_not_found` otherwise.
- `{"type": "unsubscribe_run"}`
- `{"type": "subscribe_log", "run_id": "...", "step": 0, "tail": 400}` (`tail` optional; full snapshots are capped to the trailing 8 MiB) — sends `log_snapshot` and streams live `log_chunk` frames for `(run_id, step)`.
- `{"type": "unsubscribe_log"}`
- `{"type": "ping"}` — replies `{"type": "pong"}`.

### Server → Client frames
- `{"type": "auth_ok", "version": "0.1.0"}`
- `{"type": "auth_error", "error": "unauthorized"}`
- `{"type": "snapshot", "version": "0.1.0", "projects": [Project]}`
- `{"type": "projects_updated", "projects": [Project]}`
- `{"type": "run_updated", "run": Run}` (only for subscribed `run_id`)
- `{"type": "run_not_found", "run_id": "..."}` (sent when `subscribe_run` targets an unknown or invalid `run_id`)
- `{"type": "log_snapshot", "run_id": "...", "step": 0, "tail": 400, "text": "...", "offset": 1234}`
- `{"type": "log_chunk", "run_id": "...", "step": 0, "chunk": "...", "offset": 1245}` (only for subscribed `(run_id, step)`)
- `{"type": "config_updated"}`
- `{"type": "webhook_received", "event": "push", "repo": "owner/name", "delivery_id": "abc-123", "outcome": "queued master for demo", "received_at": "2026-11-26T08:00:00Z", "queued": true}`
- `{"type": "pong"}`
