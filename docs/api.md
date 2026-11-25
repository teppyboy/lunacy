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
  "last_sha": "abc...",                // last processed sha, null if none
  "running": false,
  "current_run": Run | null,           // the running pipeline, if any
  "last_run": Run | null               // most recent finished run
}

// Run
{
  "id": "lunarum-1764067200-abc1234",  // unique, URL-safe
  "project": "lunarum",
  "sha": "abc...",
  "subject": "feat: something",        // commit subject, may be ""
  "status": "running" | "success" | "failure" | "error",
  "started_at": 1764067200,
  "finished_at": 1764067290,           // null while running
  "failed_step": "Test" | null,
  "github_url": "https://github.com/o/r/runs/123" | null, // GitHub check run (CI phase) page, else null
  "steps": [Step]
}

// Step
{
  "phase": "ci" | "deploy",
  "name": "Test",
  "status": "pending" | "running" | "success" | "failure" | "timeout" | "skipped",
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
  "status": "running" | "success" | "failure" | "skipped",
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
| POST | `/api/github/webhook` | GitHub App webhook (no bearer auth; verified with `X-Hub-Signature-256` HMAC using `[github.app] webhook_secret`). `check_run`/`check_suite` action `rerequested` → forced rerun of that sha. 404 if no webhook_secret. |
| GET | `/api/config` | `Config` (below). Secrets are replaced by `"********"` (empty string stays `""`). |
| PUT | `/api/config` | body `Config`. `"********"` in a secret field = keep current value. Validated with the same rules as `lunacy check`; on error 400 `{"error": "...", "field": "project[1].ci[0].run" \| null}`. On success: atomic write of config.toml (previous copy kept as `config.toml.bak`), hot reload (idle workers restart with new config; running pipelines finish on the old one), 200 `Config` (masked). |
| GET | `/api/system` | `{"os": "linux", "version": "0.1.0", "config_path": "...", "act": {"path": "..." \| null, "version": "0.2.97" \| null}, "docker": {"ok": true, "error": null}, "git": {"version": "..." \| null}}` for the settings/diagnostics page |
| GET | `/api/projects/{name}/workflows` | `[{"file": ".github/workflows/ci.yml", "name": "CI", "jobs": [{"id": "web", "name": "Analyze...", "runs_on": ["ubuntu-latest"]}]}]` parsed from the repo's current checkout, for act step pickers. Also accepts `?repo_path=...` instead of a configured project (for the "new project" form). |
| POST | `/api/projects/{name}/trigger` | body `{"sha": null, "force": false}` → 202 `{"queued": true}`; `sha` null = current upstream |

`Config` is the JSON form of config.toml (same field names and nesting, serde), e.g.
`{"general": {...}, "server": {...}, "github": {"mode": "checks", "token": "********", "app": {...}}, "discord": {...}, "project": [{"name": ..., "ci": [Step config], "deploy": [...]}]}`.
Step config: `{"name", "kind": "shell"|"act", "run"?, "timeout", "cwd"?, "shell"?, "env"?, "only_on"?: ["linux"|"windows"|"macos"]}`; act steps also take
`{"workflow"?: ".github/workflows/ci.yml", "job"?: "web", "event": "push", "platforms"?: {"ubuntu-latest": "catthehacker/ubuntu:act-latest"}, "args"?: ["--pull=false"], "secrets"?: {"K": "V"}, "vars"?: {...}}`.
`only_on` is absent when the step runs everywhere; a step whose `only_on` excludes the
host OS is recorded with `status = "skipped"` (not a failure) and shows up as skipped
in run history, the GitHub summary and Discord.
Secret fields (masked): `github.token`, `github.app.private_key`, `github.app.webhook_secret`, `discord.webhook_url`, `server.token`, every project `discord_webhook_url`, every act `secrets` value.

A secret may instead come from a file: `github.token_file`, `github.app.webhook_secret_file`,
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
The UI polls (`/api/projects` every ~2 s, open run every ~1 s while running); no websockets in v1.
